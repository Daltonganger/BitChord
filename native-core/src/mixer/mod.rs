//! Mixer (spec §3.1): cpal output + gapless/crossfade mixing.
//!
//! Foundation decision (milestone-4 spike, spec §3.4): **hand-rolled blending,
//! not oddio.** oddio's last release was 2023-10; the scoped fallback needed no
//! downstream changes, and per-voice sample blending in the render loop gives
//! exact control over the equal-power curve and the per-stream DSP stages the
//! spec requires ahead of the blend. Two voices, roles swapping at every
//! transition — the same topology as upstream's active/standby ExoPlayer pair.
//!
//! The transition execution loop is the Rust port of upstream
//! `CrossfadeController`'s decisions: equal-power sin/cos gain pair, 30 ms
//! fade cadence (rendered per chunk here, finer than upstream's tick), the
//! four `TransitionStyle` filter rides with upstream's verbatim constants,
//! arm-lead/timeout, and the 120 ms bail ramp.

mod bridge;
mod commands;
mod curves;
mod deck;
mod fx;
mod render;
mod state;
mod swap_alignment;
mod transition;
mod types;
mod voice;

#[cfg(test)]
mod tests;

pub(crate) use curves::MAX_DUCK_DEPTH;
pub use deck::{
    loudness_gain, DeckLoudness, MAX_LOUDNESS_GAIN_DB, MAX_TRANSITION_BOOST_DB, MAX_TRANSITION_MATCH_DB,
    MIN_LOUDNESS_GAIN_DB,
};
pub use state::NerdSnapshot;
pub use types::{
    Command, EngineEvents, QueuedTrack, TrackInfo, TrackSource, TransitionPlan, TransitionStyle,
};

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Receiver;
use rtrb::Producer;

use crate::eq::EqualizerProcessor;
use crate::sound::{Clarity, ClarityTuning, LoudnessMode, Protector, SoundMode};

use commands::handle_command;
use render::render_available;
use state::{MixerState, Phase, SwapProbe};
use voice::Voice;

const AUTOMIX_SAFETY_PREFIX: &str = "safety:";

fn automix_plan_winner(plan: &TransitionPlan) -> Option<&'static str> {
    if plan.reason.starts_with("skip-mix:") {
        None
    } else if plan.reason.starts_with(AUTOMIX_SAFETY_PREFIX) {
        Some("safety")
    } else if plan.fade_seconds > 0.0 || !plan.reason.is_empty() {
        Some("analyzed")
    } else {
        None
    }
}

pub(crate) struct AutomixWinnerRecord<'a> {
    pub(crate) winner: &'static str,
    pub(crate) style: &'static str,
    pub(crate) reason: &'a str,
    pub(crate) snap_error_seconds: Option<f64>,
}

pub(crate) fn automix_winner_record(
    plan: &TransitionPlan,
    snap_error_seconds: Option<f64>,
) -> Option<AutomixWinnerRecord<'_>> {
    Some(AutomixWinnerRecord {
        winner: automix_plan_winner(plan)?,
        style: plan.style.telemetry_name(),
        reason: plan
            .reason
            .strip_prefix(AUTOMIX_SAFETY_PREFIX)
            .unwrap_or(&plan.reason)
            .trim(),
        snap_error_seconds,
    })
}

fn accepts_analyzed_plan(phase: Phase, swap: bool) -> bool {
    phase == Phase::Arming && !swap
}

/// How long a technique's window is at `beat_seconds`, for the planner.
pub fn technique_window_seconds(style: TransitionStyle, beat_seconds: f64) -> Option<f64> {
    fx::window_seconds(style, beat_seconds)
}

/// The phrase-aware technique window used during AutoMix planning.
pub fn technique_window_seconds_for_phrase(
    style: TransitionStyle,
    beat_seconds: f64,
    phrase_bars: u32,
) -> Option<f64> {
    fx::window_seconds_for_phrase(style, beat_seconds, phrase_bars)
}

const BAIL_MS: u64 = 120;
const ARM_LEAD_MS: u64 = 4_000;
const ARM_TIMEOUT_MS: u64 = 12_000;
/// Leave this much of the incoming track after a cue so a mix-in cannot
/// land in the outro (upstream `MIN_INCOMING_CLEARANCE_SECONDS`).
const MIN_INCOMING_CLEARANCE_S: f64 = 5.0;
/// Longest mix-in the mixer will open a track at, in seconds and as a fraction
/// of its duration. The planner owns the decision (`analyzer::plan`) and adds a
/// beat-based bound on top; these are the two the mixer can restate, so a plan
/// that somehow arrives too deep is caught before it reaches `seek_seconds`.
///
/// The fraction is a floor for short material, not a ceiling for long: 6 % of a
/// one-minute clip is under four seconds, and an eight-second skip there is an
/// eighth of the record.
const MAX_CUE_SECONDS: f64 = 8.0;
const MAX_CUE_FRACTION: f64 = 0.06;
/// Upstream `swapCurrentToVersion`'s `swapCrossfadeMs` — the equal-power
/// crossfade a same-track source swap (quality upgrade, alternate version)
/// runs over. Long enough to hide a decoder swap, short enough that a
/// same-timeline blend never reads as an edit.
const SWAP_CROSSFADE_MS: f64 = 550.0;
/// How much of an armed incoming to decode before the blend, so its decoder,
/// resampler and handoff stretch are already primed when its first sample is
/// due. The arm happens seconds ahead of the fade; doing the cold decode here
/// moves that cost off the audible path.
const NORMAL_PREROLL_SECONDS: f64 = 0.5;

/// Prime an armed incoming's decoder and buffer without emitting a sample.
fn prime_voice(voice: &mut Voice, device_rate: u32) {
    let frames = (NORMAL_PREROLL_SECONDS * device_rate as f64)
        .round()
        .max(1.0) as usize;
    voice.prime_for_fade(frames, device_rate);
}

/// The mixer thread entry: owns voices + the ring producer.
#[allow(clippy::too_many_arguments)]
pub fn run_mixer(
    commands: Receiver<Command>,
    mut ring: Producer<f32>,
    buffered_frames: Arc<AtomicU64>,
    position_ms: Arc<AtomicU64>,
    duration_ms: Arc<AtomicU64>,
    device_rate: u32,
    events: Arc<dyn EngineEvents>,
    shutdown: Arc<AtomicBool>,
    flush_ring: Arc<AtomicBool>,
    bail_flush: Arc<AtomicBool>,
    nerd: Arc<Mutex<NerdSnapshot>>,
) {
    let mut state = MixerState {
        current: None,
        incoming: None,
        pending_next: None,
        warned_no_duration: false,
        transition: None,
        retiring: Vec::new(),
        render_scratch: Vec::new(),
        voice_output_scratch: Vec::new(),
        effect_dry_scratch: Vec::new(),
        retiring_scratch: Vec::new(),
        playing: false,
        bit_perfect: false,
        volume: 1.0,
        sleep_gain: 1.0,
        hold_end: false,
        crossfade_window_s: 0.0,
        spatial_enabled: false,
        head_yaw: 0.0,
        device_rate,
        buffered_frames,
        position_ms,
        duration_ms,
        events,
        state: crate::PlaybackState::Stopped,
        flush_ring,
        bail_flush,
        playback_speed: 1.0,
        skip_silence: false,
        // The mixer blends to interleaved stereo, so the equaliser is a single
        // stereo instance on the mixed stream.
        eq: EqualizerProcessor::new(device_rate, 2),
        sound_mode: SoundMode::Transparent,
        clarity_tuning: ClarityTuning::default(),
        clarity: Clarity::new(device_rate),
        protector: Protector::new(device_rate),
        pipeline_tail_frames: 0,
        output_generation: 0,
        queue_target_ms: 250,
        underrun_counter: None,
        observed_underruns: 0,
        nerd,
        last_diagnostic_signature: None,
        last_diagnostic_publish: None,
        // The low-level engine starts neutral; the app restores its policy.
        loudness_enabled: false,
        loudness_mode: LoudnessMode::Off,
        swap_probe: SwapProbe::default(),
    };

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }

        // Drain commands.
        while let Ok(cmd) = commands.try_recv() {
            handle_command(&mut state, cmd, &mut ring);
        }

        if let Some(counter) = &state.underrun_counter {
            let count = counter.load(Ordering::Relaxed);
            if count > state.observed_underruns {
                state.queue_target_ms = (state.queue_target_ms + 60).min(1000);
                crate::diagnostics::technical_event(format!(
                    "output underruns={} queue_target_ms={}",
                    count, state.queue_target_ms
                ));
                state.observed_underruns = count;
            }
        }
        if state.playing {
            render_available(&mut state, &mut ring);
            if state.flush_ring.load(Ordering::Acquire)
                || state.bail_flush.load(Ordering::Acquire)
                || state.buffered_frames.load(Ordering::Relaxed)
                    >= state.device_rate as u64 * state.queue_target_ms / 1000
            {
                std::thread::sleep(Duration::from_millis(4));
            }
        } else {
            // Idle: still service commands promptly, low CPU.
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
