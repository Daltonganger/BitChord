use super::curves::{
    automix_gains, cold_end_gains, db_to_gain, effect_intro_db, effect_intro_gains, effect_landing,
    fade_gains, fall_gain, hold_fraction, ride_effect_intro, ride_filters, rise_gain,
    COLD_END_UNDERLAY_DB,
};
use super::deck::{
    bound_mix_in_depth, clamp_start_seconds, glided_plan_rate, is_silent, known_duration,
    silence_exceeds_floor,
};
use super::state::{MixerState, Phase, SwapProbe, TransitionState};
use super::voice::Voice;
use super::*;
use crate::transition_filter::TransitionFilter;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

mod automix;
mod blends;
mod cues;
mod handoff;
mod levels;
mod regions;
mod swaps;
use std::time::Instant;

/// A tone at `amplitude` on both channels: the material every level test
/// here measures. Two of them at different frequencies add as power, which
/// is what an equal-power crossfade assumes.
fn tone(freq: f64, amplitude: f32, frames: usize) -> Vec<f32> {
    (0..frames * 2)
        .map(|i| {
            let t = (i / 2) as f64 / 48_000.0;
            amplitude * (2.0 * std::f64::consts::PI * freq * t).sin() as f32
        })
        .collect()
}

/// 100 ms windows of mean power, in dB — the level of the mix as it moves.
fn window_levels(samples: &[f32], window_frames: usize) -> Vec<f64> {
    samples
        .chunks(window_frames * 2)
        .filter(|chunk| chunk.len() == window_frames * 2)
        .map(|chunk| {
            let power: f64 =
                chunk.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / chunk.len() as f64;
            10.0 * power.max(1e-12).log10()
        })
        .collect()
}

fn deck(lufs: Option<f64>, relative: Option<f64>, applied: Option<f64>) -> DeckLoudness {
    DeckLoudness {
        lufs,
        relative_db: relative,
        applied_normalization_db: applied,
        true_peak_dbtp: None,
    }
}

/// A stereo 16-bit sine at `amplitude`, for the level tests.
fn level_wav(path: &std::path::Path, seconds: f32, freq: f32, amplitude: f32) {
    let rate = 44_100u32;
    let frames = (rate as f32 * seconds) as usize;
    let data_len = frames * 4;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
    for i in 0..frames {
        let t = i as f32 / rate as f32;
        let sample = (2.0 * std::f32::consts::PI * freq * t).sin() * amplitude * i16::MAX as f32;
        let value = sample as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

fn transition_source(path: &std::path::Path) -> TrackSource {
    TrackSource {
        source: path.display().to_string(),
        title: "level".into(),
        artist: String::new(),
        start_seconds: 0.0,
        plan: TransitionPlan::default(),
        headers: std::collections::HashMap::new(),
        claimed_kbps: 0,
        loudness_db: None,
        duration_seconds: 0.0,
    }
}

fn open_voice(path: &std::path::Path) -> Voice {
    Voice::open(
        &transition_source(path),
        false,
        0.0,
        48_000,
        1.0,
        false,
        false,
    )
    .unwrap()
}

fn measured(lufs: f64) -> crate::LoudnessMeasurement {
    crate::LoudnessMeasurement {
        track_lufs: lufs,
        album_lufs: None,
        true_peak_dbtp: None,
    }
}

/// The engine's own blend: both decks pulled and mixed through `mix_into`
/// with the equal-power curves, so the arming, the smoothing, the
/// normalization stage and the release are all the shipped ones.
fn render_blend(outgoing: &mut Voice, incoming: &mut Voice, normalization: bool) -> Vec<f32> {
    let total = 48_000 * 2;
    let mut mixed = Vec::with_capacity(total * 2);
    let mut done = 0usize;
    while done < total {
        let frames = 1024.min(total - done);
        let a = outgoing.pull(frames, 48_000);
        let b = incoming.pull(frames, 48_000);
        let mut out = vec![0.0f32; frames * 2];
        let progress = done as f64 / total as f64;
        outgoing.mix_into(&a, &mut out, fall_gain(progress), normalization);
        incoming.mix_into(&b, &mut out, rise_gain(progress), normalization);
        mixed.extend_from_slice(&out);
        done += frames;
    }
    mixed
}

/// Deterministic white noise, so the effects add uncorrelated power instead
/// of cancelling a pure tone at one delay phase. `lead_seconds` of digital
/// silence at the head is the quiet lead-in the cue trim must skip.
fn noise_wav(path: &std::path::Path, lead_seconds: f32, seconds: f32, amplitude: f32, seed: u32) {
    let rate = 44_100u32;
    let frames = (rate as f32 * seconds) as usize;
    let lead_frames = (rate as f32 * lead_seconds) as usize;
    let data_len = frames * 4;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
    let mut state: u32 = seed.max(1);
    for i in 0..frames {
        let sample = if i < lead_frames {
            0.0
        } else {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let unit = (state >> 8) as f32 / 16_777_216.0;
            (unit * 2.0 - 1.0) * amplitude * i16::MAX as f32
        };
        let value = sample as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

/// A beat-aligned kick pattern (55 Hz, one per beat) plus independent
/// noise: two of these with different seeds share their kicks and not their
/// noise, which is the correlated case a real beat-matched DjBlend has.
fn kick_wav(path: &std::path::Path, seconds: f32, seed: u32, kick: f32, noise: f32) {
    let rate = 44_100u32;
    let frames = (rate as f32 * seconds) as usize;
    let data_len = frames * 4;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
    let mut state: u32 = seed.max(1);
    for i in 0..frames {
        let t = i as f32 / rate as f32;
        let phase = t % 0.5;
        let envelope = (-phase * 28.0).exp();
        let body = (2.0 * std::f32::consts::PI * 55.0 * phase).sin() * envelope * kick;
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let unit = (state >> 8) as f32 / 16_777_216.0;
        let sample = (body + (unit * 2.0 - 1.0) * noise) * i16::MAX as f32;
        let value = sample as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

fn rms_window(samples: &[f32], start_frame: usize, frames: usize) -> f64 {
    let start = (start_frame * 2).min(samples.len());
    let end = ((start_frame + frames) * 2).min(samples.len());
    if end <= start {
        return 0.0;
    }
    let sum: f64 = samples[start..end]
        .iter()
        .map(|s| (*s as f64).powi(2))
        .sum();
    (sum / ((end - start) / 2).max(1) as f64).sqrt()
}

struct RenderedTransition {
    mixed: Vec<f32>,
    outgoing: Vec<f32>,
    incoming: Vec<f32>,
    fade_frames: usize,
    /// The technique's own landing, or the blend's midpoint.
    land_frame: usize,
}

/// Render one transition the way `render_available` does: the outgoing
/// deck through the blend fall (and its effect in the last sub-window), the
/// pre-rolled incoming through the blend rise, both served in 512-frame
/// chunks. The arm-time prime is the production one.
fn render_transition_style(
    style: TransitionStyle,
    outgoing_path: &std::path::Path,
    incoming_path: &std::path::Path,
    hold_seconds: f64,
) -> RenderedTransition {
    const RATE: u32 = 48_000;
    const BLOCK: usize = 512;
    const BEAT: f64 = 0.5;
    const FADE_SECONDS: f64 = 8.0;
    let fade_frames = (FADE_SECONDS * RATE as f64) as usize;
    let technique_window = fx::window_seconds(style, BEAT).unwrap_or(0.0);
    let plan = TransitionPlan {
        style,
        beat_seconds: BEAT,
        fade_seconds: FADE_SECONDS,
        outgoing_hold_seconds: hold_seconds,
        effect_seconds: technique_window,
        effect_start_seconds: FADE_SECONDS - technique_window,
        transition_end_seconds: FADE_SECONDS,
        bed_fraction: if matches!(style, TransitionStyle::DjBlend | TransitionStyle::DjFilter) {
            0.45
        } else {
            0.0
        },
        bed_gain_db: -14.0,
        dip_depth: 0.2,
        dip_width: 0.1,
        ..Default::default()
    };
    let mut outgoing = open_voice(outgoing_path);
    let mut incoming = open_voice(incoming_path);
    incoming.gain = 0.0;
    incoming.applied_gain = 0.0;
    prime_voice(&mut incoming, RATE);
    let mut fx = if style.is_effect() {
        fx::DeckFx::new(style, RATE, BEAT)
    } else {
        None
    };
    let effect_start_frame = ((FADE_SECONDS - technique_window) * RATE as f64) as u64;
    let mut mixed = Vec::with_capacity(fade_frames * 2);
    let mut out_raw = Vec::with_capacity(fade_frames * 2);
    let mut in_raw = Vec::with_capacity(fade_frames * 2);
    let mut emitted_in = 0u64;
    let mut done = 0usize;
    let hold = hold_fraction(&plan, FADE_SECONDS);
    while done < fade_frames {
        let p = done as f64 / fade_frames as f64;
        let (rise, fall) = if style.is_effect() {
            effect_intro_gains(p, hold, effect_intro_db(&plan))
        } else if style == TransitionStyle::EqualPower {
            if hold > 0.0 {
                cold_end_gains(p, hold)
            } else {
                fade_gains(p, false)
            }
        } else {
            automix_gains(p, &plan, hold)
        };
        let a = outgoing.pull(BLOCK, RATE);
        let mut out = vec![0.0f32; BLOCK * 2];
        if fx.is_some() && emitted_in >= effect_start_frame {
            let mut dry = vec![0.0f32; BLOCK * 2];
            outgoing.mix_into(&a, &mut dry, fall, false);
            fx.as_mut().unwrap().render(&dry, &mut out, BLOCK);
        } else {
            outgoing.mix_into(&a, &mut out, fall, false);
        }
        let b = incoming.pull(BLOCK, RATE);
        emitted_in += (b.len() / 2) as u64;
        incoming.mix_into(&b, &mut out, rise, false);
        let mut a_pad = a;
        a_pad.resize(BLOCK * 2, 0.0);
        let mut b_pad = b;
        b_pad.resize(BLOCK * 2, 0.0);
        out_raw.extend_from_slice(&a_pad);
        in_raw.extend_from_slice(&b_pad);
        mixed.extend_from_slice(&out);
        done += BLOCK;
    }
    let land_frame = match &fx {
        Some(effect) => effect_start_frame as usize + effect.land_frame() as usize,
        None => fade_frames / 2,
    };
    RenderedTransition {
        mixed,
        outgoing: out_raw,
        incoming: in_raw,
        fade_frames,
        land_frame,
    }
}

/// How far the level moved from the first 100 ms window, and the peak.
fn level_spread(samples: &[f32]) -> (f64, f32) {
    let levels = window_levels(samples, 4_800);
    let first = levels.first().copied().unwrap_or(0.0);
    let spread = levels
        .iter()
        .map(|level| (level - first).abs())
        .fold(0.0f64, f64::max);
    let peak = samples.iter().fold(0f32, |peak, s| peak.max(s.abs()));
    (spread, peak)
}

fn levels() -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("bitchord-mixer-levels-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let loud = dir.join("loud.wav");
    let quiet = dir.join("quiet.wav");
    // Six dB apart, different frequencies so their powers add rather than
    // their waveforms — which is what an equal-power crossfade assumes.
    // Long enough to hold a two-second blend and then the release, which
    // is slower than the arming on purpose.
    level_wav(&loud, 8.0, 1_000.0, 0.5);
    level_wav(&quiet, 8.0, 1_500.0, 0.25);
    (loud, quiet)
}

#[derive(Default)]
struct RecordedEvents {
    handoffs: Mutex<Vec<String>>,
    ended: Mutex<Vec<&'static str>>,
    states: Mutex<Vec<&'static str>>,
    errors: Mutex<Vec<String>>,
    position_ms: Option<Arc<AtomicU64>>,
    position_at_handoff: Mutex<Vec<u64>>,
}

impl EngineEvents for RecordedEvents {
    fn state_changed(&self, state: crate::PlaybackState) {
        self.states.lock().unwrap().push(match state {
            crate::PlaybackState::Stopped => "stopped",
            crate::PlaybackState::Buffering => "buffering",
            crate::PlaybackState::Playing => "playing",
            crate::PlaybackState::Paused => "paused",
        });
    }
    fn track_ended(&self, reason: crate::TrackEndReason, _source: String) {
        self.ended.lock().unwrap().push(match reason {
            crate::TrackEndReason::Natural => "natural",
            crate::TrackEndReason::Skipped => "skipped",
            crate::TrackEndReason::Error => "error",
        });
    }
    fn error(&self, message: String) {
        self.errors.lock().unwrap().push(message);
    }
    fn handoff(&self, info: TrackInfo) {
        if let Some(position) = &self.position_ms {
            self.position_at_handoff
                .lock()
                .unwrap()
                .push(position.load(Ordering::Relaxed));
        }
        self.handoffs.lock().unwrap().push(info.title);
    }
    fn duration_changed(&self, _seconds: f64) {}
}

fn test_wav(path: &std::path::Path, seconds: f32, freq: f32) {
    // Minimal 44.1 kHz stereo 16-bit PCM WAV.
    let rate = 44_100u32;
    let frames = (rate as f32 * seconds) as usize;
    let data_len = frames * 4;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
    for i in 0..frames {
        let t = i as f32 / rate as f32;
        let v = (0.3 * (2.0 * std::f32::consts::PI * freq * t).sin() * 32767.0) as i16;
        bytes.extend_from_slice(&v.to_le_bytes());
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

fn armed_test_state(
    outgoing: &std::path::Path,
    incoming: &std::path::Path,
    phase: Phase,
) -> MixerState {
    let source = |path: &std::path::Path, title: &str| TrackSource {
        source: path.display().to_string(),
        title: title.into(),
        artist: String::new(),
        start_seconds: 0.0,
        plan: TransitionPlan::default(),
        headers: std::collections::HashMap::new(),
        claimed_kbps: 0,
        loudness_db: None,
        duration_seconds: 12.0,
    };
    let mut current = Voice::open(
        &source(outgoing, "out"),
        false,
        0.0,
        44_100,
        1.0,
        false,
        false,
    )
    .unwrap();
    let incoming_voice = Voice::open(
        &source(incoming, "in"),
        false,
        0.0,
        44_100,
        1.0,
        false,
        false,
    )
    .unwrap();
    current.gain = 1.0;
    MixerState {
        current: Some(current),
        incoming: Some(incoming_voice),
        pending_next: None,
        warned_no_duration: false,
        transition: Some(TransitionState {
            phase,
            fade_frames: 4 * 44_100,
            handed_off: phase == Phase::Fading,
            plan: TransitionPlan {
                fade_seconds: 4.0,
                reason: "safety:".into(),
                ..TransitionPlan::default()
            },
            arm_deadline: Instant::now() + Duration::from_secs(30),
            swap: false,
            fx: None,
            effect_start_frame: 0,
            effect_input_gain: None,
        }),
        retiring: Vec::new(),
        render_scratch: Vec::new(),
        voice_output_scratch: Vec::new(),
        effect_dry_scratch: Vec::new(),
        retiring_scratch: Vec::new(),
        playing: true,
        bit_perfect: false,
        volume: 1.0,
        sleep_gain: 1.0,
        hold_end: false,
        crossfade_window_s: 4.0,
        spatial_enabled: false,
        head_yaw: 0.0,
        device_rate: 44_100,
        buffered_frames: Arc::new(AtomicU64::new(0)),
        position_ms: Arc::new(AtomicU64::new(0)),
        duration_ms: Arc::new(AtomicU64::new(0)),
        events: Arc::new(RecordedEvents::default()),
        state: crate::PlaybackState::Playing,
        flush_ring: Arc::new(AtomicBool::new(false)),
        bail_flush: Arc::new(AtomicBool::new(false)),
        playback_speed: 1.0,
        skip_silence: false,
        eq: EqualizerProcessor::new(44_100, 2),
        sound_mode: SoundMode::Transparent,
        clarity_tuning: ClarityTuning::default(),
        clarity: Clarity::new(44_100),
        protector: Protector::new(44_100),
        pipeline_tail_frames: 0,
        output_generation: 0,
        queue_target_ms: 250,
        underrun_counter: None,
        observed_underruns: 0,
        nerd: Arc::new(Mutex::new(NerdSnapshot::default())),
        last_diagnostic_signature: None,
        last_diagnostic_publish: None,
        loudness_enabled: false,
        loudness_mode: LoudnessMode::Off,
        swap_probe: SwapProbe::default(),
    }
}

fn analyzed_upgrade(incoming: &std::path::Path) -> TrackSource {
    TrackSource {
        source: incoming.display().to_string(),
        title: "in".into(),
        artist: String::new(),
        start_seconds: 1.0,
        plan: TransitionPlan {
            style: TransitionStyle::DjBlend,
            fade_seconds: 6.0,
            cue_seconds: 1.0,
            reason: "beat grid and vocal windows".into(),
            ..TransitionPlan::default()
        },
        headers: std::collections::HashMap::new(),
        claimed_kbps: 0,
        loudness_db: None,
        duration_seconds: 12.0,
    }
}

/// Clicks on one channel, silence on the other, so a mix can be split back
/// into the two records.
fn click_wav(path: &std::path::Path, seconds: f32, bpm: f64, left: bool) {
    let rate = 44_100u32;
    let frames = (rate as f32 * seconds) as usize;
    let interval = (rate as f64 * 60.0 / bpm).round() as usize;
    let burst = rate as usize * 8 / 1000;
    let data_len = frames * 4;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
    for i in 0..frames {
        let within = i % interval;
        let sample = if within < burst {
            let t = within as f32 / rate as f32;
            let env = (-t * 500.0).exp();
            (env * (2.0 * std::f32::consts::PI * 4000.0 * t).sin() * 0.8 * 32767.0) as i16
        } else {
            0
        };
        let (l, r) = if left { (sample, 0) } else { (0, sample) };
        bytes.extend_from_slice(&l.to_le_bytes());
        bytes.extend_from_slice(&r.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

fn planned_at(cue: f64) -> TransitionPlan {
    TransitionPlan {
        cue_seconds: cue,
        ..TransitionPlan::default()
    }
}

/// Drain the ring the way the device callback would (decrementing the
/// shared counter) for a fixed number of frames.
fn drain_frames(consumer: &mut rtrb::Consumer<f32>, buffered: &AtomicU64, frames: usize) {
    let mut popped = 0usize;
    let target = frames * 2;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while popped < target && std::time::Instant::now() < deadline {
        match consumer.pop() {
            Ok(_) => {
                popped += 1;
                // `render_f32` counts frames, not samples; decrementing per
                // sample would drain the shared counter twice as fast and
                // make the audible playhead look like the decoder head.
                if popped % 2 == 0 {
                    let mut current = buffered.load(Ordering::Relaxed);
                    while current > 0 {
                        match buffered.compare_exchange_weak(
                            current,
                            current - 1,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(v) => current = v,
                        }
                    }
                }
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(1)),
        }
    }
}

/// Drains until the handoff fires, returning the playhead at that moment.
///
/// The playhead is the number a caller needs: the handoff is the engine
/// saying "the incoming track is audible now", so how far into the outgoing
/// track that happens is exactly whether there was a transition or a cut.
fn drain_until_handoff(
    consumer: &mut rtrb::Consumer<f32>,
    buffered: &AtomicU64,
    position: &AtomicU64,
    events: &RecordedEvents,
    seconds: u64,
) -> Option<f64> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    loop {
        while consumer.pop().is_ok() {
            let mut current = buffered.load(Ordering::Relaxed);
            while current > 0 {
                match buffered.compare_exchange_weak(
                    current,
                    current - 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(v) => current = v,
                }
            }
        }
        if !events.handoffs.lock().unwrap().is_empty() {
            // The mixer publishes the incoming track's playhead at handoff,
            // which is its own cue rather than the outgoing tail — so this is
            // the *outgoing* position only if the handoff had not been
            // published yet. Sampled before the check on the next pass, this
            // is close enough for a margin measured in seconds.
            return Some(position.load(Ordering::Relaxed) as f64 / 1000.0);
        }
        if std::time::Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Drain `frames` and return the RMS of what came out.
fn rms_frames(consumer: &mut rtrb::Consumer<f32>, buffered: &AtomicU64, frames: usize) -> f64 {
    let mut sum = 0.0f64;
    let mut count = 0usize;
    let target = frames * 2;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while count < target && std::time::Instant::now() < deadline {
        match consumer.pop() {
            Ok(sample) => {
                sum += (sample as f64) * (sample as f64);
                count += 1;
                if count % 2 == 0 {
                    let mut current = buffered.load(Ordering::Relaxed);
                    while current > 0 {
                        match buffered.compare_exchange_weak(
                            current,
                            current - 1,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(v) => current = v,
                        }
                    }
                }
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(1)),
        }
    }
    if count == 0 {
        return 0.0;
    }
    (sum / count as f64).sqrt()
}

struct SwapHarness {
    tx: crossbeam_channel::Sender<Command>,
    consumer: rtrb::Consumer<f32>,
    buffered: std::sync::Arc<AtomicU64>,
    position: std::sync::Arc<AtomicU64>,
    events: std::sync::Arc<RecordedEvents>,
    nerd: std::sync::Arc<std::sync::Mutex<NerdSnapshot>>,
    shutdown: std::sync::Arc<AtomicBool>,
    flush_ring: std::sync::Arc<AtomicBool>,
    bail_flush: std::sync::Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SwapHarness {
    fn new(tag: &str) -> (SwapHarness, std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("bitchord-mixer-{tag}"));
        std::fs::create_dir_all(&dir).unwrap();
        let low = dir.join("low.wav");
        let high = dir.join("high.wav");
        test_wav(&low, 8.0, 440.0);
        test_wav(&high, 8.0, 440.0);

        let events = std::sync::Arc::new(RecordedEvents::default());
        let (tx, rx) = crossbeam_channel::unbounded::<Command>();
        let (producer_side, consumer) = rtrb::RingBuffer::<f32>::new(44_100 * 2 * 2);
        let buffered = std::sync::Arc::new(AtomicU64::new(0));
        let position = std::sync::Arc::new(AtomicU64::new(0));
        let duration = std::sync::Arc::new(AtomicU64::new(0));
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let flush_ring = std::sync::Arc::new(AtomicBool::new(false));
        let bail_flush = std::sync::Arc::new(AtomicBool::new(false));
        let nerd = std::sync::Arc::new(std::sync::Mutex::new(NerdSnapshot::default()));
        let handle = std::thread::spawn({
            let events = events.clone();
            let buffered = buffered.clone();
            let position = position.clone();
            let duration = duration.clone();
            let shutdown = shutdown.clone();
            let flush_ring = flush_ring.clone();
            let bail_flush = bail_flush.clone();
            let nerd = nerd.clone();
            move || {
                run_mixer(
                    rx,
                    producer_side,
                    buffered,
                    position,
                    duration,
                    44_100,
                    events,
                    shutdown,
                    flush_ring,
                    bail_flush,
                    nerd,
                )
            }
        });
        (
            SwapHarness {
                tx,
                consumer,
                buffered,
                position,
                events,
                nerd,
                shutdown,
                flush_ring,
                bail_flush,
                handle: Some(handle),
            },
            low,
            high,
        )
    }

    fn source(path: &std::path::Path, title: &str, kbps: u32) -> TrackSource {
        TrackSource {
            source: path.display().to_string(),
            title: title.into(),
            artist: String::new(),
            start_seconds: 0.0,
            plan: TransitionPlan::default(),
            headers: std::collections::HashMap::new(),
            claimed_kbps: kbps,
            loudness_db: None,
            duration_seconds: 0.0,
        }
    }

    fn finish(mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
