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

use crate::sound::{Clarity, ClarityTuning, LoudnessMode, Protector, SoundMode};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use rtrb::Producer;

use crate::decode::resampler::StreamResampler;
use crate::decode::{SourceKind, SymphoniaDecoder};
use crate::eq::{EqCurve, EqualizerProcessor};
use crate::spatial::SpatialRenderer;
use crate::time_stretch::TimeStretch;
use crate::transition_filter::TransitionFilter;

mod fx;

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

fn accepts_analyzed_plan(phase: Phase, swap: bool) -> bool {
    phase == Phase::Arming && !swap
}

/// How long a technique's window is at `beat_seconds`, for the planner.
pub fn technique_window_seconds(style: TransitionStyle, beat_seconds: f64) -> Option<f64> {
    fx::window_seconds(style, beat_seconds)
}

/// Callback surface — the UniFFI-generated Swift side implements this.
pub trait EngineEvents: Send + Sync {
    fn state_changed(&self, state: crate::PlaybackState);
    fn track_ended(&self, reason: crate::TrackEndReason, source: String);
    fn error(&self, message: String);
    /// The incoming track became audible: current-track metadata flips now
    /// (upstream fires `onHandoff` as the first note sounds, not at blend end).
    fn handoff(&self, info: TrackInfo);
    fn duration_changed(&self, seconds: f64);
}

#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub title: String,
    pub artist: String,
    pub source: String,
    pub duration_seconds: f64,
    pub codec: String,
    pub sample_rate: u32,
    pub bit_depth: u32,
    pub channels: u32,
    pub kbps: u32,
}

/// What to load and how a transition out of/into it should be rendered.
#[derive(Debug, Clone)]
pub struct TrackSource {
    pub source: String,
    pub title: String,
    pub artist: String,
    /// Start position (Automix cue point). 0 = top.
    pub start_seconds: f64,
    pub plan: TransitionPlan,
    /// HTTP headers for the streaming reader (User-Agent, Origin, Referer).
    pub headers: std::collections::HashMap<String, String>,
    /// Claimed bitrate from the resolver (0 = unknown).
    pub claimed_kbps: u32,
    /// Per-track loudness from the catalogue/player response (`None` for local
    /// files and substitutes, which carry no figure). Upstream keeps the same
    /// map (`StreamResolver.loudnessDbFor`) and the enhancer stays off without
    /// one — normalizing against a made-up number would be worse than not
    /// normalizing.
    pub loudness_db: Option<f64>,
    /// Track length the *caller* knows, from the catalogue response or local
    /// metadata. 0 = unknown.
    ///
    /// The decoder can only report a duration the container declares, and plenty
    /// of what this engine plays declares none: a bare MP3 without a Xing/LAME
    /// duration tag, a progressively-fetched MP4 whose `moov` has not arrived
    /// yet, most WebM. That is not a cosmetic gap — a transition is *scheduled*
    /// against the end of the outgoing track, so with no end there is nowhere
    /// to schedule it, the incoming is never armed, and the queue advances by a
    /// cut at end-of-stream. The caller resolved the track and already has the
    /// number, so it is passed in rather than re-derived.
    pub duration_seconds: f64,
}

/// Port of upstream `playback.smart.TransitionStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionStyle {
    EqualPower,
    DjFilter,
    DjBlend,
    Gapless,
    /// Tempo-synced echo thrown on the outgoing record's last beat; the next
    /// one lands on its downbeat while the repeats ring out. See [`fx`].
    EchoOut,
    /// One beat captured and repeated in ½, ¼, ⅛ slices under a rising
    /// high-pass, then the next record on the one.
    LoopRoll,
    /// The outgoing record dissolves into a long reverb; the next one rises
    /// out of the wash.
    ReverbWash,
    /// A two-beat high-pass build and a hard cut on the downbeat — the drop swap.
    DropCut,
}

impl TransitionStyle {
    /// A technique that finishes the outgoing record with an effect rather
    /// than overlapping the two, and so needs no tempo match.
    pub fn is_effect(self) -> bool {
        fx::Technique::of(self).is_some()
    }
}

/// The style fields of upstream `TransitionPlan` — everything the render
/// needs and nothing else. Without analysis (v1) the default renders a plain
/// equal-power fade with filters open, exactly upstream's unanalysed fallback.
#[derive(Debug, Clone)]
pub struct TransitionPlan {
    pub style: TransitionStyle,
    pub bass_swap: bool,
    pub bass_swap_fraction: f64,
    pub filter_sweep: f64,
    pub vocal_overlap: f64,
    /// Explicit fade length for this pair; 0 = engine's crossfade window.
    pub fade_seconds: f64,
    /// Seconds from the blend start during which the outgoing stays at unity
    /// before it begins to fall; 0 = the normal equal-power crossing.
    ///
    /// A cold ending sets this to a phrase so the final chord is struck at
    /// unity and only its decay is faded through; the incoming entered a
    /// phrase earlier and is rising underneath it.
    pub outgoing_hold_seconds: f64,
    /// F's loop bridge: where on the outgoing's timeline the measured unit
    /// starts, how long it is (the measured downbeat span), and how many times
    /// it is replayed. 0 = no bridge.
    pub bridge_start_seconds: f64,
    pub bridge_seconds: f64,
    pub bridge_repeats: u32,
    /// The effect's own window inside the blend, in seconds (0 = no effect).
    /// The technique only processes the outgoing deck over this last part;
    /// the incoming rises across the whole `fade_seconds` crossfade.
    pub effect_seconds: f64,
    /// Where the effect window starts, as a position in the outgoing track's
    /// own timeline (0 = no effect).
    pub effect_start_seconds: f64,
    /// Where the blend *ends*, as a position in the outgoing track's own
    /// timeline; 0 means "at the file end".
    ///
    /// The planner chooses this — `outgoing_mix_end`, the ranked mix-out anchor,
    /// which is allowed to sit up to 12 s inside the track — and derives
    /// `bass_swap_fraction` and `vocal_overlap` from the window it defines.
    /// Without it the mixer armed the fade from the file's last byte instead, so
    /// on any track whose anchor is interior the entire blend — gains and both
    /// filter rides together — landed late by however much the anchor is early.
    pub transition_end_seconds: f64,
    pub cue_seconds: f64,
    /// Tempo stretch for the incoming voice's WSOLA stage. Held for the whole
    /// blend, then glided back to unity over `post_glide_seconds`.
    pub playback_rate: f64,
    /// Fade progress where the incoming bed ends and the rise into the swap
    /// begins. 0 means there is no bed — an equal-power fade or a gapless cut.
    pub bed_fraction: f64,
    /// Incoming level during the bed, in dB. About −14 is "playing underneath".
    pub bed_gain_db: f64,
    /// Depth of the one-beat dip in the incoming track just before the bass
    /// swap, 0…1. 0 means the rise is monotonic. The outgoing track never dips.
    pub dip_depth: f64,
    /// Width of that dip as a fraction of the fade. One beat of the shared grid.
    pub dip_width: f64,
    /// How long after the blend the incoming tempo glides home, in seconds.
    /// 0 releases the stretch as the blend ends.
    pub post_glide_seconds: f64,
    /// The outgoing track's length, as the planner measured it. 0 = unknown.
    ///
    /// A transition is scheduled against a planned mix-out anchor or the
    /// outgoing track's end. The decoder cannot always report its duration;
    /// this value lets a plain overlap start before end-of-stream in that case.
    ///
    /// The planner has the figure already — it measured the track to find its
    /// mix-out anchor — so it travels with the plan rather than being asked for
    /// again.
    pub outgoing_duration_seconds: f64,
    /// One beat of the outgoing record, in seconds; 0 = unknown. The effect
    /// techniques are measured in beats.
    pub beat_seconds: f64,
    /// Why the planner chose this style, for the transition log.
    pub reason: String,
}

impl Default for TransitionPlan {
    fn default() -> Self {
        Self {
            style: TransitionStyle::EqualPower,
            bass_swap: false,
            bass_swap_fraction: 0.7,
            filter_sweep: 0.0,
            vocal_overlap: 0.0,
            fade_seconds: 0.0,
            outgoing_hold_seconds: 0.0,
            bridge_start_seconds: 0.0,
            bridge_seconds: 0.0,
            bridge_repeats: 0,
            effect_seconds: 0.0,
            effect_start_seconds: 0.0,
            transition_end_seconds: 0.0,
            cue_seconds: 0.0,
            playback_rate: 1.0,
            bed_fraction: 0.0,
            bed_gain_db: 0.0,
            dip_depth: 0.0,
            dip_width: 0.0,
            post_glide_seconds: 0.0,
            outgoing_duration_seconds: 0.0,
            beat_seconds: 0.0,
            reason: String::new(),
        }
    }
}

/// Identity of the armed successor, for the skip peek. Title/artist is what
/// the app matches against its queue; the source disambiguates repeats.
#[derive(Clone, Debug)]
pub struct QueuedTrack {
    pub title: String,
    pub artist: String,
    pub source: String,
}

pub enum Command {
    /// Background preparation only applies to the voice it was prepared for.
    ForSource {
        source: String,
        command: Box<Command>,
    },
    Load {
        request: TrackSource,
        reply: Sender<Result<TrackInfo, String>>,
    },
    /// Same track, better source: open the replacement *at the outgoing voice's
    /// own position* and equal-power crossfade into it over
    /// `crossfade_seconds`. This is upstream's standby-player version swap
    /// (`swapCurrentToVersion`), and the reason it exists is that the reload
    /// path — stop, open, seek — puts a hole in the audio that nothing after it
    /// can hide. The position comes from the engine, not the caller: the
    /// caller only knows the audible playhead, which trails the decoder by the
    /// whole ring.
    SwapSource {
        request: TrackSource,
        crossfade_seconds: f64,
        reply: Sender<Result<TrackInfo, String>>,
    },
    QueueNext {
        request: TrackSource,
    },
    /// Peek at the armed successor without touching it: the pending request
    /// first, else the incoming voice of a running blend. Lets a manual Next
    /// promote exactly the track the listener picked instead of re-resolving
    /// it over the network.
    PendingTrack {
        reply: Sender<Option<QueuedTrack>>,
    },
    /// Manual Next into the armed successor, now instead of at the planned
    /// mix point. A sounding blend completes immediately; a merely queued
    /// request opens and replaces the current voice with the same backlog
    /// ramp a Load uses. Replies Err when nothing is armed, and the caller
    /// falls back to a full load. Upstream's skip-to-next-item, minus the
    /// playlist (the app owns the queue and flips its own index on reply).
    SkipToPending {
        skip_mix_enabled: bool,
        reply: Sender<Result<TrackInfo, String>>,
    },
    Play,
    Pause,
    Stop,
    /// Move the playhead. Carries no reply: a seek is a request, and blocking
    /// the caller's thread on the mixer's answer is a priority inversion when the
    /// caller is the main thread — which is exactly what a tap on a lyric line
    /// is. Both Android's `MediaPlayer.seekTo` and ExoPlayer's `seekTo` return
    /// before the seek lands, and the position reconciles from the playhead
    /// afterwards. A refusal arrives as an error event instead.
    Seek {
        seconds: f64,
    },
    SetVolume(f32),
    SetSleepGain(f32),
    HoldTrackEnd(bool),
    SetPlaybackRegions {
        source: String,
        regions: crate::PlaybackRegions,
    },
    /// Enables the unprocessed single-voice path. The output stream is
    /// separately rebuilt to a matching integer format before this is sent.
    SetBitPerfect(bool),
    SetCrossfadeWindow(f64),
    SetSpatial(bool),
    SetHeadYaw(f32),
    /// Manual filter aim on the current or incoming stream (shared's planner
    /// drives this once analysis exists). Overridden by the automatic ride
    /// while a transition is in flight.
    SetVoiceFilter {
        incoming: bool,
        low_hz: f32,
        high_hz: f32,
    },
    OpenFilters,
    SetPlaybackSpeed(f32),
    SetSkipSilence(bool),
    /// Full equaliser tuning: the ten-slot gains and Qs, the make-up preamp
    /// (computed once by the caller, never on the audio thread), and balance.
    SetEqTuning {
        enabled: bool,
        gains_db: [f32; crate::eq::EqLayout::SLOTS],
        qs: [f32; crate::eq::EqLayout::SLOTS],
        preamp_db: f32,
        balance: f32,
    },
    /// Loudness-normalization master switch. Per-voice gains stay where they
    /// are; the render multiplies by them only while this is set, so toggling
    /// needs no reload.
    SetLoudnessEnabled(bool),
    SetSoundMode(SoundMode),
    ObserveUnderruns(Arc<AtomicU64>),
    SetDecodeChunk(usize),
    SetPreferAppleAac(bool),
    StartDiagnosticCapture {
        directory: String,
        seconds: f64,
    },
    FinishDiagnosticCapture {
        reply: Option<Sender<Result<(), String>>>,
    },
    SetClarityTuning(ClarityTuning),
    SetLoudnessMode(LoudnessMode),
    SetLoudnessMeasurement(crate::LoudnessMeasurement),
    /// AirPods / speaker swap: new ring producer plus the device's native rate.
    SetOutputFormat {
        rate: u32,
        producer: Producer<f32>,
        generation: u64,
        acknowledged: Option<Sender<bool>>,
    },
    Shutdown,
}

// ---- Crossfade constants (verbatim from upstream CrossfadeController) ------

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
    let frames = (NORMAL_PREROLL_SECONDS * device_rate as f64).round().max(1.0) as usize;
    voice.prime_for_fade(frames, device_rate);
}

const FILTER_ENTRY_HZ: f64 = 7_000.0;
const FILTER_FLOOR_HZ: f64 = 300.0;
const BASS_SWAP_HZ: f64 = 200.0;
const BASS_SWAP_WIDTH: f64 = 0.10;
const FILTER_SWEEP_SHAPE: f64 = 0.75;
const ENTRY_HIGH_PASS_HZ: f64 = 1_200.0;
const ENTRY_OPEN_BY: f64 = 0.6;
const ENTRY_SHAPE: f64 = 0.35;
const VOCAL_SEPARATION_FLOOR_HZ: f64 = 1_600.0;
const VOCAL_SEPARATION_HIGH_PASS_HZ: f64 = 700.0;
const BLEND_ENTRY_HIGH_PASS_HZ: f64 = 520.0;
const BLEND_ENTRY_OPEN_BY: f64 = 0.45;
const BLEND_ENTRY_CLASH_HIGH_PASS_HZ: f64 = 950.0;
const BLEND_ENTRY_CLASH_OPEN_BY: f64 = 0.7;
const BLEND_EXIT_FROM: f64 = 0.3;
const BLEND_EXIT_LOW_PASS_HZ: f64 = 2_200.0;
const BLEND_EXIT_CLASH_FROM: f64 = 0.12;
const BLEND_EXIT_CLASH_LOW_PASS_HZ: f64 = 1_100.0;

/// Deepest dip a plan may ask for, as a fraction of the incoming gain.
///
/// A one-beat notch of this depth is about −9 dB on the record being brought
/// in. The outgoing track is untouched, so the blend dips a few dB and
/// recovers. Deeper than this reads as the transition losing the thread.
pub(crate) const MAX_DUCK_DEPTH: f64 = 0.65;

/// How long the incoming bed takes to rise out of silence, so the blend does
/// not open on a step.
const BED_ATTACK_SECONDS: f64 = 0.040;

/// Equal-power fall: `cos` of the half turn. Used by the bail ramp and as half
/// of the plain crossfade.
fn fall_gain(progress: f64) -> f32 {
    (progress.clamp(0.0, 1.0) * core::f64::consts::PI / 2.0).cos() as f32
}

fn rise_gain(progress: f64) -> f32 {
    (progress.clamp(0.0, 1.0) * core::f64::consts::PI / 2.0).sin() as f32
}

/// Smoothstep: zero slope at both ends.
fn smoothstep(p: f64) -> f64 {
    let p = p.clamp(0.0, 1.0);
    p * p * (3.0 - 2.0 * p)
}

fn db_to_gain(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0)
}

/// One-beat notch in the incoming gain, centred just before the bass swap.
///
/// Incoming only. The record already playing is the reference; dipping it
/// would dip the song the listener came for. Because the notch sits in a gain
/// that is otherwise climbing, the shape is "gains, quickly loses, gains".
fn dip_gain(progress: f64, plan: &TransitionPlan) -> f32 {
    let depth = plan.dip_depth.clamp(0.0, MAX_DUCK_DEPTH);
    if depth <= 0.0 {
        return 1.0;
    }
    let width = plan.dip_width.clamp(0.015, 0.25);
    let swap = plan.bass_swap_fraction.clamp(0.05, 0.95);
    let centre = (swap - width * 0.5).clamp(0.02, 0.95);
    let x = (progress.clamp(0.0, 1.0) - centre) / (width * 0.5);
    (1.0 - depth * (-x * x).exp()) as f32
}

/// DJ envelope: a ramp across the whole blend, with a bed at the open.
///
/// * The incoming track eases up from silence over ~40 ms, then climbs to
///   unity across the rest of the blend. A one-beat dip can still notch that
///   climb just before the low end changes hands.
/// * The outgoing track stays at full level while that bed is opening, then
///   follows a cos fall over the same span the incoming is climbing. The two
///   move together. Holding the outgoing at unity until a late swap, then
///   dropping it, is a cut once the blend itself is short.
fn automix_gains(progress: f64, plan: &TransitionPlan, hold: f64) -> (f32, f32) {
    let p = progress.clamp(0.0, 1.0);
    let swap = plan.bass_swap_fraction.clamp(0.15, 0.92);
    // A cold ending holds the outgoing until its final chord even through the
    // DJ bed's own fall point: the chord has to be struck at unity.
    let bed_end = plan.bed_fraction.clamp(0.05, swap - 0.02).max(hold).clamp(0.05, 0.95);
    let bed = db_to_gain(plan.bed_gain_db).clamp(0.02, 0.5);
    let attack = if plan.fade_seconds > 0.0 {
        (BED_ATTACK_SECONDS / plan.fade_seconds).clamp(0.004, 0.12)
    } else {
        0.02
    };
    let rise = if p <= attack {
        bed * smoothstep(p / attack)
    } else {
        let t = ((p - attack) / (1.0 - attack)).clamp(0.0, 1.0);
        bed + (1.0 - bed) * smoothstep(t)
    };
    let fall = if p <= bed_end {
        1.0
    } else {
        let t = ((p - bed_end) / (1.0 - bed_end)).clamp(0.0, 1.0);
        (t * core::f64::consts::PI / 2.0).cos()
    };
    ((rise * dip_gain(p, plan) as f64) as f32, fall as f32)
}

/// The incoming's level under the held outgoing during a cold ending's phrase,
/// in dB **relative to its own matched level**: the same scale into which
/// `mix_into` folds the transition match and the incoming's normalization. It
/// is never an absolute level, so a quiet record and a loud one both sit 12 dB
/// under the chord. The next record has to be audible under the final chord,
/// but an equal-power rise across the whole blend would have it at 0.7 by the
/// time the chord is struck — a +2 dB lift exactly where the listener is paying
/// attention to the ending. Rising to an underlay first keeps that below a
/// decibel and still lets the incoming take over across the decay.
const COLD_END_UNDERLAY_DB: f64 = -12.0;

/// The `(rise, fall)` pair when a cold ending holds the outgoing at unity until
/// its final chord, at `hold` (a fraction of the fade).
///
/// Before the chord the outgoing is untouched and the incoming lifts gently to
/// the underlay; from the chord the two cross over an equal-power fall that
/// ends exactly with the record, so the chord's attack is at unity and its decay
/// carries the incoming up. The summed power stays within about 0.5 dB of unity
/// across the hold and the crossing.
fn cold_end_gains(progress: f64, hold: f64) -> (f32, f32) {
    let p = progress.clamp(0.0, 1.0);
    let h = hold.clamp(0.0, 0.95);
    if h <= 0.0 {
        return fade_gains(p, false);
    }
    let underlay = db_to_gain(COLD_END_UNDERLAY_DB);
    if p <= h {
        // The chord is still ahead: the outgoing is at unity and the next
        // record is only establishing itself underneath it.
        let rise = underlay * smoothstep(p / h);
        return (rise as f32, 1.0);
    }
    let angle = ((p - h) / (1.0 - h)) * core::f64::consts::FRAC_PI_2;
    let rise = underlay + (1.0 - underlay) * angle.sin();
    (rise as f32, angle.cos() as f32)
}

/// The incoming's level by the throw when an effect plan does not carry one.
const EFFECT_INTRO_DB: f64 = -4.0;
/// Under the outgoing the incoming's bass is held out: one low end at a time,
/// and the next record's arrives on the landing.
const EFFECT_INTRO_HIGH_PASS_HZ: f64 = 300.0;
/// With `filter_sweep` at 1 (two grids that do not agree) the incoming's top
/// is held this low too, so its hats never tick against the outgoing's.
const EFFECT_INTRO_LOW_PASS_HZ: f64 = 3_500.0;

/// The `(rise, fall)` pair on an effect handover with an intro of `hold`
/// (a fraction of the fade).
///
/// Across the intro the outgoing stays at unity and the incoming climbs to
/// `intro_db`, filtered by [`ride_effect_intro`]: plainly audible, the two
/// records playing together. From the throw the technique shapes the outgoing
/// (its own dry curve, so `fall` matters only until then) and the incoming
/// rises the rest of the way.
fn effect_intro_gains(progress: f64, hold: f64, intro_db: f64) -> (f32, f32) {
    let p = progress.clamp(0.0, 1.0);
    let h = hold.clamp(0.0, 0.95);
    if h <= 0.0 {
        return fade_gains(p, false);
    }
    let intro = db_to_gain(intro_db.min(0.0));
    if p <= h {
        return ((intro * smoothstep(p / h)) as f32, 1.0);
    }
    let angle = ((p - h) / (1.0 - h)) * core::f64::consts::FRAC_PI_2;
    let rise = intro + (1.0 - intro) * angle.sin();
    (rise as f32, angle.cos() as f32)
}

/// The incoming's filter on an effect handover: bass out until the landing,
/// and on a grid clash (`band` 1) the top held back until the throw, opening
/// across it, so the record is whole when it lands.
fn ride_effect_intro(progress: f64, hold: f64, landing: f64, band: f64, inc: &mut TransitionFilter) {
    if progress >= landing {
        inc.open();
        return;
    }
    let opening = if landing > hold {
        ((progress - hold) / (landing - hold)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let amount = band.clamp(0.0, 1.0) * (1.0 - opening);
    let low_pass = glide(TransitionFilter::open_hz() as f64, EFFECT_INTRO_LOW_PASS_HZ, amount);
    inc.set_cutoffs(low_pass as f32, EFFECT_INTRO_HIGH_PASS_HZ as f32);
}

/// Where an effect plan's incoming lands, as a fraction of a fade of
/// `fade_seconds` whose last `1 - hold` is the technique window.
fn effect_landing(plan: &TransitionPlan, hold: f64) -> f64 {
    let land = fx::landing_fraction(plan.style, plan.beat_seconds).unwrap_or(0.5);
    hold + (1.0 - hold) * land
}

fn effect_intro_db(plan: &TransitionPlan) -> f64 {
    if plan.bed_gain_db < 0.0 {
        plan.bed_gain_db
    } else {
        EFFECT_INTRO_DB
    }
}

/// A plan's hold as a fraction of the fade actually being played. The engine's
/// span can be shorter than the plan's `fade_seconds` (a short incoming is
/// capped at a third of itself), and the hold is anchored to the blend start,
/// not to the plan.
fn hold_fraction(plan: &TransitionPlan, fade_seconds: f64) -> f64 {
    if fade_seconds <= 0.0 {
        0.0
    } else {
        (plan.outgoing_hold_seconds / fade_seconds).clamp(0.0, 0.95)
    }
}

/// The `(rise, fall)` gain pair for fade progress `p`.
///
/// Which pair is correct depends on whether the two sides carry the same
/// signal, and getting that wrong is audible either way:
///
/// * Different tracks with no DJ envelope are uncorrelated, so their powers
///   add and the equal-power pair holds the level still (`rise² + fall² = 1`).
/// * A source swap is one recording on both sides, aligned, so the amplitudes
///   add. The equal-power pair would then peak at `√2·sin(p·π/2 + π/4)` —
///   +3.01 dB through the middle of every upgrade. A linear pair sums to 1.
/// * A planned DJ blend is neither: [`automix_gains`] holds the outgoing track
///   up while the incoming one plays underneath, then hands over.
fn fade_gains(progress: f64, same_signal: bool) -> (f32, f32) {
    let p = progress.clamp(0.0, 1.0);
    if same_signal {
        (p as f32, (1.0 - p) as f32)
    } else {
        (rise_gain(p), fall_gain(p))
    }
}

/// Geometric interpolation between two cutoffs — pitch is logarithmic.
fn glide(from: f64, to: f64, amount: f64) -> f64 {
    from * (to / from).powf(amount.clamp(0.0, 1.0))
}

fn entry_high_pass(progress: f64, amount: f64, top_hz: f64, open_by: f64) -> f32 {
    let remaining = (1.0 - progress / open_by).clamp(0.0, 1.0);
    glide(
        TransitionFilter::off_hz() as f64,
        top_hz,
        amount * remaining.powf(ENTRY_SHAPE),
    ) as f32
}

fn bass_cutoff(amount: f64) -> f32 {
    glide(TransitionFilter::off_hz() as f64, BASS_SWAP_HZ, amount) as f32
}

fn blend_exit_low_pass(progress: f64, clash: f64) -> f32 {
    let from = BLEND_EXIT_FROM + (BLEND_EXIT_CLASH_FROM - BLEND_EXIT_FROM) * clash;
    let amount = ((progress - from) / (1.0 - from)).clamp(0.0, 1.0);
    let floor = glide(BLEND_EXIT_LOW_PASS_HZ, BLEND_EXIT_CLASH_LOW_PASS_HZ, clash);
    glide(TransitionFilter::open_hz() as f64, floor, amount) as f32
}

fn ride_filter_sweep(
    progress: f64,
    plan: &TransitionPlan,
    out: &mut TransitionFilter,
    inc: &mut TransitionFilter,
) {
    let sweep = plan.filter_sweep.clamp(0.0, 1.0);
    if sweep <= 0.0 {
        out.open();
        inc.open();
        return;
    }
    let open = TransitionFilter::open_hz() as f64;
    let entry = glide(open, FILTER_ENTRY_HZ, sweep);
    let floor = glide(open, FILTER_FLOOR_HZ, sweep);
    let cutoff = glide(entry, floor, progress.powf(FILTER_SWEEP_SHAPE));
    out.set_cutoffs(cutoff as f32, TransitionFilter::off_hz());
    inc.set_cutoffs(
        TransitionFilter::open_hz(),
        entry_high_pass(progress, sweep, ENTRY_HIGH_PASS_HZ, ENTRY_OPEN_BY),
    );
}

fn ride_vocal_separation(
    progress: f64,
    plan: &TransitionPlan,
    out: &mut TransitionFilter,
    inc: &mut TransitionFilter,
) {
    let amount = plan.vocal_overlap.clamp(0.0, 1.0);
    if amount <= 0.0 {
        out.open();
        inc.open();
        return;
    }
    let open = TransitionFilter::open_hz() as f64;
    let floor = glide(open, VOCAL_SEPARATION_FLOOR_HZ, amount);
    out.set_cutoffs(
        glide(open, floor, progress.powf(FILTER_SWEEP_SHAPE)) as f32,
        TransitionFilter::off_hz(),
    );
    inc.set_cutoffs(
        TransitionFilter::open_hz(),
        entry_high_pass(
            progress,
            amount,
            VOCAL_SEPARATION_HIGH_PASS_HZ,
            ENTRY_OPEN_BY,
        ),
    );
}

fn ride_bass_swap(
    progress: f64,
    plan: &TransitionPlan,
    out: &mut TransitionFilter,
    inc: &mut TransitionFilter,
) {
    let swap_at = plan.bass_swap_fraction.clamp(0.05, 0.95);
    let handover = ((progress - swap_at) / BASS_SWAP_WIDTH * 0.5 + 0.5).clamp(0.0, 1.0);
    let clash = plan.vocal_overlap.clamp(0.0, 1.0);
    let entry = bass_cutoff(1.0 - handover).max(entry_high_pass(
        progress,
        1.0,
        glide(
            BLEND_ENTRY_HIGH_PASS_HZ,
            BLEND_ENTRY_CLASH_HIGH_PASS_HZ,
            clash,
        ),
        BLEND_ENTRY_OPEN_BY + (BLEND_ENTRY_CLASH_OPEN_BY - BLEND_ENTRY_OPEN_BY) * clash,
    ));
    inc.set_cutoffs(TransitionFilter::open_hz(), entry);
    out.set_cutoffs(blend_exit_low_pass(progress, clash), bass_cutoff(handover));
}

fn ride_filters(
    progress: f64,
    plan: &TransitionPlan,
    out: &mut TransitionFilter,
    inc: &mut TransitionFilter,
) {
    match plan.style {
        TransitionStyle::DjFilter => ride_filter_sweep(progress, plan, out, inc),
        TransitionStyle::DjBlend => {
            if plan.bass_swap {
                ride_bass_swap(progress, plan, out, inc)
            } else {
                ride_vocal_separation(progress, plan, out, inc)
            }
        }
        // An album played through: any filtering would be an edit the record
        // didn't ask for.
        TransitionStyle::Gapless => {
            out.open();
            inc.open();
        }
        TransitionStyle::EqualPower => ride_vocal_separation(progress, plan, out, inc),
        // The effect techniques do their own filtering on the outgoing deck.
        TransitionStyle::EchoOut
        | TransitionStyle::LoopRoll
        | TransitionStyle::ReverbWash
        | TransitionStyle::DropCut => {
            out.open();
            inc.open();
        }
    }
}

// ---- Voices ----------------------------------------------------------------

const CHUNK_FRAMES: usize = 512;

struct Voice {
    _source_lease: crate::playback_regions::SourceLease,
    head_trim_allowed: bool,
    region_fade_in: usize,
    regions: crate::PlaybackRegions,
    skipped_regions: Vec<bool>,
    decoder: SymphoniaDecoder,
    resampler: StreamResampler,
    user_stretch: Option<TimeStretch>,
    spatial: SpatialRenderer,
    filter: TransitionFilter,
    gain: f32,
    /// The fade gain the last `mix_into` actually reached, so the next call can
    /// ramp from it instead of stepping once per 512-frame chunk.
    applied_gain: f32,
    /// Cue offset at open; zeroed by a seek (decoder position then runs from
    /// file start).
    base_position: f64,
    info: TrackInfo,
    finished: bool,
    decode_error: Option<String>,
    /// Device-domain frames emitted (post-resample).
    emitted_dev_frames: u64,
    /// Rate the post-decode DSP is currently running at; changes on a route
    /// rebuild, so it is a field rather than a constructor argument.
    device_rate: u32,
    /// Device-domain samples decoded/resampled but not yet served, plus the
    /// read cursor — pull never returns more than the caller asked for.
    pending_dev: Vec<f32>,
    pending_dev_cursor: usize,
    skip_silence: bool,
    /// Consecutive silent frames in the *device* domain. Dividing by the
    /// source rate (the earlier bug) made the 1 s floor shrink on 48/96 kHz
    /// output — the "make the music sound rushed" failure upstream raised
    /// `MIN_SILENCE_US` to avoid.
    silent_dev_frames: u64,
    /// The listener's speed, applied with pitch-preserving stereo WSOLA.
    ///
    /// Kept strictly separate from `plan_rate`: folding the handoff stretch
    /// into this resampler is exactly what made it a pitch bend, and the two
    /// being distinguishable here is what stops that coming back.
    effective_speed: f32,
    /// Automix handoff stretch, applied by WSOLA so it moves tempo without
    /// moving pitch. `None` whenever the rate is unity.
    stretch: Option<TimeStretch>,
    /// The handoff stretch has come home to unity and the stage is holding
    /// unplaced samples. It is drained into the output on the next pull rather
    /// than dropped, so retiring it is a handover and not a ~12 ms splice.
    stretch_draining: bool,
    /// Automix tempo stretch target; drives `stretch` and nothing else.
    plan_rate: f64,
    /// The stretch the plan asked for. Held for the blend, then the value the
    /// post-blend glide walks back to unity from.
    base_plan_rate: f64,
    /// Post-blend glide length, from the plan. 0 means release at the blend end.
    post_glide_seconds: f64,
    /// Output frames the glide spans, and how many have been rendered. Both
    /// zero unless a glide is in progress.
    glide_total_frames: u64,
    glide_done_frames: u64,
    /// The track length this voice plans against, if known. `None` means the
    /// container declared none and the caller supplied none, and every
    /// transition that would be scheduled against the end of this track is
    /// therefore impossible.
    known_duration: Option<f64>,
    /// Catalogue loudness figure for the normalization stage (`None` = no
    /// figure, no correction). The master switch lives on the mixer state, so
    /// this is the measurement only.
    loudness_db: Option<f64>,
    replay_gain: crate::metadata::ReplayGain,
    normalization_mode: LoudnessMode,
    measured_loudness: Option<crate::LoudnessMeasurement>,
    smoothed_normalization: f32,
    /// The transition's level correction in dB, relative to this deck's own
    /// normalization. Zero except while a blend is matching this deck to the one
    /// it is replacing.
    match_db: f32,
    /// The correction actually applied, ramped towards `match_db` so arming and
    /// releasing it are never a step.
    smoothed_match: f32,
    /// True while the correction is being walked back after the blend, which
    /// uses a slower ramp than arming it does.
    match_releasing: bool,
    /// Bypass all source/device resampling and voice DSP when rates match.
    bit_perfect: bool,
    /// F's loop bridge: one measured, vocal-free unit of the outro captured as
    /// it plays and replayed at the same point in the fade. `None` unless the
    /// planner asked for one and the blend has started.
    bridge: Option<VoiceBridge>,
}

impl Voice {
    /// `cue` marks a fresh start — a track being cued or mixed in — which the
    /// 5-second clearance rule may pull back to the top rather than let it open
    /// inside the outro. A source swap continues a track that is already
    /// playing, so its position is where the listener actually is and nothing
    /// may move it.
    fn open(
        request: &TrackSource,
        spatial_enabled: bool,
        head_yaw: f32,
        device_rate: u32,
        playback_speed: f32,
        skip_silence: bool,
        cue: bool,
    ) -> Result<Voice, String> {
        let source_lease = crate::playback_regions::SourceLease::new(&request.source);
        let kind = SourceKind::parse(&request.source);
        let mut decoder =
            SymphoniaDecoder::open(&kind, &request.headers).map_err(|e| e.to_string())?;
        let src_rate = decoder.sample_rate();
        crate::diagnostics::source(&request.source, src_rate);
        // Spatial + transition filter run in the device domain, after
        // resample — same 15 ms Haas window the DAC hears, and clip
        // harmonics never hit the sinc. Upstream's processor is also at
        // the sink rate; an earlier port ran this on source PCM and then
        // resampled the hard-clipped output, which rang.
        let mut spatial = SpatialRenderer::new(device_rate);
        spatial.set_enabled(spatial_enabled && decoder.channels() >= 2);
        spatial.set_head_yaw(head_yaw);
        let decoder_duration = decoder.duration_seconds();
        let mut duration = known_duration(decoder_duration, request.duration_seconds);
        // A download still in progress reports the container length of the
        // bytes on disk. Scheduling the blend against that starts the next
        // song in the middle of this one, over whatever fragment has arrived.
        // The catalogue figure is the length the blend has to land on.
        if source_still_growing(&request.source) && request.duration_seconds > duration + 1.0 {
            duration = request.duration_seconds;
        }
        let regions = crate::playback_regions::get(&request.source).normalized(duration);
        let start = if cue {
            let cleared = clamp_start_seconds(request.start_seconds, duration);
            bound_mix_in_depth(cleared, &request.plan, duration)
        } else {
            request.start_seconds.max(0.0)
        };
        let start = if request.start_seconds <= 0.0 || cue {
            regions.next_allowed(start)
        } else {
            start
        };
        if start > 0.0 {
            decoder.seek_seconds(start).map_err(|e| e.to_string())?;
        }
        log::info!(
            "voice opened: title={:?} requested_start={:.3}s clamped_start={:.3}s actual_start={:.3}s \
             duration={:.1}s (container={:?} declared={:.1}s) source_rate={} device_rate={}",
            request.title,
            request.start_seconds,
            start,
            decoder.position_seconds(),
            duration,
            decoder_duration,
            request.duration_seconds,
            src_rate,
            device_rate,
        );
        let plan_rate = if request.plan.playback_rate > 0.05 {
            request.plan.playback_rate
        } else {
            1.0
        };
        // The handoff stretch is *not* engaged here. It exists to hold two
        // records on one grid for the length of a blend, and `start_fade` is
        // where a blend begins — a `Load` (a tap on the queue, a skip) carries
        // the same plan record but has no blend to serve, and opening the stage
        // on it would leave a track playing 3 % slow for its whole length with
        // nothing to release it. `plan_rate` is recorded either way so the
        // glide has a number to walk from.
        let effective = (playback_speed as f64).clamp(0.5, 2.0) as f32;
        Ok(Voice {
            _source_lease: source_lease,
            head_trim_allowed: request.start_seconds <= 0.0 || cue,
            region_fade_in: 0,
            skipped_regions: vec![false; regions.excluded.len()],
            regions,
            resampler: StreamResampler::stereo(src_rate, device_rate),
            user_stretch: user_stretcher(device_rate, effective),
            spatial,
            filter: TransitionFilter::new(2, device_rate),
            gain: 1.0,
            applied_gain: 1.0,
            base_position: 0.0,
            info: TrackInfo {
                title: request.title.clone(),
                artist: request.artist.clone(),
                source: request.source.clone(),
                duration_seconds: duration,
                codec: decoder.codec().to_string(),
                sample_rate: src_rate,
                bit_depth: decoder.bit_depth(),
                channels: decoder.channels() as u32,
                kbps: request.claimed_kbps,
            },
            decoder,
            finished: false,
            decode_error: None,
            emitted_dev_frames: 0,
            device_rate,
            pending_dev: Vec::new(),
            pending_dev_cursor: 0,
            skip_silence,
            silent_dev_frames: 0,
            effective_speed: effective,
            stretch: None,
            stretch_draining: false,
            plan_rate,
            base_plan_rate: plan_rate,
            post_glide_seconds: request.plan.post_glide_seconds.max(0.0),
            glide_total_frames: 0,
            glide_done_frames: 0,
            known_duration: (duration > 0.0).then_some(duration),
            loudness_db: request.loudness_db,
            replay_gain: if let SourceKind::Path(path) = &kind {
                crate::metadata::read_replay_gain(path)
            } else {
                crate::metadata::ReplayGain::default()
            },
            normalization_mode: LoudnessMode::Off,
            measured_loudness: None,
            smoothed_normalization: 1.0,
            match_db: 0.0,
            smoothed_match: 1.0,
            match_releasing: false,
            bit_perfect: false,
            bridge: None,
        })
    }

    /// What this deck's integrated loudness says, for the transition match, and
    /// what normalization is already doing about it.
    fn deck_loudness(&self, normalization: bool) -> DeckLoudness {
        DeckLoudness {
            lufs: self.measured_loudness.as_ref().map(|m| m.track_lufs),
            relative_db: self.loudness_db,
            applied_normalization_db: normalization_gain(self, normalization)
                .1
                .map(|db| db as f64),
        }
    }

    /// Matches this deck's level to the one it is fading in over.
    ///
    /// Armed when the blend is armed, and the correction is **applied** here
    /// rather than ramped in during the first 0.4 s of the fade: the owner's
    /// "the next song blasts, then corrects" is exactly that late ramp. The
    /// fade gain is zero at the first audible frame and the incoming's own
    /// normalization is settled at the same time, so setting the targets here
    /// has no step to click — there is nothing audible yet to step.
    fn arm_transition_match(&mut self, outgoing: Option<&Voice>, normalization: bool) {
        // The incoming's own normalization also ramps for 50 ms inside
        // `mix_into`; the first audible frame must already carry it.
        self.smoothed_normalization = normalization_gain(self, normalization).0;
        let db = outgoing.and_then(|outgoing| {
            DeckLoudness::match_gain_db(
                self.deck_loudness(normalization),
                outgoing.deck_loudness(normalization),
            )
        });
        match db.filter(|db| db.abs() >= 0.05) {
            Some(db) => {
                if (db as f32 - self.match_db).abs() >= 0.05 || self.match_releasing {
                    log::info!(
                        "transition loudness match: {db:+.1} dB (applied before the first frame)"
                    );
                }
                self.match_db = db as f32;
                self.smoothed_match = db_to_gain(db) as f32;
                self.match_releasing = false;
            }
            None => {
                if self.match_db.abs() >= 0.05 {
                    log::info!("transition loudness match: cleared before the first frame");
                }
                self.match_db = 0.0;
                self.smoothed_match = 1.0;
                self.match_releasing = false;
            }
        }
    }

    /// Walks the correction back to none after the blend.
    ///
    /// The incoming deck keeps its normalization for the rest of the track; the
    /// match only existed to make the join level. Releasing it slowly means the
    /// join has no step in it either.
    fn release_transition_match(&mut self) {
        if self.match_db.abs() < 1e-4 {
            return;
        }
        self.match_db = 0.0;
        self.match_releasing = true;
    }

    fn mix_into(
        &mut self,
        samples: &[f32],
        destination: &mut [f32],
        fade_gain: f32,
        normalization: bool,
    ) {
        let target = normalization_gain(self, normalization).0;
        let alpha = 1.0 - (-1.0 / (self.device_rate as f32 * 0.050)).exp();
        // 0.4 s to put the match in place, 1.5 s to take it away: a boost that
        // appeared instantly would click, and one that vanished instantly would
        // be a step at the end of the mix.
        let match_seconds = if self.match_releasing { 1.5 } else { 0.4 };
        let alpha_match = 1.0 - (-1.0 / (self.device_rate as f32 * match_seconds)).exp();
        let target_match = db_to_gain(self.match_db as f64) as f32;
        // The caller recomputes the fade target once per 512-frame chunk, so
        // applying it flat makes a slow crossing a staircase (a zipper at the
        // low end). Walk from the previous chunk's applied gain to this one
        // across the samples; the next call starts where this one ended.
        let start_gain = self.applied_gain;
        self.applied_gain = fade_gain;
        let frames = (samples.len() / 2).max(1) as f32;
        for (index, (source, mix)) in samples
            .chunks_exact(2)
            .zip(destination.chunks_exact_mut(2))
            .enumerate()
        {
            let progress = index as f32 / (frames - 1.0).max(1.0);
            let fade = start_gain + (fade_gain - start_gain) * progress;
            self.smoothed_normalization += (target - self.smoothed_normalization) * alpha;
            if (target - self.smoothed_normalization).abs() < 1e-6 {
                self.smoothed_normalization = target;
            }
            self.smoothed_match += (target_match - self.smoothed_match) * alpha_match;
            let gain = fade * self.smoothed_normalization * self.smoothed_match;
            mix[0] += source[0] * gain;
            mix[1] += source[1] * gain;
        }
    }

    fn set_playback_speed(&mut self, speed: f32, device_rate: u32) {
        let effective = (speed as f64).clamp(0.5, 2.0) as f32;
        if (effective - self.effective_speed).abs() < 0.001 {
            return;
        }
        self.effective_speed = effective;
        if let Some(stretch) = &mut self.user_stretch {
            stretch.set_rate(effective as f64);
        } else {
            self.user_stretch = user_stretcher(device_rate, effective);
        }
    }

    /// Moves the automix handoff stretch to `rate`.
    ///
    /// Engages the WSOLA stage on the way up and retires it on the way back to
    /// unity. Retiring is a flush-and-drop rather than a bypass: at a rate of
    /// exactly 1.0 the stage reconstructs its input sample for sample, so the
    /// drained tail meets the pass-through with nothing to hear — which is what
    /// lets the glide land on unity at the end of a blend and leave nothing
    /// behind but the audio.
    fn set_plan_rate(&mut self, rate: f64) {
        let rate = if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            1.0
        };
        self.plan_rate = rate;
        if (rate - 1.0).abs() <= 1e-4 {
            // Hold the stage until the next pull drains it.
            self.stretch_draining = self.stretch.is_some();
        } else if let Some(stretch) = self.stretch.as_mut() {
            stretch.set_rate(rate);
        } else {
            self.stretch = Some(TimeStretch::new(rate));
        }
    }

    /// The natural end of a stretched stream: drain the stage into the pending
    /// buffer and drop it. Returns the drained samples so the caller can splice
    /// them in ahead of whatever comes next.
    fn drain_stretch(&mut self) -> Vec<f32> {
        match self.stretch.as_mut() {
            Some(stretch) => {
                stretch.flush();
                let tail = stretch.take();
                self.stretch = None;
                tail
            }
            None => Vec::new(),
        }
    }

    /// Releases the tempo stretch a beatmatched handoff stacked on this voice.
    ///
    /// The stretch exists only to hold two tracks on a shared grid for the
    /// length of the blend. Upstream undoes it unconditionally and idempotently
    /// the moment the fade ends — `CrossfadeController.finish` and `retire`
    /// both call `setPlaybackSpeed(AppSettings.playbackSpeed.value)` — because a
    /// voice that keeps it plays fast for its whole length: +3 % tempo and +51
    /// cents on every track that ever arrived through a beatmatched blend, with
    /// the published position running at the stretched rate as well.
    ///
    /// The guarantee for a voice that never got to glide: a skip, a bail, a
    /// cut at end-of-stream. Snaps the rate to unity and drains the stage on
    /// the next pull. A finished blend does not come here first — it calls
    /// [`Voice::begin_tempo_glide`] and this runs when that glide lands.
    fn release_plan_stretch(&mut self, speed: f32, device_rate: u32) {
        self.glide_total_frames = 0;
        self.glide_done_frames = 0;
        if (self.plan_rate - 1.0).abs() < f64::EPSILON && !self.stretch_draining {
            return;
        }
        // Clear the multiplier *before* re-deriving, or `set_playback_speed`
        // would simply stack the stretch back on.
        self.plan_rate = 1.0;
        self.stretch_draining = self.stretch.is_some();
        self.set_playback_speed(speed, device_rate);
    }

    /// Starts the post-blend walk from the matched tempo back to the record's
    /// own. No-op when the plan asked for none, in which case the caller
    /// releases immediately.
    fn begin_tempo_glide(&mut self, device_rate: u32) {
        if self.post_glide_seconds <= 0.05 || (self.base_plan_rate - 1.0).abs() < 1e-4 {
            return;
        }
        let frames = (self.post_glide_seconds * device_rate.max(1) as f64)
            .round()
            .max(1.0) as u64;
        self.glide_total_frames = frames;
        self.glide_done_frames = 0;
        self.set_plan_rate(self.base_plan_rate);
    }

    /// Sets the stretch to where the glide has reached. Call once per rendered
    /// chunk, before the pull, then [`Voice::note_glide_frames`] with what
    /// came out.
    fn drive_tempo_glide(&mut self, device_rate: u32, speed: f32) {
        if self.glide_total_frames == 0 {
            return;
        }
        let p = (self.glide_done_frames as f64 / self.glide_total_frames as f64).clamp(0.0, 1.0);
        if p >= 1.0 {
            self.release_plan_stretch(speed, device_rate);
            return;
        }
        let target = glided_plan_rate(self.base_plan_rate, p);
        if (target - self.plan_rate).abs() >= 1e-5 {
            self.set_plan_rate(target);
        }
    }

    fn note_glide_frames(&mut self, frames: u64) {
        if self.glide_total_frames > 0 {
            self.glide_done_frames = self.glide_done_frames.saturating_add(frames);
        }
    }

    /// Rebuild post-decode DSP for a new DAC rate (AirPods connect/disconnect).
    fn retarget_device(&mut self, device_rate: u32, spatial_enabled: bool, head_yaw: f32) {
        let src = self.info.sample_rate.max(1);
        self.device_rate = device_rate;
        let enabled = self.spatial.enabled();
        self.resampler = StreamResampler::stereo(src, device_rate);
        self.user_stretch = user_stretcher(device_rate, self.effective_speed);
        self.stretch = None;
        self.stretch_draining = false;
        let mut spatial = SpatialRenderer::new(device_rate);
        spatial.set_enabled(enabled && spatial_enabled && self.info.channels >= 2);
        spatial.set_head_yaw(head_yaw);
        self.spatial = spatial;
        self.filter = TransitionFilter::new(2, device_rate);
        self.pending_dev.clear();
        self.pending_dev_cursor = 0;
    }

    fn reset_processing(&mut self) {
        self.region_fade_in = 0;
        self.resampler.reset();
        self.user_stretch = user_stretcher(self.device_rate, self.effective_speed);
        self.stretch = None;
        self.stretch_draining = false;
        self.plan_rate = 1.0;
        self.glide_total_frames = 0;
        self.glide_done_frames = 0;
        self.spatial.flush();
        self.filter.flush();
        self.pending_dev.clear();
        self.pending_dev_cursor = 0;
        self.silent_dev_frames = 0;
        self.finished = false;
    }

    fn position_seconds(&self) -> f64 {
        let decoder = self.base_position + self.decoder.position_seconds();
        // The WSOLA stage holds roughly a frame of input it has not placed yet,
        // so the decoder runs that far ahead of the audio the listener is
        // hearing. Bounded at `FRAME + SEARCH` — about 12 ms — and only while a
        // handoff stretch is running, but a playhead that leads the sound is
        // the same bug as one that trails it.
        (decoder
            - self.stretch_latency_seconds()
            - self.resampler.delay_frames() / self.device_rate as f64
            - self
                .user_stretch
                .as_ref()
                .map(|s| s.latency_frames() / self.device_rate as f64)
                .unwrap_or(0.0))
        .max(0.0)
    }

    /// How far ahead of the output the decoder is running, in source seconds.
    fn stretch_latency_seconds(&self) -> f64 {
        self.stretch
            .as_ref()
            .map(|stretch| stretch.latency_frames() / self.device_rate.max(1) as f64)
            .unwrap_or(0.0)
    }

    /// Where this voice's *next served sample* sits on the source timeline.
    ///
    /// [`position_seconds`] is the decoder's read head, which runs ahead of the
    /// output by whatever `pending_dev` is holding. Opening a replacement
    /// source at that figure would leave it up to a full decode batch — around
    /// 190 ms at 22.05 kHz — behind the voice it is replacing, and a
    /// same-timeline crossfade that far out of alignment is heard as a doubled
    /// voice rather than as a quality change. Resampling preserves time, so the
    /// un-served device tail converts straight back to source seconds; the
    /// playback-speed factor maps it through the speed resampler.
    fn emitted_position_seconds(&self) -> f64 {
        let pending_frames = (self
            .pending_dev
            .len()
            .saturating_sub(self.pending_dev_cursor)
            / 2) as f64;
        let pending_seconds = pending_frames / self.device_rate.max(1) as f64
            * self.effective_speed as f64
            * self.plan_rate as f64;
        (self.position_seconds() - pending_seconds).max(0.0)
    }

    /// Remaining source-domain seconds the decoder still holds, if known.
    ///
    /// `info.duration_seconds` rather than the decoder's own figure, so a
    /// container that declares no length still schedules: `remaining_seconds`
    /// is what the arming and the fade start are both measured against, and
    /// `None` here means "there is no end to schedule against", which is a cut,
    /// not a blend.
    fn remaining_seconds(&self) -> Option<f64> {
        let total = self.regions.effective_end(self.known_duration?);
        Some((total - self.decoder.position_seconds()).max(0.0))
    }

    /// Remaining audible seconds from the next served sample, so a pre-rolled
    /// buffer counts as content ahead of the playhead rather than as already
    /// played.
    fn audible_remaining_seconds(&self) -> Option<f64> {
        let total = self.regions.effective_end(self.known_duration?);
        Some((total - self.emitted_position_seconds()).max(0.0))
    }

    /// Decode the first `frames` of an armed incoming into `pending_dev`
    /// without emitting them. The decoder, resampler and stretch are primed,
    /// and the buffer is what `pull` serves when the blend starts — so the
    /// first fade chunk already holds the record's own cue instead of paying
    /// for a cold decode while the outgoing has begun to fall.
    ///
    /// The audible timeline does not move: `emitted_position_seconds`
    /// subtracts the buffer, and `emitted_dev_frames` is reset because none of
    /// it has been heard.
    fn prime_for_fade(&mut self, frames: usize, device_rate: u32) {
        if frames == 0 || self.finished || !self.pending_dev.is_empty() {
            return;
        }
        // The handoff stretch is engaged at the fade normally; engage it here
        // so the pre-rolled samples are at the same rate the blend will use.
        let planned_rate = self.base_plan_rate;
        if (planned_rate - 1.0).abs() > 1e-4 {
            self.set_plan_rate(planned_rate);
        }
        let prefetched = self.pull(frames, device_rate);
        if prefetched.is_empty() {
            return;
        }
        let rest = self.pending_dev.split_off(self.pending_dev_cursor);
        self.pending_dev = prefetched;
        self.pending_dev.extend_from_slice(&rest);
        self.pending_dev_cursor = 0;
        self.emitted_dev_frames = 0;
        // A head-trim seek during the pull resets the processing chain; keep
        // the planned rate if that happened.
        if (planned_rate - 1.0).abs() > 1e-4
            && (self.plan_rate - planned_rate).abs() > 1e-9
        {
            self.set_plan_rate(planned_rate);
        }
    }

    /// Pulls up to `frames` device-domain interleaved stereo frames.
    /// Pulls up to `frames` device-domain interleaved stereo frames. Never
    /// returns more than asked: decoded batches overshooting the request are
    /// stashed in `pending_dev` and served on later calls.
    /// Serves the next chunk, running F's loop bridge when one is armed.
    ///
    /// While the bridge replays, the decoder is deliberately not pulled: the
    /// music after the captured unit stays in the decoder, so the unit can play
    /// again and the record then continues sample-continuously. The served
    /// frame count (`emitted_dev_frames`) still advances for the replay, so the
    /// blend's clock keeps moving.
    fn pull(&mut self, frames: usize, device_rate: u32) -> Vec<f32> {
        if let Some(bridge) = self.bridge.as_mut() {
            if bridge.stage == BridgeStage::Replaying {
                let mut out = bridge.replay(frames, device_rate);
                self.emitted_dev_frames += (out.len() / 2) as u64;
                if bridge.stage == BridgeStage::Done && out.len() < frames * 2 {
                    let more = self.pull_decoded((frames * 2 - out.len()) / 2, device_rate);
                    out.extend(more);
                }
                return out;
            }
        }
        let mut out = self.pull_decoded(frames, device_rate);
        if let Some(mut bridge) = self.bridge.take() {
            let held = bridge.after_pull(
                &mut out,
                &mut self.pending_dev,
                &mut self.pending_dev_cursor,
            );
            if held > 0 {
                self.emitted_dev_frames = self.emitted_dev_frames.saturating_sub(held as u64);
            }
            self.bridge = Some(bridge);
        }
        out
    }

    /// Arms the bridge at the blend: `delay_frames` are served before the unit
    /// starts, the unit is `unit_frames` long, and it is replayed
    /// `repeats` times. Bit-perfect playback has no blend, so it never arms.
    fn set_bridge(&mut self, delay_frames: u64, unit_frames: u64, repeats: u32) {
        if unit_frames == 0 || self.bit_perfect {
            return;
        }
        self.bridge = Some(VoiceBridge::new(delay_frames, unit_frames, repeats));
    }

    /// Pulls up to `frames` device-domain frames from the decoder. The public
    /// `pull` is the wrapper that runs F's loop bridge around this.
    fn pull_decoded(&mut self, frames: usize, device_rate: u32) -> Vec<f32> {
        if self.bit_perfect
            && device_rate == self.info.sample_rate
            && self.regions.excluded.is_empty()
            && self.regions.audible_start_seconds == 0.0
            && self.regions.audible_end_seconds.is_none()
        {
            return self.pull_bitperfect(frames);
        }
        let want = (frames * 2).max(2);
        let mut out: Vec<f32> = Vec::with_capacity(want);

        let serve_pending = |out: &mut Vec<f32>, voice: &mut Voice| {
            let remaining = want - out.len();
            if remaining == 0 || voice.pending_dev_cursor >= voice.pending_dev.len() {
                return;
            }
            let take = (voice.pending_dev.len() - voice.pending_dev_cursor).min(remaining);
            out.extend_from_slice(
                &voice.pending_dev[voice.pending_dev_cursor..voice.pending_dev_cursor + take],
            );
            voice.pending_dev_cursor += take;
            voice.emitted_dev_frames += (take / 2) as u64;
        };
        serve_pending(&mut out, self);

        // The handoff stretch has landed on unity: flush the stage and serve
        // what it was still holding before decoding anything more, so the
        // handover to the plain path is sample-continuous.
        if self.stretch_draining {
            self.stretch_draining = false;
            let tail = self.drain_stretch();
            if !tail.is_empty() {
                let remaining = want - out.len();
                let take = tail.len().min(remaining);
                out.extend_from_slice(&tail[..take]);
                self.emitted_dev_frames += (take / 2) as u64;
                if take < tail.len() {
                    self.pending_dev = tail;
                    self.pending_dev_cursor = take;
                }
                if out.len() >= want {
                    return out;
                }
            }
        }

        while out.len() < want && !self.finished {
            let at = self.decoder.position_seconds();
            if self.head_trim_allowed && at < self.regions.audible_start_seconds {
                // Seek once, then let read_stereo consume compressed preroll.
                // Repeating a seek before reading can never advance the codec
                // and can also loop on a boundary between source samples.
                self.head_trim_allowed = false;
                if self
                    .decoder
                    .seek_seconds(self.regions.next_allowed(at))
                    .is_ok()
                {
                    self.reset_processing();
                    continue;
                }
            }
            let end = self
                .regions
                .effective_end(self.known_duration.unwrap_or(0.0));
            let reached_end = end > 0.0 && at >= end;
            if let Some((index, segment)) =
                self.regions.excluded.iter().enumerate().find(|(i, s)| {
                    !self.skipped_regions[*i]
                        && at >= s.start_seconds - 0.0001
                        && at < s.end_seconds
                })
            {
                let target = segment.end_seconds;
                self.skipped_regions[index] = true;
                match self.decoder.seek_seconds(target) {
                    Ok(()) => {
                        self.reset_processing();
                        self.region_fade_in = (self.info.sample_rate as usize / 200).max(1);
                        log::info!("non-music segment skipped {:.3}..{:.3}", at, target);
                    }
                    Err(error) => {
                        log::warn!("segment seek failed: {error}");
                    }
                }
                continue;
            }
            let mut until = if end > 0.0 { end } else { f64::INFINITY };
            for (i, s) in self.regions.excluded.iter().enumerate() {
                if !self.skipped_regions[i] && s.start_seconds > at {
                    until = until.min(s.start_seconds);
                }
            }
            let frames = (((until - at) * self.info.sample_rate as f64).ceil() as usize)
                .max(1)
                .min(crate::diagnostics::chunk());
            let mut src = if reached_end {
                Vec::new()
            } else {
                self.decoder.read_stereo(frames).unwrap_or_else(|e| {
                    self.decode_error = Some(format!("decode failed: {e}"));
                    Vec::new()
                })
            };
            // Five-millisecond ramps around a policy cut avoid introducing a
            // discontinuity while leaving the source timeline untouched.
            let ramp_frames = (self.info.sample_rate as usize / 200).max(1);
            let soften_cut = self
                .regions
                .excluded
                .iter()
                .any(|s| s.start_seconds == until)
                || self.regions.audible_end_seconds == Some(until);
            if self.region_fade_in > 0 || soften_cut {
                for (i, frame) in src.chunks_exact_mut(2).enumerate() {
                    let incoming_gain = if self.region_fade_in > 0 {
                        let gain = 1.0 - self.region_fade_in as f32 / ramp_frames as f32;
                        self.region_fade_in -= 1;
                        gain
                    } else {
                        1.0
                    };
                    let to_cut = until - (at + i as f64 / self.info.sample_rate as f64);
                    let outgoing_gain = if soften_cut {
                        (to_cut / 0.005).clamp(0.0, 1.0) as f32
                    } else {
                        1.0
                    };
                    frame[0] *= incoming_gain * outgoing_gain;
                    frame[1] *= incoming_gain * outgoing_gain;
                }
            }
            if src.is_empty() {
                self.finished = true;
                self.pending_dev = self.resampler.flush();
                self.pending_dev_cursor = 0;
                if let Some(stretch) = &mut self.user_stretch {
                    let mut tail = stretch.process_and_take(&self.pending_dev);
                    stretch.flush();
                    tail.extend(stretch.take());
                    self.pending_dev = tail;
                }
                // A stretch still running at end of stream gets flushed, or the
                // last few milliseconds of the record are simply gone. The
                // resampler tail goes *through* it rather than being pasted onto
                // its output, so the two arrive in the order the chain actually
                // produced them.
                if self.stretch.is_some() {
                    let input = core::mem::take(&mut self.pending_dev);
                    let mut tail = self
                        .stretch
                        .as_mut()
                        .expect("checked above")
                        .process_and_take(&input);
                    let mut rest = self.drain_stretch();
                    tail.append(&mut rest);
                    self.pending_dev = tail;
                }
                if self.spatial.enabled() || self.filter.active() {
                    self.pending_dev
                        .extend(vec![0.0; self.device_rate as usize]);
                }
                // Device-domain DSP (spatial, then transition filter) — same
                // order as upstream's sink processors, at the rate the DAC hears.
                self.spatial.process(&mut self.pending_dev);
                self.filter.process(&mut self.pending_dev);
                serve_pending(&mut out, self);
                break;
            }
            crate::diagnostics::capture_voice(0, &self.info.source, &src);
            self.pending_dev = self.resampler.process(&src);
            self.pending_dev_cursor = 0;
            if let Some(stretch) = &mut self.user_stretch {
                self.pending_dev = stretch.process_and_take(&self.pending_dev);
            }
            // The handoff stretch runs here: in the device domain, after the
            // listener's pitch-preserving speed and before
            // the spatial and transition stages — so the blend's EQ is shaping
            // the stretched signal rather than being stretched itself, and a
            // filter sweep lands on the beat it was aimed at.
            if self.stretch.is_some() {
                let input = core::mem::take(&mut self.pending_dev);
                self.pending_dev = self
                    .stretch
                    .as_mut()
                    .expect("checked above")
                    .process_and_take(&input);
            }
            // Classify silence on the dry buffer. Spatial's 0.82 makeup
            // would otherwise pull quiet music under the threshold whenever
            // widening is on. DSP still runs so the delay line stays
            // continuous across a drop.
            let silent = self.skip_silence && is_silent(&self.pending_dev);
            self.spatial.process(&mut self.pending_dev);
            self.filter.process(&mut self.pending_dev);
            crate::diagnostics::capture_voice(1, &self.info.source, &self.pending_dev);
            if silent {
                self.silent_dev_frames += (self.pending_dev.len() / 2) as u64;
                // Upstream MIN_SILENCE_US = 1 s: keep the first second so a
                // breath still lands, then trim the rest of the gap. Upstream
                // keeps a 20 % sliver (capped at 2 s) at 10 % volume rather
                // than cutting to true digital silence, so a skipped gap still
                // reads as a gap and never clicks.
                if silence_exceeds_floor(self.silent_dev_frames, device_rate) {
                    let silent_s = self.silent_dev_frames as f64 / device_rate as f64;
                    if silent_s <= MIN_SILENCE_SECS + MAX_SILENCE_KEEP_SECS {
                        // Keep every 5th frame (20 % retention) at −20 dB.
                        let mut kept: Vec<f32> = Vec::with_capacity(self.pending_dev.len() / 5 + 2);
                        for frame in self.pending_dev.chunks_exact(2).step_by(5) {
                            kept.push(frame[0] * MIN_VOLUME_TO_KEEP);
                            kept.push(frame[1] * MIN_VOLUME_TO_KEEP);
                        }
                        self.pending_dev = kept;
                        self.pending_dev_cursor = 0;
                    } else {
                        self.pending_dev.clear();
                        self.pending_dev_cursor = 0;
                        continue;
                    }
                }
            } else {
                self.silent_dev_frames = 0;
            }
            serve_pending(&mut out, self);
        }
        out
    }

    /// Reads source frames directly. This branch is only used for stereo
    /// lossless PCM with the DAC clock set to the source rate; it deliberately
    /// avoids the resamplers, silence scan, spatial stage, and transition filter.
    fn pull_bitperfect(&mut self, frames: usize) -> Vec<f32> {
        if self.finished {
            return Vec::new();
        }
        match self.decoder.read_stereo(frames) {
            Ok(samples) => {
                if samples.is_empty() {
                    self.finished = true;
                } else {
                    self.emitted_dev_frames += (samples.len() / 2) as u64;
                }
                samples
            }
            Err(error) => {
                log::warn!("bit-perfect source read failed: {error}");
                self.finished = true;
                Vec::new()
            }
        }
    }
}

/// F's loop bridge: one measured, vocal-free unit of the outgoing's outro,
/// captured as it is served and replayed at the same point in the fade.
///
/// The unit's length is the planner's measured downbeat span (not bpm × 4), and
/// the seam is a **10 ms equal-gain** crossfade from the unit's end to its
/// start: both sides are the same record, so an equal-power seam would add
/// about +3 dB at the join.
struct VoiceBridge {
    /// Served frames to wait, from the blend start, before the unit begins.
    delay_frames: u64,
    /// The unit's length in device frames; the captured buffer is this long.
    unit_frames: u64,
    /// Extra passes after the first one.
    repeats: u32,
    /// Frames served since arming, for the delay.
    served: u64,
    /// The captured unit, interleaved stereo, `unit_frames` frames.
    captured: Vec<f32>,
    /// Frames already served of the current replay.
    replay_at: usize,
    done_replays: u32,
    stage: BridgeStage,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BridgeStage {
    Waiting,
    Capturing,
    Replaying,
    Done,
}

impl VoiceBridge {
    fn new(delay_frames: u64, unit_frames: u64, repeats: u32) -> Self {
        Self {
            delay_frames,
            unit_frames,
            repeats: repeats.max(1),
            served: 0,
            captured: Vec::with_capacity(unit_frames as usize * 2),
            replay_at: 0,
            done_replays: 0,
            stage: BridgeStage::Waiting,
        }
    }

    /// The replay's output for up to `frames` frames. The first 10 ms of each
    /// pass crossfades the audio just heard (the unit's end) into the unit's
    /// start; the replay's last sample is the same one the first pass ended on,
    /// so returning to the live stream needs no second fade.
    fn replay(&mut self, frames: usize, rate: u32) -> Vec<f32> {
        let seam =
            ((rate as u64 / 100).max(1) as usize).min((self.unit_frames / 4).max(1) as usize);
        let mut out = Vec::with_capacity(frames * 2);
        while out.len() < frames * 2 && self.stage == BridgeStage::Replaying {
            let at = self.replay_at;
            if at >= self.unit_frames as usize {
                self.replay_at = 0;
                self.done_replays += 1;
                if self.done_replays >= self.repeats {
                    self.stage = BridgeStage::Done;
                }
                continue;
            }
            if at < seam {
                let gain = (at + 1) as f32 / seam as f32;
                let tail = (self.unit_frames as usize - seam + at) * 2;
                out.push(self.captured[tail] * (1.0 - gain) + self.captured[at * 2] * gain);
                out.push(
                    self.captured[tail + 1] * (1.0 - gain) + self.captured[at * 2 + 1] * gain,
                );
            } else {
                out.push(self.captured[at * 2]);
                out.push(self.captured[at * 2 + 1]);
            }
            self.replay_at += 1;
        }
        out
    }

    /// Reads the frames just served. While waiting it counts down; while
    /// capturing it fills the unit; when the unit completes mid-chunk it holds
    /// the frames after it in `pending` (so the replay can play first) and
    /// switches to replaying. Returns the number of frames held back.
    fn after_pull(
        &mut self,
        out: &mut Vec<f32>,
        pending: &mut Vec<f32>,
        pending_cursor: &mut usize,
    ) -> usize {
        if self.stage != BridgeStage::Waiting && self.stage != BridgeStage::Capturing {
            return 0;
        }
        let mut frames = out.len() / 2;
        let mut skip = 0usize;
        if self.stage == BridgeStage::Waiting {
            if self.served + frames as u64 <= self.delay_frames {
                self.served += frames as u64;
                return 0;
            }
            let into = (self.delay_frames - self.served) as usize;
            skip = into.min(frames);
            self.served += skip as u64;
            frames -= skip;
            self.stage = BridgeStage::Capturing;
        }
        let captured_frames = self.captured.len() / 2;
        let remaining = (self.unit_frames as usize).saturating_sub(captured_frames);
        let take = frames.min(remaining);
        if take > 0 {
            let start = skip * 2;
            self.captured
                .extend_from_slice(&out[start..start + take * 2]);
        }
        if self.captured.len() / 2 == self.unit_frames as usize {
            let cut = (skip + take) * 2;
            let held = out.split_off(cut);
            let held_frames = held.len() / 2;
            let rest = pending[*pending_cursor..].to_vec();
            *pending = held;
            pending.extend(rest);
            *pending_cursor = 0;
            self.replay_at = 0;
            self.done_replays = 0;
            self.stage = BridgeStage::Replaying;
            return held_frames;
        }
        0
    }
}

/// Media3 `DEFAULT_SILENCE_THRESHOLD_LEVEL` (1024 of int16 full scale).
const SILENCE_THRESHOLD: f32 = 1024.0 / 32768.0;
/// Upstream `PlaybackService.MIN_SILENCE_US` — shortest gap we'll trim.
const MIN_SILENCE_SECS: f64 = 1.0;
/// Upstream Media3 `DEFAULT_MAX_SILENCE_TO_KEEP_DURATION_US` (2 s) — cap on the
/// sliver of a trimmed gap that is kept.
const MAX_SILENCE_KEEP_SECS: f64 = 2.0;
/// Upstream Media3 `DEFAULT_MIN_VOLUME_TO_KEEP_PERCENTAGE` (10) — the kept
/// sliver is faded to −20 dB rather than true digital silence.
const MIN_VOLUME_TO_KEEP: f32 = 0.1;

/// Upstream permits a +3 dB enhancer boost, but this engine has no true-peak
/// measurement for the source. Keep the safe attenuation range and avoid
/// boosting already mastered streams into the hard-clipping boundary.
pub const MIN_LOUDNESS_GAIN_DB: f64 = -15.0;
pub const MAX_LOUDNESS_GAIN_DB: f64 = 0.0;
/// Float samples must stay in range before the output callback. The previous
/// final `clamp` hard-cut any overs above 0 dBFS before they reached either
/// float or integer output.

/// How loud one deck is, in whichever integrated form the engine has for it,
/// and how much of that the normalization stage will already have taken out.
///
/// Two forms exist and they are not interchangeable: a measured LUFS figure from
/// the offline analyser, or the player response's relative figure. A transition
/// may only compare like with like — an absolute figure and a relative one say
/// nothing about each other — so a deck with no matching counterpart is left
/// alone rather than matched against a number of a different kind.
#[derive(Clone, Copy, Debug)]
pub struct DeckLoudness {
    /// Integrated loudness in LUFS, from `loudness::measure`.
    pub lufs: Option<f64>,
    /// The player response's relative figure, in dB. **Positive is louder**:
    /// `loudness_gain` attenuates by `-db`, so this is the same sign the
    /// normalization stage reads, not its inverse.
    pub relative_db: Option<f64>,
    /// The correction the normalization stage applies to this deck right now,
    /// in dB (`None` when it is off or has no figure). The match is measured
    /// *after* this, so the same difference is never corrected twice.
    pub applied_normalization_db: Option<f64>,
}

impl DeckLoudness {
    fn applied(self) -> f64 {
        self.applied_normalization_db
            .filter(|v| v.is_finite())
            .unwrap_or(0.0)
    }

    /// The level this deck plays at, in the measure the two decks share.
    ///
    /// A measured LUFS figure is the better evidence, so it is used whenever
    /// both decks have one. When only one does — which is the normal case while
    /// a transition is being armed, because the measurement is delivered for the
    /// *playing* deck and the incoming one has only just been opened — the
    /// player's relative figures are used instead, provided both decks have one.
    /// Comparing nothing because the two happen to be known in different forms
    /// would decline a match that both figures could have made.
    fn shared_level(incoming: Self, outgoing: Self) -> Option<(f64, f64)> {
        let (ai, ao) = (incoming.applied(), outgoing.applied());
        if let (Some(li), Some(lo)) = (
            incoming.lufs.filter(|v| v.is_finite()),
            outgoing.lufs.filter(|v| v.is_finite()),
        ) {
            return Some((li + ai, lo + ao));
        }
        if let (Some(ri), Some(ro)) = (
            incoming.relative_db.filter(|v| v.is_finite()),
            outgoing.relative_db.filter(|v| v.is_finite()),
        ) {
            return Some((ri + ai, ro + ao));
        }
        None
    }

    /// The gain, in dB, that puts the incoming deck at the outgoing deck's
    /// level: positive boosts the incoming, negative attenuates it.
    ///
    /// Clamped to ±6 dB. A transition may correct a level, but it is not a
    /// re-master: a figure that asks for more than 6 dB is more likely to be a
    /// bad measurement — a spoken intro, a silent tail — than a track that far
    /// off, and a wrong correction that large is worse than none.
    pub fn match_gain_db(incoming: DeckLoudness, outgoing: DeckLoudness) -> Option<f64> {
        let (a, b) = DeckLoudness::shared_level(incoming, outgoing)?;
        let delta = b - a;
        if !delta.is_finite() {
            return None;
        }
        Some(delta.clamp(-MAX_TRANSITION_MATCH_DB, MAX_TRANSITION_MATCH_DB))
    }
}

/// The loudest correction a transition may apply, either way.
pub const MAX_TRANSITION_MATCH_DB: f64 = 6.0;

/// The loudness correction for one voice: linear gain plus the applied dB for
/// the readout. `(1.0, None)` whenever there is nothing to correct with — the
/// switch off, no figure, or a non-finite one — which is also what the nerd
/// stats and the pipeline panel report, so "no correction" reads as absent
/// rather than as zero.
pub fn loudness_gain(loudness_db: Option<f64>, enabled: bool) -> (f32, Option<f32>) {
    match (enabled, loudness_db) {
        (true, Some(db)) if db.is_finite() => {
            let gain_db = (-db).clamp(MIN_LOUDNESS_GAIN_DB, MAX_LOUDNESS_GAIN_DB);
            (10f64.powf(gain_db / 20.0) as f32, Some(gain_db as f32))
        }
        _ => (1.0, None),
    }
}

/// A transparent-until-needed block peak guard for the mixed signal. It applies
/// one linear gain to the already-rendered chunk, preserving its waveform while
/// keeping normalization, EQ and overlapping voices from being hard-clipped at
/// the output boundary. Samples below the ceiling are returned unchanged.

/// The track length the engine will plan a transition against.
///
/// The container's own figure wins when it has one — it is measured from the
/// bytes actually being decoded, and a catalogue figure can be a rounding of a
/// different edit of the same record. The caller's figure is the fallback for
/// the containers that declare nothing, which is where a transition either gets
/// scheduled or does not happen at all.
fn known_duration(container: Option<f64>, declared: f64) -> f64 {
    container
        .filter(|d| d.is_finite() && *d > 0.0)
        .or_else(|| Some(declared).filter(|d| d.is_finite() && *d > 0.0))
        .unwrap_or(0.0)
}

/// `.grow` without `.complete` is a stream that has not finished arriving.
fn source_still_growing(path: &str) -> bool {
    if path.is_empty() || path.contains("://") {
        return false;
    }
    let grow = format!("{path}.grow");
    let complete = format!("{path}.complete");
    std::path::Path::new(&grow).is_file() && !std::path::Path::new(&complete).exists()
}

fn user_stretcher(_device_rate: u32, speed: f32) -> Option<TimeStretch> {
    if (speed - 1.0).abs() < 1e-6 {
        None
    } else {
        let mut stretch = TimeStretch::new(speed as f64);
        stretch.set_rate(speed as f64);
        Some(stretch)
    }
}

/// The stretch ratio at fade progress `p`: `base` walked to unity, or unity if
/// there is nothing to walk.
///
/// A *step* in the rate is a step in pitch as well as tempo:
/// `release_plan_stretch` undoing a 3 % stretch is 51 cents arriving in one
/// frame, at whatever gain the blend had reached. Upstream does exactly that,
/// and James Cridland's write-up of Apple's AutoMix names the audible artefact
/// of the smoother version of the same trick — "a bit of obvious speed slowing"
/// — as the mark of a real transition rather than a fault. What makes it read as
/// a move rather than a glitch is the ramp.
///
/// Geometric, because rate is a ratio: an exponential sweep is a constant slope
/// in cents per second, so the ear hears an even glide rather than a fast start
/// and a slow crawl. At `p = 1` it is exactly 1.0, which is the whole point —
/// the release then finds the rate already at unity and is a no-op, so there is
/// nothing to snap.
fn glided_plan_rate(base: f64, p: f64) -> f64 {
    if !base.is_finite() || base <= 0.0 || (base - 1.0).abs() < 1e-6 {
        return 1.0;
    }
    let p = p.clamp(0.0, 1.0);
    if p >= 1.0 {
        1.0
    } else {
        base.powf(1.0 - p)
    }
}

fn is_silent(interleaved: &[f32]) -> bool {
    // Empty is "no audio yet" (resampler still priming), not a gap to trim.
    if interleaved.is_empty() {
        return false;
    }
    interleaved.iter().all(|s| s.abs() < SILENCE_THRESHOLD)
}

fn silence_exceeds_floor(silent_dev_frames: u64, device_rate: u32) -> bool {
    silent_dev_frames as f64 / device_rate.max(1) as f64 > MIN_SILENCE_SECS
}

/// A voice's fade gain with the loudness correction folded in.
///
/// The fade gain (`voice.gain`) is the mix; the loudness gain is the level.
/// Multiplying here rather than in `Voice::pull` keeps the measurement (peak
/// classification for skip-silence) on the uncorrected signal — a quiet track
/// turned up is still a quiet track as far as gap detection is concerned.
fn normalization_gain(voice: &Voice, enabled: bool) -> (f32, Option<f32>) {
    if !enabled {
        return (1.0, None);
    }
    if let Some(db) = voice.replay_gain.gain_db(voice.normalization_mode) {
        return (10f32.powf(db as f32 / 20.0), Some(db as f32));
    }
    if let Some(db) = voice
        .measured_loudness
        .as_ref()
        .and_then(|m| m.gain_db(voice.normalization_mode))
    {
        return (10f32.powf(db as f32 / 20.0), Some(db as f32));
    }
    // YouTube relative loudness: attenuation only; no invented LUFS value.
    loudness_gain(voice.loudness_db, true)
}

/// A cue in the last few seconds is the outro, not a mix-in. Fall back to
/// the top of the file rather than starting the next song near its end.
fn clamp_start_seconds(requested: f64, duration: f64) -> f64 {
    let requested = requested.max(0.0);
    if requested <= 0.0 {
        return 0.0;
    }
    if duration <= 0.0 {
        return requested;
    }
    if requested >= (duration - MIN_INCOMING_CLEARANCE_S).max(0.0) {
        log::warn!(
            "cue {requested:.1}s is within {MIN_INCOMING_CLEARANCE_S}s of duration {duration:.1}s; starting at 0"
        );
        return 0.0;
    }
    requested
}

/// The last bound on a planned mix-in, applied to the value about to reach
/// `seek_seconds`.
///
/// The planner already applies this three ways, so this is deliberately
/// redundant. It is here because of how the original bug got so far: nothing
/// between the analysis and the decoder was willing to disagree with the layer
/// above it, and a plan asking to start the next record a minute in was obeyed
/// all the way down. A redundant check costs a comparison.
///
/// Only a *planned* mix-in is bounded. `start_seconds` doubles as a resume
/// position — `loadCurrent(startAt:)` after a cold restore carries a timestamp
/// minutes into the track and no plan — and clamping that would throw the
/// listener back to the top of a song they had just reopened.
fn bound_mix_in_depth(requested: f64, plan: &TransitionPlan, duration: f64) -> f64 {
    if requested <= 0.0 || duration <= 0.0 || plan.cue_seconds <= 0.0 {
        return requested;
    }
    let ceiling = MAX_CUE_SECONDS.min(duration * MAX_CUE_FRACTION);
    if requested > ceiling {
        log::warn!(
            "planned mix-in at {requested:.1}s exceeds the {MAX_CUE_SECONDS}s / \
             {MAX_CUE_FRACTION} bound for a {duration:.1}s track; starting at 0"
        );
        return 0.0;
    }
    requested
}

// ---- Transition state -------------------------------------------------------

#[derive(PartialEq, Clone, Copy)]
#[allow(dead_code)]
enum Phase {
    Idle,
    Arming,
    Fading,
}

struct TransitionState {
    phase: Phase,
    fade_frames: u64,
    handed_off: bool,
    plan: TransitionPlan,
    arm_deadline: Instant,
    /// A same-track source swap (quality upgrade / alternate version) rather
    /// than a queue advance. Upstream fades these with a plain equal-power
    /// curve and no filter ride — filtering a track against itself would edit
    /// the record — and never reports a track change for them.
    swap: bool,
    /// The outgoing deck's effect, for the techniques that finish a record
    /// with one. Created when the window opens.
    fx: Option<fx::DeckFx>,
    /// When inside the transition the effect sub-window starts, in frames from
    /// the blend start. Before it the outgoing plays dry on the blend's own
    /// fall; after it the effect dresses that fall.
    effect_start_frame: u64,
    /// The outgoing's level when its effect began, held for the rest of the
    /// window: the technique ramps its own dry out and rings its own tail,
    /// so feeding it the blend's still-falling gain fed an echo or a loop
    /// almost nothing, and the effect was 20–40 dB under the mix.
    effect_input_gain: Option<f32>,
}

struct RetiringVoice {
    voice: Voice,
    from_gain: f32,
    started_at: Instant,
}

struct MixerState {
    current: Option<Voice>,
    incoming: Option<Voice>,
    pending_next: Option<TrackSource>,
    /// Latched once a transition has been found unschedulable, so the warning
    /// about it is one line per track rather than one per chunk.
    warned_no_duration: bool,
    transition: Option<TransitionState>,
    retiring: Vec<RetiringVoice>,
    playing: bool,
    bit_perfect: bool,
    volume: f32,
    sleep_gain: f32,
    hold_end: bool,
    crossfade_window_s: f64,
    spatial_enabled: bool,
    head_yaw: f32,
    device_rate: u32,
    /// Frames pushed to the ring minus frames the callback consumed. The
    /// render loop needs this to know how much audio is still *audible* (ring
    /// backlog counts toward a track's tail).
    buffered_frames: Arc<AtomicU64>,
    position_ms: Arc<AtomicU64>,
    duration_ms: Arc<AtomicU64>,
    events: Arc<dyn EngineEvents>,
    state: crate::PlaybackState,
    /// Set by Load/Stop; the device callback discards leftover ring samples
    /// so the previous track cannot keep sounding under the new one.
    flush_ring: Arc<AtomicBool>,
    /// Set by hard_cut (skip / Load / Stop); the device callback ramps down
    /// the outgoing audio over 120 ms before discarding the backlog.
    bail_flush: Arc<AtomicBool>,
    playback_speed: f32,
    skip_silence: bool,
    eq: EqualizerProcessor,
    sound_mode: SoundMode,
    clarity_tuning: ClarityTuning,
    clarity: Clarity,
    protector: Protector,
    pipeline_tail_frames: usize,
    output_generation: u64,
    queue_target_ms: u64,
    underrun_counter: Option<Arc<AtomicU64>>,
    observed_underruns: u64,
    nerd: Arc<Mutex<NerdSnapshot>>,
    /// Loudness-normalization master switch (upstream
    /// `AppSettings.loudnessNormalization`, on by default). Per-voice figures
    /// ride on the voices; this decides whether the render uses them.
    loudness_enabled: bool,
    loudness_mode: LoudnessMode,
    /// Correlation probe for a source swap, see [`SwapProbe`].
    swap_probe: SwapProbe,
}

/// Measures how correlated the two sides of a source swap really are.
///
/// A swap fades one recording into another copy of itself, and which gain pair
/// keeps that level-flat depends entirely on the correlation `ρ` between the
/// two encodings: the linear pair is flat at ρ=1 and the equal-power pair at
/// ρ=0, and the port's deviation from flat with the linear pair is
/// `10·log10((1+ρ)/2)` — 0 dB at ρ=1, −0.11 at 0.95, −1.25 at 0.5.
///
/// Nothing else can supply that number, because it depends on how two encoders
/// of one master differ, so it has to come from a real upgrade. This reads it
/// off the samples that are genuinely being blended: both voices are already
/// decoded and aligned by the time they are rendered, so the probe costs a few
/// multiplies per sample for the length of the fade — no extra decoding and no
/// perturbation of what the listener hears.
#[derive(Default)]
struct SwapProbe {
    /// The outgoing voice's samples for the current chunk.
    current: Vec<f32>,
    /// How many of those the incoming voice's chunk also covers.
    len: usize,
    sum_aa: f64,
    sum_ab: f64,
    sum_bb: f64,
}

impl SwapProbe {
    fn reset(&mut self) {
        self.len = 0;
        self.sum_aa = 0.0;
        self.sum_ab = 0.0;
        self.sum_bb = 0.0;
    }

    /// Records the outgoing side of a chunk.
    fn observe_current(&mut self, frames: &[f32]) {
        if self.current.len() < frames.len() {
            self.current.resize(frames.len(), 0.0);
        }
        self.len = frames.len();
        self.current[..self.len].copy_from_slice(frames);
        for sample in &self.current[..self.len] {
            self.sum_aa += (*sample as f64) * (*sample as f64);
        }
    }

    /// Records the incoming side and the cross term. The fade gains are
    /// positive scalars, so they cancel out of a normalised correlation and
    /// the raw samples can be used on both sides.
    fn observe_incoming(&mut self, frames: &[f32]) {
        let n = frames.len().min(self.len);
        for index in 0..n {
            let outgoing = self.current[index] as f64;
            let incoming = frames[index] as f64;
            self.sum_ab += outgoing * incoming;
            self.sum_bb += incoming * incoming;
        }
    }

    /// Pearson correlation at zero lag, or `None` until both sides have been
    /// seen and both carry signal.
    fn correlation(&self) -> Option<f64> {
        if self.sum_aa <= 0.0 || self.sum_bb <= 0.0 {
            return None;
        }
        Some(self.sum_ab / (self.sum_aa.sqrt() * self.sum_bb.sqrt()))
    }
}

#[derive(Debug, Clone, Default)]
pub struct NerdSnapshot {
    pub codec: String,
    pub sample_rate: u32,
    pub bit_depth: u32,
    pub channels: u32,
    pub kbps: u32,
    /// Applied loudness correction in dB (`None` = no correction: switch off
    /// or no figure). The pipeline panel reads this, not the switch — the
    /// switch is the request, this is the answer.
    pub loudness_gain_db: Option<f32>,
    /// The level correction the last armed transition applied to its incoming
    /// deck, in dB (`None` = none was armed: no comparable figures, or already
    /// level). Kept after the handoff, like the swap correlation, because it is
    /// a fact about the join that was made.
    pub transition_match_db: Option<f32>,
    /// Pearson correlation between the two encodings of the last source swap,
    /// measured on the samples that were actually blended. `None` until a swap
    /// has run. This is data about the catalogue — how alike two encoders of one
    /// master are — and it is the only number that says whether the swap's
    /// linear fade is the flat one.
    pub swap_correlation: Option<f64>,
    pub active_stages: Vec<String>,
    pub converter_delay_frames: f64,
    pub protection_reduction_db: f32,
    pub protection_interventions: u64,
    pub build_revision: String,
    pub decoder_implementation: String,
    pub decoded_layout: String,
    pub queued_target_ms: u64,
    pub loudness_origin: String,
}

impl MixerState {
    fn diagnostic_settings(&self) -> serde_json::Value {
        serde_json::json!({"sound_mode":format!("{:?}",self.sound_mode),"clarity_preset":format!("{:?}",self.clarity_tuning.preset),"clarity_wet":self.clarity_tuning.wet,"clarity_trims_db":self.clarity_tuning.trims_db,"manual_eq":self.eq.diagnostic_settings(),"loudness_mode":format!("{:?}",self.loudness_mode),"playback_speed":self.playback_speed,"application_volume":self.volume,"bit_perfect":self.bit_perfect,"spatial_enabled":self.spatial_enabled,"head_yaw":self.head_yaw,"skip_silence":self.skip_silence,"crossfade_seconds":self.crossfade_window_s,"underruns":self.underrun_counter.as_ref().map(|c|c.load(Ordering::Relaxed))})
    }
    fn set_state(&mut self, state: crate::PlaybackState) {
        if self.state != state {
            self.state = state;
            self.events.state_changed(state);
        }
    }

    fn publish_nerd(&self, info: &TrackInfo, loudness_db: Option<f64>) {
        let (_, gain_db) = loudness_gain(
            loudness_db,
            self.loudness_enabled && self.sound_mode == SoundMode::Enhanced,
        );
        if let Ok(mut nerd) = self.nerd.lock() {
            // Preserved: they describe the last swap and the last transition,
            // not this track.
            let correlation = nerd.swap_correlation;
            let transition_match = nerd.transition_match_db;
            *nerd = NerdSnapshot {
                codec: info.codec.clone(),
                sample_rate: info.sample_rate,
                bit_depth: info.bit_depth,
                channels: info.channels,
                kbps: info.kbps,
                loudness_gain_db: gain_db,
                swap_correlation: correlation,
                transition_match_db: transition_match,
                ..NerdSnapshot::default()
            };
        }
    }

    /// Upstream `retire` + skip: drop every voice and the pending next so a
    /// user-chosen track cannot mix with the one it replaced.
    ///
    /// Rather than hard-clearing the output ring on a sample boundary (which
    /// produces an audible click), triggers `bail_flush`: the output callback
    /// smoothly ramps out the next 120 ms of buffered audio and discards the
    /// remaining backlog before the new track starts.
    fn hard_cut(&mut self) {
        self.eq.flush();
        self.clarity.reset();
        self.protector.reset();
        self.pipeline_tail_frames = 0;
        self.transition = None;
        self.incoming = None;
        self.pending_next = None;
        self.retiring.clear();
        self.current = None;
        if self.bit_perfect {
            self.flush_ring.store(true, Ordering::Release);
        } else {
            self.bail_flush.store(true, Ordering::Release);
        }
    }

    /// Gapless: the current track ended and the next file is already queued.
    fn promote_pending(&mut self) {
        if self.hold_end {
            return;
        }
        let Some(request) = self.pending_next.take() else {
            return;
        };
        match Voice::open(
            &request,
            self.spatial_enabled && self.sound_mode == SoundMode::Enhanced,
            self.head_yaw,
            self.device_rate,
            self.playback_speed,
            self.skip_silence && self.sound_mode == SoundMode::Enhanced,
            true,
        ) {
            Ok(mut voice) => {
                voice.gain = 1.0;
                voice.release_plan_stretch(self.playback_speed, self.device_rate);
                let info = voice.info.clone();
                let duration = info.duration_seconds;
                let (_, gain_db) = loudness_gain(
                    voice.loudness_db,
                    self.loudness_enabled && self.sound_mode == SoundMode::Enhanced,
                );
                if let Ok(mut nerd) = self.nerd.lock() {
                    let correlation = nerd.swap_correlation;
                    *nerd = NerdSnapshot {
                        codec: info.codec.clone(),
                        sample_rate: info.sample_rate,
                        bit_depth: info.bit_depth,
                        channels: info.channels,
                        kbps: info.kbps,
                        loudness_gain_db: gain_db,
                        swap_correlation: correlation,
                        ..NerdSnapshot::default()
                    };
                }
                log::info!("promoted pending next: {}", info.title);
                self.current = Some(voice);
                self.duration_ms
                    .store((duration * 1000.0) as u64, Ordering::Relaxed);
                if duration > 0.0 {
                    self.events.duration_changed(duration);
                }
                self.publish_audible_position();
                self.events.handoff(info);
            }
            Err(e) => {
                log::warn!("promote pending failed for {}: {e}", request.title);
                self.events
                    .error(format!("could not prepare next track: {e}"));
            }
        }
    }

    fn promote_incoming(&mut self) {
        if let Some(mut incoming) = self.incoming.take() {
            incoming.gain = 1.0;
            incoming.filter.open();
            // It is the session voice now; a beatmatched handoff's stretch does
            // not outlive the blend it was for.
            incoming.release_plan_stretch(self.playback_speed, self.device_rate);
            let info = incoming.info.clone();
            let duration = info.duration_seconds;
            self.current = Some(incoming);
            self.duration_ms
                .store((duration * 1000.0) as u64, Ordering::Relaxed);
            if duration > 0.0 {
                self.events.duration_changed(duration);
            }
            let db = self.current.as_ref().and_then(|v| v.loudness_db);
            self.publish_nerd(&info, db);
            self.publish_audible_position();
            self.events.handoff(info);
        }
        self.transition = None;
    }

    /// Replaces the current voice's source in place, crossfading into the
    /// replacement — upstream `swapCurrentToVersion`.
    ///
    /// The replacement opens at the outgoing voice's *emitted* position, so the
    /// moment the fade starts both voices are reading the same instant of the
    /// same recording. The fade is driven by the incoming voice's own output,
    /// so it waits for the decoder to prime rather than guessing how long that
    /// takes; until then the outgoing voice stays at unity and nothing is lost.
    fn swap_source(
        &mut self,
        request: TrackSource,
        crossfade_seconds: f64,
    ) -> Result<TrackInfo, String> {
        if self.current.is_none() {
            return Err("no track to swap".into());
        }
        // A queue transition in flight belongs to the track that is leaving.
        // Abandoning it here would promote an incoming track we are about to
        // discard, so the swap is refused instead and the caller can retry.
        if self.transition.is_some() {
            return Err("a transition is already running".into());
        }
        // The blend has not armed yet, but it is inside the window where it
        // will. A 550 ms source swap would take the only transition slot, the
        // song change would never arm, and the next track would cut in.
        if self.pending_next.is_some() {
            if let Some(current) = &self.current {
                let plan = self
                    .pending_next
                    .as_ref()
                    .map(|request| request.plan.clone())
                    .unwrap_or_default();
                let fade_s =
                    self.plan_seconds(&plan, current.known_duration.unwrap_or(0.0));
                let tail = self.audible_tail_s(&plan, current);
                let horizon = fade_s + ARM_LEAD_MS as f64 / 1000.0;
                if tail.is_finite() && tail <= horizon {
                    return Err("a transition is already running".into());
                }
            }
        }
        let anchor = self
            .current
            .as_ref()
            .map(|voice| voice.emitted_position_seconds())
            .unwrap_or(0.0);
        let mut request = request;
        request.start_seconds = anchor;
        let mut voice = Voice::open(
            &request,
            self.spatial_enabled && self.sound_mode == SoundMode::Enhanced,
            self.head_yaw,
            self.device_rate,
            self.playback_speed,
            self.skip_silence && self.sound_mode == SoundMode::Enhanced,
            false,
        )?;
        voice.gain = 0.0; // silent until the fade lifts it
        voice.applied_gain = 0.0;
        let info = voice.info.clone();
        let seconds = if crossfade_seconds > 0.0 {
            crossfade_seconds
        } else {
            SWAP_CROSSFADE_MS / 1000.0
        };
        let fade_frames = (seconds * self.device_rate as f64).max(1.0) as u64;
        log::info!(
            "source swap: {} at {:.3}s, {:.0}ms crossfade",
            info.source,
            anchor,
            seconds * 1000.0
        );
        self.swap_probe.reset();
        self.incoming = Some(voice);
        self.transition = Some(TransitionState {
            phase: Phase::Fading,
            fade_frames,
            handed_off: true,
            plan: TransitionPlan::default(),
            arm_deadline: Instant::now(),
            swap: true,
            fx: None,
            effect_start_frame: 0,
            effect_input_gain: None,
        });
        // Same track, same instant: the timeline does not move, so this only
        // re-points the playhead at the replacement voice.
        self.publish_audible_position();
        Ok(info)
    }

    /// Arms a transition when the current track's audible tail is close
    /// enough. Returns true if a transition was created.
    /// Where the outgoing track ends, on its own timeline, or `INFINITY` when
    /// nothing knows.
    ///
    /// The planned anchor is authoritative for when to transition. The
    /// duration figures are the fallback when no anchor was analyzed.
    ///
    /// 1. `transition_end_seconds` — the planner's ranked mix-out anchor, which
    ///    is where the blend is meant to *finish* and can sit up to 12 s inside
    ///    the track. Using it is what stops a whole blend landing late.
    /// 2. `outgoing_duration_seconds` — the planner's measurement of the track,
    ///    which exists for exactly the case the decoder cannot cover.
    /// 3. The live decoder's own remaining time, which is the file end and is
    ///    `None` for a container that declares no length.
    ///
    /// The second is not redundant with the third on purpose: the planner reads
    /// the track through the metadata reader to find its mix-out anchor, so it
    /// has a figure in hand whether or not the decoder that will play the track
    /// was able to work one out.
    fn outgoing_end_s(&self, plan: &TransitionPlan, current: &Voice) -> f64 {
        let analyzed_end = (plan.transition_end_seconds.is_finite()
            && plan.transition_end_seconds > 0.0)
            .then_some(plan.transition_end_seconds);
        let decoded_end = current
            .known_duration
            .filter(|duration| duration.is_finite() && *duration > 0.0);
        if let Some(end) = analyzed_end {
            // A stale or inaccurate analysis anchor must never schedule the
            // blend past the audio the decoder can actually produce.
            return decoded_end.map_or(end, |duration| {
                end.min(current.regions.effective_end(duration))
            });
        }
        if let Some(duration) = decoded_end {
            return current.regions.effective_end(duration);
        }
        if plan.outgoing_duration_seconds.is_finite() && plan.outgoing_duration_seconds > 0.0 {
            return plan.outgoing_duration_seconds;
        }
        current.position_seconds() + current.remaining_seconds().unwrap_or(f64::INFINITY)
    }

    /// The audible tail left in the outgoing track, in seconds, or `INFINITY`
    /// when the end is unknown and therefore unschedulable.
    fn audible_tail_s(&self, plan: &TransitionPlan, current: &Voice) -> f64 {
        let end_s = self.outgoing_end_s(plan, current);
        if !end_s.is_finite() {
            return f64::INFINITY;
        }
        let buffered_s =
            self.buffered_frames.load(Ordering::Relaxed) as f64 / self.device_rate as f64;
        (end_s - current.position_seconds()).max(0.0) + buffered_s
    }

    /// A later plan for the track already armed, applied only while the fade
    /// has not started.
    ///
    /// The safety overlap is queued as soon as the next file's first bytes
    /// exist, and the analysed plan can take longer than that. Dropping the
    /// analysed plan because the safety one already armed is how a blend that
    /// was ready still played out to the file's last byte.
    fn upgrade_armed_plan(&mut self, request: TrackSource) {
        let still_waiting = self
            .transition
            .as_ref()
            .is_some_and(|transition| accepts_analyzed_plan(transition.phase, transition.swap));
        if !still_waiting {
            log::debug!("queueNext ignored; transition already fading");
            return;
        }
        let cue = request.start_seconds.max(0.0);
        let track = self
            .current
            .as_ref()
            .and_then(|voice| voice.known_duration)
            .unwrap_or(0.0);
        let fade_s = self.plan_seconds(&request.plan, track);
        let rate = if request.plan.playback_rate > 0.05 {
            request.plan.playback_rate
        } else {
            1.0
        };
        let glide = request.plan.post_glide_seconds.max(0.0);
        if let Some(transition) = &mut self.transition {
            if fade_s > 0.0 {
                transition.fade_frames = (fade_s * self.device_rate as f64) as u64;
            }
            transition.plan = request.plan;
        }
        if let Some(voice) = &mut self.incoming {
            voice.base_plan_rate = rate;
            voice.plan_rate = rate;
            voice.post_glide_seconds = glide;
            if (voice.decoder.position_seconds() - cue).abs() > 0.05 {
                voice.base_position = 0.0;
                voice.reset_processing();
                if let Err(e) = voice.decoder.seek_seconds(cue) {
                    log::warn!("could not move the armed cue to {cue:.2}s: {e}");
                }
            }
            // A moved cue clears the primed buffer in `reset_processing`; fill
            // it again so the first fade chunk is not a cold decode.
            prime_voice(voice, self.device_rate);
        }
        log::info!("updated armed transition: fade={fade_s:.1}s cue={cue:.2}s rate={rate:.4}");
    }

    /// Convert an armed successor into an immediate two-beat EchoOut when a
    /// trusted planner grid is present. The queue owner keeps its current skip
    /// path when this returns `None`. This is the native short-window port of
    /// upstream `DesktopAudioEngine.beginSkipMix`.
    fn begin_skip_mix(&mut self) -> Option<TrackInfo> {
        let transition = self.transition.as_mut()?;
        if !accepts_analyzed_plan(transition.phase, transition.swap) {
            return None;
        }
        let beat = transition.plan.beat_seconds;
        if !beat.is_finite() || !(0.25..=1.5).contains(&beat) {
            return None;
        }
        let current = self.current.as_ref()?;
        let incoming = self.incoming.as_ref()?;
        let device_rate = self.device_rate.max(1);
        let beat_frames = (beat * device_rate as f64).round().max(1.0) as u64;
        let fade_frames = beat_frames.saturating_mul(2);
        let fade_seconds = fade_frames as f64 / device_rate as f64;
        if current
            .audible_remaining_seconds()
            .is_some_and(|remaining| remaining < fade_seconds)
            || incoming
                .audible_remaining_seconds()
                .is_some_and(|remaining| remaining < fade_seconds)
        {
            return None;
        }
        let start = current.emitted_position_seconds();
        let info = incoming.info.clone();
        transition.fade_frames = fade_frames;
        transition.plan.style = TransitionStyle::EchoOut;
        transition.plan.fade_seconds = fade_seconds;
        transition.plan.transition_end_seconds = start + fade_seconds;
        transition.plan.effect_start_seconds = start;
        transition.plan.effect_seconds = fade_seconds;
        transition.plan.outgoing_hold_seconds = 0.0;
        transition.plan.bed_fraction = 0.0;
        transition.plan.bed_gain_db = 0.0;
        transition.plan.dip_depth = 0.0;
        transition.plan.dip_width = 0.0;
        transition.plan.playback_rate = 1.0;
        transition.plan.post_glide_seconds = 0.0;
        transition.plan.reason = "skip-mix:".into();
        if let Some(incoming) = &mut self.incoming {
            incoming.base_plan_rate = 1.0;
            incoming.post_glide_seconds = 0.0;
            incoming.release_plan_stretch(self.playback_speed, self.device_rate);
        }
        log::info!(
            "automix: skip-mix style=EchoOut window={fade_seconds:.2}s beat={beat:.3}s"
        );
        Some(info)
    }

    /// Opens a different file on a transition that has not started sounding.
    ///
    /// This is how a better encode of the next record joins the blend: the
    /// ramp the listener hears is the song change, not a second crossfade
    /// laid on top of it. Once the fade is audible the voice stays — swapping
    /// it then is the cut this exists to avoid.
    fn retarget_armed_source(&mut self, request: TrackSource) {
        match Voice::open(
            &request,
            self.spatial_enabled && self.sound_mode == SoundMode::Enhanced,
            self.head_yaw,
            self.device_rate,
            self.playback_speed,
            self.skip_silence && self.sound_mode == SoundMode::Enhanced,
            true,
        ) {
            Ok(mut voice) => {
                voice.gain = 0.0;
                voice.applied_gain = 0.0;
                voice.normalization_mode = self.loudness_mode;
                voice.arm_transition_match(
                    self.current.as_ref(),
                    self.loudness_enabled && self.sound_mode == SoundMode::Enhanced,
                );
                let track = self
                    .current
                    .as_ref()
                    .and_then(|current| current.known_duration)
                    .unwrap_or(0.0);
                let fade_s = self.plan_seconds(&request.plan, track);
                let rate = if request.plan.playback_rate > 0.05 {
                    request.plan.playback_rate
                } else {
                    1.0
                };
                voice.base_plan_rate = rate;
                voice.plan_rate = rate;
                voice.post_glide_seconds = request.plan.post_glide_seconds.max(0.0);
                // Prime the replacement before it is needed: this voice is
                // pulled for the first time at the fade, and a cold decode is
                // the hole the blend cannot afford.
                prime_voice(&mut voice, self.device_rate);
                if let Some(transition) = &mut self.transition {
                    if fade_s > 0.0 {
                        transition.fade_frames = (fade_s * self.device_rate as f64) as u64;
                    }
                    transition.plan = request.plan;
                }
                log::info!(
                    "retargeted armed transition to {} fade={fade_s:.1}s",
                    voice.info.source
                );
                self.incoming = Some(voice);
            }
            Err(e) => {
                log::warn!("could not retarget the armed transition: {e}");
            }
        }
    }

    fn consider_arm(&mut self) {
        if self.hold_end
            || self.transition.is_some()
            || self.pending_next.is_none()
            || !self.playing
        {
            return;
        }
        let Some(current) = &self.current else {
            return;
        };
        if current.finished {
            return;
        }
        // Unknown or implausibly short duration (muxed MP4 used to report
        // AAC packet counts as PCM frames): wait for real EOS instead of
        // blending immediately. Upstream uses ExoPlayer's container duration.
        if current.known_duration.unwrap_or(0.0) < 2.0 && current.position_seconds() < 2.0 {
            return;
        }
        // The plan decides both where the blend ends and how long it runs —
        // upstream arms from `plan.transitionStart` (CrossfadeController.kt:713)
        // and starts the fade from the same window (`:731` → `:967`). Arming
        // from the file end instead prepared the incoming only after the moment
        // the fade was already due, which pushed the whole blend late.
        let plan = self
            .pending_next
            .as_ref()
            .map(|request| request.plan.clone())
            .unwrap_or_default();
        let window_s = self.effective_fade_seconds(current.known_duration.unwrap_or(0.0));
        let fade_s = if plan.fade_seconds > 0.0 {
            self.plan_seconds(&plan, current.known_duration.unwrap_or(0.0))
        } else {
            window_s
        };
        let audible_tail_s = self.audible_tail_s(&plan, current);
        if !audible_tail_s.is_finite() {
            // There is no end to schedule against, so there is no blend: the
            // incoming is never armed and the queue advances by a cut at
            // end-of-stream. Worth saying out loud, because from the outside it
            // is indistinguishable from a crossfade setting of zero.
            if !self.warned_no_duration {
                self.warned_no_duration = true;
                log::warn!(
                    "no transition can be scheduled for {:?}: neither the container nor the \
                     caller knows how long it is, so there is no end to schedule against. The \
                     next track will be cut in at the end rather than mixed. Pass \
                     LoadRequest.durationSeconds, or have the planner set \
                     TransitionPlan.outgoing_duration_seconds.",
                    current.info.title,
                );
            }
            return;
        }
        let need_s = fade_s + ARM_LEAD_MS as f64 / 1000.0;
        if audible_tail_s > need_s {
            return;
        }

        let request = self.pending_next.take().unwrap();
        let fade_frames = (fade_s * self.device_rate as f64) as u64;
        match Voice::open(
            &request,
            self.spatial_enabled && self.sound_mode == SoundMode::Enhanced,
            self.head_yaw,
            self.device_rate,
            self.playback_speed,
            self.skip_silence && self.sound_mode == SoundMode::Enhanced,
            true,
        ) {
            Ok(mut voice) => {
                voice.gain = 0.0; // silent until the fade lifts it
                voice.applied_gain = 0.0;
                // The match is measured after normalization, so the voice has to
                // know what normalization it will get before the correction is
                // worked out — a freshly opened voice is in `Off`.
                voice.normalization_mode = self.loudness_mode;
                voice.arm_transition_match(
                    self.current.as_ref(),
                    self.loudness_enabled && self.sound_mode == SoundMode::Enhanced,
                );
                let match_db = voice.match_db;
                if let Ok(mut nerd) = self.nerd.lock() {
                    nerd.transition_match_db = (match_db.abs() >= 0.05).then_some(match_db);
                }
                // Decode the cue now rather than at the fade: the arm is
                // seconds ahead, and this is the only cold decode in the path.
                prime_voice(&mut voice, self.device_rate);
                log::info!("armed next track: {}", request.title);
                self.incoming = Some(voice);
                self.transition = Some(TransitionState {
                    phase: Phase::Arming,
                    fade_frames,
                    handed_off: false,
                    plan,
                    arm_deadline: Instant::now() + Duration::from_millis(ARM_TIMEOUT_MS),
                    swap: false,
                    fx: None,
                    effect_start_frame: 0,
                    effect_input_gain: None,
                });
            }
            Err(e) => {
                // Upstream gives up and lets the queue move on plainly: a
                // missed crossfade rather than a broken one.
                log::warn!("arm failed for {}: {e}", request.title);
                self.events
                    .error(format!("could not prepare next track: {e}"));
                self.transition = None;
            }
        }
    }

    fn effective_fade_seconds(&self, total: f64) -> f64 {
        // A fade that swallows a third of a song stops being a transition —
        // upstream's `fadeFor` cap. Applied against the *current* track.
        let window = self.crossfade_window_s.max(0.0);
        if total > 0.0 {
            return window.min(total / 3.0);
        }
        window
    }

    /// A planned fade, kept off a half-second floor.
    ///
    /// Analysis of a partial file, or a grid snap onto the mix-out, can hand
    /// the mixer a fade of a few hundred milliseconds. On a track with room
    /// for four seconds, that figure is a cut and the floor replaces it. A
    /// plan that already asked for a real blend is left alone, including one
    /// that spends most of a short record — the planner chose that length.
    /// The window a plan needs: a technique's own length, in beats, or the
    /// floored blend length for a crossfade.
    fn plan_seconds(&self, plan: &TransitionPlan, track_seconds: f64) -> f64 {
        if plan.style.is_effect() {
            // The effect rides a real crossfade: its length is the planner's
            // blend, and the technique only fills the last sub-window.
            if plan.fade_seconds > 0.0 {
                return plan.fade_seconds;
            }
            return fx::window_seconds(plan.style, plan.beat_seconds)
                .unwrap_or_else(|| self.blend_seconds(plan.fade_seconds, track_seconds));
        }
        self.blend_seconds(plan.fade_seconds, track_seconds)
    }

    fn blend_seconds(&self, plan_fade: f64, track_seconds: f64) -> f64 {
        let planned = plan_fade.max(0.0);
        // A region-limited handoff must never be expanded across a skipped interval.
        let active = self.current.iter().chain(self.incoming.iter()).any(|v| {
            v.regions.audible_start_seconds > 0.0
                || v.regions.audible_end_seconds.is_some()
                || !v.regions.excluded.is_empty()
        });
        let pending = self
            .pending_next
            .as_ref()
            .map(|r| crate::playback_regions::get(&r.source))
            .unwrap_or_default();
        if active
            || pending.audible_start_seconds > 0.0
            || pending.audible_end_seconds.is_some()
            || !pending.excluded.is_empty()
        {
            return planned;
        }
        if planned == 0.0 || planned >= 4.0 {
            return planned;
        }
        if track_seconds > 0.0 && track_seconds < 12.0 {
            return planned;
        }
        4.0
    }

    /// Starts the fade the moment the audible tail has shrunk to it — or, for
    /// gapless (zero window), at the exact end.
    fn consider_start_fade(&mut self) {
        let Some(t) = &self.transition else {
            return;
        };
        if t.phase != Phase::Arming {
            return;
        }
        let Some(current) = &self.current else {
            return;
        };
        let Some(_incoming) = &self.incoming else {
            return;
        };
        if Instant::now() > t.arm_deadline {
            // Incoming never became ready; queue moves on plainly.
            log::warn!("arm timeout; dropping transition");
            self.incoming = None;
            self.transition = None;
            return;
        }
        let fade_s = t.fade_frames as f64 / self.device_rate as f64;
        // The blend starts when the *rendered* playhead crosses the plan, not
        // when the audible tail estimate does. The audible figure adds the ring
        // backlog, which moves with how full the device buffer is, so the same
        // plan started on a different beat every time. The next sample this
        // voice will emit is the one that has to land on the downbeat, and it
        // is checked once per chunk, so the error is at most 512 frames.
        let start_s = {
            let end_s = self.outgoing_end_s(&t.plan, current);
            if !end_s.is_finite() {
                return;
            }
            (end_s - fade_s).max(0.0)
        };
        if current.emitted_position_seconds() + 1e-4 < start_s {
            return;
        }
        self.start_fade();
    }

    fn start_fade(&mut self) {
        let Some(mut voice) = self.incoming.take() else {
            return;
        };
        // The blend is what the handoff stretch is for, so the stage goes live
        // here rather than at open. It holds this rate until the blend ends;
        // the glide home starts in `finish_transition`.
        if (voice.base_plan_rate - 1.0).abs() > 1e-4 {
            log::info!(
                "handoff stretch engaged: rate={:.4} ({:.0} cents, pitch preserved)",
                voice.base_plan_rate,
                1200.0 * (voice.base_plan_rate.ln() / 2f64.ln()).abs(),
            );
            voice.set_plan_rate(voice.base_plan_rate);
        }
        let info = voice.info.clone();
        log::info!("fade start + handoff: {}", info.title);
        self.duration_ms
            .store((info.duration_seconds * 1000.0) as u64, Ordering::Relaxed);
        // G's open point: a route or loudness-mode change between arm and
        // fade re-arms the match here, while the incoming is still silent, so
        // the first frame is never corrected after it is audible.
        voice.normalization_mode = self.loudness_mode;
        voice.arm_transition_match(
            self.current.as_ref(),
            self.loudness_enabled && self.sound_mode == SoundMode::Enhanced,
        );
        self.incoming = Some(voice);
        let device_rate = self.device_rate;
        if let Some(t) = &mut self.transition {
            t.phase = Phase::Fading;
            t.handed_off = true;
            t.fx = if t.plan.reason.starts_with("skip-mix:") {
                fx::DeckFx::new_skip_echo(device_rate, t.plan.beat_seconds)
            } else {
                fx::DeckFx::new(t.plan.style, device_rate, t.plan.beat_seconds)
            };
            if let Some(winner) = automix_plan_winner(&t.plan) {
                log::info!(
                    "automix: plan_won={winner} fade={:.2}s style={:?}",
                    t.fade_frames as f64 / device_rate as f64,
                    t.plan.style
                );
            }
            let transition_start = (t.plan.transition_end_seconds
                - t.fade_frames as f64 / device_rate as f64)
                .max(0.0);
            t.effect_start_frame = ((t.plan.effect_start_seconds - transition_start)
                * device_rate as f64)
                .max(0.0) as u64;
            if t.fx.is_some() {
                log::info!(
                    "transition technique {:?} ({:.2}s beat): {}",
                    t.plan.style,
                    t.plan.beat_seconds,
                    t.plan.reason
                );
            }
        }
        // F: arm the loop bridge on the outgoing at the blend start. The unit
        // starts `bridge_start_seconds` on the record's timeline; the served
        // position is the blend's clock, so the delay is in served frames.
        if let Some(t) = &self.transition {
            if t.plan.bridge_seconds > 0.0 && t.plan.bridge_repeats > 0 {
                if let Some(current) = &mut self.current {
                    let delay = ((t.plan.bridge_start_seconds
                        - current.emitted_position_seconds())
                    .max(0.0)
                        * device_rate as f64)
                        .round() as u64;
                    let unit = (t.plan.bridge_seconds * device_rate as f64).round() as u64;
                    current.set_bridge(delay, unit, t.plan.bridge_repeats);
                    log::info!(
                        "bridge: {:.2}s unit at {:.2}s, {} repeat(s), {} frames of delay",
                        t.plan.bridge_seconds,
                        t.plan.bridge_start_seconds,
                        t.plan.bridge_repeats,
                        delay
                    );
                }
            }
        }
        let db = self.incoming.as_ref().and_then(|v| v.loudness_db);
        self.publish_nerd(&info, db);
        // Playhead follows the incoming track from this moment — upstream
        // swaps the session player at handoff. Leaving it on the outgoing
        // voice published a time near that track's end under the new
        // duration, so the next song appeared to start in its outro.
        self.publish_audible_position();
        self.events.handoff(info);
    }

    /// Drives gains + filter ride for the fade in flight. Returns true when
    /// the transition finished this chunk.
    fn drive_fade(&mut self) -> bool {
        if !self
            .transition
            .as_ref()
            .is_some_and(|t| t.phase == Phase::Fading)
        {
            return false;
        }
        if let Some(t) = self.transition.as_ref().filter(|t| t.fx.is_some()) {
            // The effect rides the blend: the incoming rises across the whole
            // crossfade while the outgoing falls across the same span (an
            // equal-power pair, so the sum never bumps as the two records add
            // up). The effect only dresses the outgoing's last sub-window.
            let fade = t.fade_frames.max(1);
            let played = self
                .incoming
                .as_ref()
                .map(|voice| voice.emitted_dev_frames)
                .unwrap_or(0);
            let p = (played as f64 / fade as f64).clamp(0.0, 1.0);
            let hold = hold_fraction(&t.plan, fade as f64 / self.device_rate as f64);
            let (rise, fall) = effect_intro_gains(p, hold, effect_intro_db(&t.plan));
            let landing = effect_landing(&t.plan, hold);
            let band = t.plan.filter_sweep;
            if let Some(current) = &mut self.current {
                current.gain = fall;
                current.filter.open();
            }
            if let Some(incoming) = &mut self.incoming {
                incoming.gain = rise;
                ride_effect_intro(p, hold, landing, band, &mut incoming.filter);
            }
            return played >= fade;
        }
        let Some(incoming) = &self.incoming else {
            return false;
        };
        let incoming_cap_frames = incoming
            .audible_remaining_seconds()
            .map(|remaining| {
                let played = incoming.emitted_dev_frames as f64;
                let total_from_cue = played + remaining * self.device_rate as f64;
                ((total_from_cue / 3.0) as u64).max(1)
            })
            .unwrap_or(u64::MAX);
        let emitted = incoming.emitted_dev_frames;
        let fade_frames = self.transition.as_ref().map(|t| t.fade_frames).unwrap_or(0);
        let span = if fade_frames == 0 {
            0
        } else {
            let capped = fade_frames.min(incoming_cap_frames).max(1);
            // The third-of-the-record cap is for a genuinely short song. A cap
            // under a second, against a plan that asked for more, is a partial
            // container duration — and using it is how a blend collapses into
            // a half-second cut.
            if fade_frames > self.device_rate as u64 && capped < self.device_rate as u64 {
                fade_frames
            } else {
                capped
            }
        };
        let p = if span == 0 {
            1.0
        } else {
            (emitted as f64 / span as f64).clamp(0.0, 1.0)
        };
        let plan = self.transition.as_ref().map(|t| (t.plan.clone(), t.swap));
        let swap = plan.as_ref().is_some_and(|(_, swap)| *swap);
        let span_seconds = span as f64 / self.device_rate as f64;
        let hold = plan
            .as_ref()
            .map(|(plan, _)| hold_fraction(plan, span_seconds))
            .unwrap_or(0.0);
        let dj = plan.as_ref().is_some_and(|(plan, _)| {
            !swap
                && plan.bed_fraction > 0.0
                && matches!(
                    plan.style,
                    TransitionStyle::DjBlend | TransitionStyle::DjFilter
                )
        });
        // A swap needs the linear pair. A plain crossfade needs equal-power.
        // A DJ blend holds the outgoing record up and brings the next one in
        // underneath. A cold ending holds the outgoing until its final chord
        // and crosses over the decay. The tempo stretch is *held* here —
        // walking it during the fade is how two matched grids drift into a
        // flam. It glides home after the blend, on the promoted voice.
        let (rise, fall) = if swap {
            fade_gains(p, true)
        } else if dj {
            automix_gains(p, &plan.as_ref().unwrap().0, hold)
        } else if hold > 0.0 {
            cold_end_gains(p, hold)
        } else {
            fade_gains(p, false)
        };
        if let (Some(current), Some(incoming), Some((plan, _))) =
            (&mut self.current, &mut self.incoming, plan)
        {
            current.gain = fall;
            incoming.gain = rise;
            if swap {
                // A source swap is the same recording against itself, so any
                // ride here would be an edit the record never asked for.
                current.filter.open();
                incoming.filter.open();
            } else {
                ride_filters(p, &plan, &mut current.filter, &mut incoming.filter);
            }
        }
        // The outgoing decoder can hit EOF before a late/stale plan reaches
        // its end. That must not finish the transition: doing so promotes the
        // incoming voice at its current gain and makes the outgoing song sound
        // like it was cut mid-fade. Let the incoming ramp reach unity; only
        // abandon it early if the incoming source itself is fully drained.
        let incoming_drained = self.incoming.as_ref().is_some_and(|voice| {
            voice.finished && voice.pending_dev_cursor >= voice.pending_dev.len()
        });
        p >= 1.0 || incoming_drained
    }

    fn finish_transition(&mut self) {
        let swap = self.transition.as_ref().is_some_and(|t| t.swap);
        if let Some(t) = &self.transition {
            if t.handed_off {
                if swap {
                    // The one measurement that can only come from a device:
                    // how alike two encodings of one master are. Recorded
                    // rather than asserted, because it is data about the
                    // catalogue, not a property of this code.
                    let correlation = self.swap_probe.correlation();
                    match correlation {
                        Some(rho) => log::info!(
                            "source swap correlation: rho={rho:.4} \
                             (linear-fade deviation from flat: {:.2} dB)",
                            10.0 * ((1.0 + rho) / 2.0).log10()
                        ),
                        None => log::info!(
                            "source swap correlation: unmeasurable (no signal overlapped)"
                        ),
                    }
                    if let Ok(mut nerd) = self.nerd.lock() {
                        nerd.swap_correlation = correlation;
                    }
                    // Condition in-place source swap on Pearson correlation rho >= 0.85
                    // so mismatched recordings/transcodes do not cut over.
                    if let Some(rho) = correlation {
                        if rho < 0.85 {
                            log::warn!(
                                "source swap rejected: rho={rho:.4} < 0.85 \
                                 (mismatched recording or transcode)"
                            );
                            self.incoming = None;
                            if let Some(current) = &mut self.current {
                                current.gain = 1.0;
                                current.filter.open();
                            }
                            self.transition = None;
                            return;
                        }
                    }
                }
                if let Some(mut incoming) = self.incoming.take() {
                    incoming.gain = 1.0;
                    incoming.release_transition_match();
                    incoming.filter.open();
                    // Hold the matched tempo through the blend, then walk it
                    // home. A swap has nothing to walk, and a plan with no
                    // glide releases here so the stretch cannot outlive the mix.
                    let glide = !swap
                        && incoming.post_glide_seconds > 0.05
                        && (incoming.base_plan_rate - 1.0).abs() > 1e-4;
                    if glide {
                        incoming.begin_tempo_glide(self.device_rate);
                    } else {
                        incoming.release_plan_stretch(self.playback_speed, self.device_rate);
                    }
                    let info = incoming.info.clone();
                    self.current = Some(incoming);
                    if swap {
                        // The readout should name the source that is actually
                        // playing, and that only becomes true here.
                        let db = self.current.as_ref().and_then(|v| v.loudness_db);
                        self.publish_nerd(&info, db);
                    }
                }
                // Old current dropped; its tail is spent by definition of the
                // fade end.
            } else {
                // Never audible; drop the incoming scratch voice.
                self.incoming = None;
                if let Some(current) = &mut self.current {
                    current.gain = 1.0;
                }
            }
        }
        self.transition = None;
    }

    /// 120 ms ramp-away for an interrupted transition — no click.
    fn bail(&mut self) {
        let Some(t) = &self.transition else {
            return;
        };
        if !t.handed_off {
            // Nothing was ever audible; no ramp to run.
            self.incoming = None;
            self.transition = None;
            if let Some(current) = &mut self.current {
                current.gain = 1.0;
                current.filter.open();
            }
            return;
        }
        // Open glided (never snapped): the incoming track is audible and its
        // low end may be mid-handover.
        if let Some(incoming) = &mut self.incoming {
            incoming.filter.open();
            incoming.gain = 1.0;
            // A seek abandoned the blend. The glide is for a mix that finished,
            // not for one that was interrupted.
            incoming.release_plan_stretch(self.playback_speed, self.device_rate);
        }
        if let Some(mut current) = self.current.take() {
            let from_gain = current.gain;
            current.filter.open();
            self.retiring.push(RetiringVoice {
                voice: current,
                from_gain,
                started_at: Instant::now(),
            });
        }
        self.transition = None;
    }

    fn stop_all(&mut self) {
        self.hard_cut();
        self.playing = false;
        self.set_state(crate::PlaybackState::Stopped);
    }

    /// Playhead the listener hears: decoder time minus samples still in the
    /// output ring (not yet consumed by the device callback). After handoff
    /// this is the incoming track — the session has already moved on.
    fn publish_audible_position(&mut self) {
        let handed_off = self.transition.as_ref().is_some_and(|t| t.handed_off);
        let swap = self.transition.as_ref().is_some_and(|t| t.swap);
        let Some(voice) = (if handed_off {
            self.incoming.as_ref().or(self.current.as_ref())
        } else {
            self.current.as_ref()
        }) else {
            return;
        };
        let buffered_s =
            self.buffered_frames.load(Ordering::Relaxed) as f64 / self.device_rate.max(1) as f64;
        // Incoming has not filled the ring yet; subtracting the outgoing
        // backlog would clamp a just-cued track to 0 every time. Only the
        // current (session) voice's own backlog counts.
        //
        // A source swap is the exception: it is the same track at the same
        // instant, so the ring backlog still belongs to the timeline and comes
        // off exactly as it does mid-track. Publishing the replacement's
        // decoder head instead would throw the playhead forward by the whole
        // ring depth.
        let audible = if handed_off && !swap {
            voice.emitted_position_seconds().max(0.0)
        } else {
            (voice.emitted_position_seconds()
                - buffered_s * voice.effective_speed as f64 * voice.plan_rate as f64)
                .max(0.0)
        };
        let processing_delay = if self.sound_mode == SoundMode::Enhanced && !self.bit_perfect {
            (self.clarity.delay_frames() as usize + self.protector.delay_frames()) as f64
                / self.device_rate.max(1) as f64
                * voice.effective_speed as f64
                * voice.plan_rate as f64
        } else {
            0.0
        };
        self.position_ms.store(
            ((audible - processing_delay).max(0.0) * 1000.0) as u64,
            Ordering::Relaxed,
        );
    }
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
        // New profiles default to Off; the owner restores any legacy setting.
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

fn handle_command(state: &mut MixerState, cmd: Command, ring: &mut Producer<f32>) {
    match cmd {
        Command::ForSource { source, command } => {
            let handed_off = state.transition.as_ref().is_some_and(|t| t.handed_off);
            let voice = if handed_off {
                state.incoming.as_ref().or(state.current.as_ref())
            } else {
                state.current.as_ref()
            };
            if voice.is_some_and(|voice| voice.info.source == source) {
                handle_command(state, *command, ring);
            } else if let Command::SwapSource { reply, .. } = *command {
                let _ = reply.send(Err("source swap belongs to a superseded track".into()));
            }
        }
        Command::Load { request, reply } => {
            // Upstream: a skip/replace is not a blend. `onSkipRequested` bails
            // and `setMediaItems` replaces the session playlist; the spare is
            // `stop()`+`clearMediaItems()`. Mixing the old voice under the new
            // one reads as the tap being ignored.
            state.hard_cut();
            match Voice::open(
                &request,
                state.spatial_enabled && state.sound_mode == SoundMode::Enhanced,
                state.head_yaw,
                state.device_rate,
                state.playback_speed,
                state.skip_silence && state.sound_mode == SoundMode::Enhanced,
                true,
            ) {
                Ok(mut voice) => {
                    voice.bit_perfect = state.bit_perfect
                        && crate::is_lossless_pcm_codec(&voice.info.codec)
                        && voice.info.sample_rate == state.device_rate
                        && matches!(voice.info.bit_depth, 16 | 24)
                        && voice.info.channels == 2;
                    let info = voice.info.clone();
                    let duration = voice.info.duration_seconds;
                    let (_, gain_db) = loudness_gain(
                        voice.loudness_db,
                        state.loudness_enabled && state.sound_mode == SoundMode::Enhanced,
                    );
                    if let Ok(mut nerd) = state.nerd.lock() {
                        *nerd = NerdSnapshot {
                            codec: info.codec.clone(),
                            sample_rate: info.sample_rate,
                            bit_depth: info.bit_depth,
                            channels: info.channels,
                            kbps: info.kbps,
                            loudness_gain_db: gain_db,
                            // A new track clears the previous swap's figure.
                            swap_correlation: None,
                            ..NerdSnapshot::default()
                        };
                    }
                    state.current = Some(voice);
                    state.playing = true;
                    state.position_ms.store(0, Ordering::Relaxed);
                    state
                        .duration_ms
                        .store((duration * 1000.0) as u64, Ordering::Relaxed);
                    state.set_state(crate::PlaybackState::Playing);
                    if duration > 0.0 {
                        state.events.duration_changed(duration);
                    }
                    let _ = reply.send(Ok(info));
                }
                Err(e) => {
                    state.set_state(crate::PlaybackState::Stopped);
                    let _ = reply.send(Err(e));
                }
            }
        }
        Command::SwapSource {
            request,
            crossfade_seconds,
            reply,
        } => match state.swap_source(request, crossfade_seconds) {
            Ok(info) => {
                let _ = reply.send(Ok(info));
            }
            Err(e) => {
                let _ = reply.send(Err(e));
            }
        },
        Command::QueueNext { mut request } => {
            if let Some(current) = &state.current {
                request.plan = crate::playback_regions::constrain(
                    request.plan.into(),
                    current.regions.clone(),
                    crate::playback_regions::get(&request.source),
                    current.info.duration_seconds,
                    request.duration_seconds,
                )
                .into();
                request.start_seconds = request.plan.cue_seconds;
            }
            if state.bit_perfect || state.hold_end {
                return;
            }
            if request.source.is_empty() {
                state.pending_next = None;
                if state
                    .transition
                    .as_ref()
                    .is_some_and(|t| t.phase == Phase::Arming && !t.swap)
                {
                    state.transition = None;
                    state.incoming = None;
                }
            } else if state
                .current
                .as_ref()
                .is_some_and(|c| c.info.source == request.source)
            {
                // Same path as the playing voice is normally a duplicate and is
                // dropped. A planned fade is the repeat-one / single-item
                // repeat-all self-mix: open a second reader on the same file and
                // blend the track into itself.
                if request.plan.fade_seconds > 0.0 {
                    log::info!(
                        "queued self-mix for {} fade={:.1}s",
                        request.title,
                        request.plan.fade_seconds
                    );
                    state.pending_next = Some(request);
                } else {
                    log::debug!("queueNext ignored; already current");
                }
            } else if state
                .incoming
                .as_ref()
                .is_some_and(|c| c.info.source == request.source)
            {
                // The early overlap can arm before analysis finishes. Replacing
                // it once the fade has started would replay this song after the
                // handoff; replacing the plan while it is still waiting moves
                // the blend to where the analysis said it should be.
                state.upgrade_armed_plan(request);
            } else if state
                .transition
                .as_ref()
                .is_some_and(|transition| transition.phase == Phase::Arming && !transition.swap)
            {
                // Not audible yet. A better encode of the incoming record, or a
                // queue change, replaces the armed voice. Doing this once the
                // fade is sounding would cut the blend.
                state.retarget_armed_source(request);
            } else if state.incoming.as_ref().is_some_and(|incoming| {
                incoming.info.title == request.title && incoming.info.artist == request.artist
            }) {
                log::info!("held a same-song source change; the blend is already audible");
            } else {
                state.pending_next = Some(request);
            }
        }
        Command::PendingTrack { reply } => {
            let queued = state
                .pending_next
                .as_ref()
                .map(|request| QueuedTrack {
                    title: request.title.clone(),
                    artist: request.artist.clone(),
                    source: request.source.clone(),
                })
                .or_else(|| {
                    state.incoming.as_ref().map(|voice| QueuedTrack {
                        title: voice.info.title.clone(),
                        artist: voice.info.artist.clone(),
                        source: voice.info.source.clone(),
                    })
                });
            let _ = reply.send(queued);
        }
        Command::SkipToPending {
            skip_mix_enabled,
            reply,
        } => {
            // A blend already sounding completes now: the listener asked for
            // its target, not for the rest of the fade.
            if state.incoming.is_some() {
                let info = state.incoming.as_ref().map(|voice| voice.info.clone());
                let arming_skip = skip_mix_enabled
                    && state.transition.as_ref().is_some_and(|transition| {
                        accepts_analyzed_plan(transition.phase, transition.swap)
                    });
                let skip_mix = if arming_skip {
                    state.begin_skip_mix()
                } else {
                    None
                };
                if let Some(info) = skip_mix {
                    state.consider_start_fade();
                    let _ = reply.send(Ok(info));
                } else {
                    state.promote_incoming();
                    state.position_ms.store(0, Ordering::Relaxed);
                    if !state.playing {
                        state.playing = true;
                        state.set_state(crate::PlaybackState::Playing);
                    }
                    match info {
                        Some(info) => {
                            log::info!("skip promoted blending {}", info.title);
                            let _ = reply.send(Ok(info));
                        }
                        None => {
                            let _ = reply.send(Err("nothing to skip to".into()));
                        }
                    }
                }
            } else if let Some(request) = state.pending_next.take() {
                let title = request.title.clone();
                match Voice::open(
                    &request,
                    state.spatial_enabled && state.sound_mode == SoundMode::Enhanced,
                    state.head_yaw,
                    state.device_rate,
                    state.playback_speed,
                    state.skip_silence && state.sound_mode == SoundMode::Enhanced,
                    true,
                ) {
                    Ok(mut voice) => {
                        // Same landing as a Load: ramp out the backlog, drop
                        // every straggler, start the new voice at the top.
                        state.hard_cut();
                        voice.gain = 1.0;
                        voice.release_plan_stretch(state.playback_speed, state.device_rate);
                        let info = voice.info.clone();
                        let duration = info.duration_seconds;
                        let (_, gain_db) = loudness_gain(
                            voice.loudness_db,
                            state.loudness_enabled && state.sound_mode == SoundMode::Enhanced,
                        );
                        if let Ok(mut nerd) = state.nerd.lock() {
                            *nerd = NerdSnapshot {
                                codec: info.codec.clone(),
                                sample_rate: info.sample_rate,
                                bit_depth: info.bit_depth,
                                channels: info.channels,
                                kbps: info.kbps,
                                loudness_gain_db: gain_db,
                                swap_correlation: None,
                                ..NerdSnapshot::default()
                            };
                        }
                        state.current = Some(voice);
                        state.playing = true;
                        state.set_state(crate::PlaybackState::Playing);
                        state.position_ms.store(0, Ordering::Relaxed);
                        state
                            .duration_ms
                            .store((duration * 1000.0) as u64, Ordering::Relaxed);
                        if duration > 0.0 {
                            state.events.duration_changed(duration);
                        }
                        state.publish_audible_position();
                        state.events.handoff(info.clone());
                        log::info!("skip promoted queued {}", info.title);
                        let _ = reply.send(Ok(info));
                    }
                    Err(error) => {
                        // The track is not lost: put the request back so the
                        // planned blend (or a retry) still finds it.
                        state.pending_next = Some(request);
                        log::warn!("skip could not open {title}: {error}");
                        let _ = reply.send(Err(error));
                    }
                }
            } else {
                let _ = reply.send(Err("nothing queued to skip to".into()));
            }
        }
        Command::Play => {
            if state.current.is_some() {
                state.playing = true;
                state.set_state(crate::PlaybackState::Playing);
            }
        }
        Command::Pause => {
            // Silence the DAC immediately (output_paused) but keep the
            // already-mixed ring: those samples are the playhead. Flushing
            // them made pause snappy and resume skip ahead by ~2 s.
            state.publish_audible_position();
            state.playing = false;
            state.set_state(crate::PlaybackState::Paused);
        }
        Command::Stop => {
            state.stop_all();
        }
        Command::Seek { seconds } => {
            if state.transition.is_some() {
                state.bail();
            }
            match &mut state.current {
                Some(voice) => {
                    let r = voice.decoder.seek_seconds(seconds);
                    match r {
                        Ok(()) => {
                            voice.base_position = 0.0;
                            voice.reset_processing();
                            voice.head_trim_allowed = false;
                            for (i, s) in voice.regions.excluded.iter().enumerate() {
                                if seconds >= s.start_seconds && seconds < s.end_seconds {
                                    voice.skipped_regions[i] = true;
                                }
                            }
                            state.eq.flush();
                            state.clarity.reset();
                            state.protector.reset();
                            state.pipeline_tail_frames = 0;
                            state.flush_ring.store(true, Ordering::Release);
                            state.buffered_frames.store(0, Ordering::Relaxed);
                            state
                                .position_ms
                                .store((seconds * 1000.0) as u64, Ordering::Relaxed);
                        }
                        Err(e) => {
                            state.events.error(format!("seek failed: {e}"));
                        }
                    }
                }
                None => {
                    state.events.error("seek ignored — nothing playing".into());
                }
            }
        }
        Command::SetPlaybackRegions { source, regions } => {
            for voice in state.current.iter_mut().chain(state.incoming.iter_mut()) {
                if voice.info.source == source {
                    let regions = regions.clone().normalized(voice.info.duration_seconds);
                    voice.skipped_regions = regions
                        .excluded
                        .iter()
                        .map(|s| {
                            voice
                                .regions
                                .excluded
                                .iter()
                                .position(|old| {
                                    old.start_seconds == s.start_seconds
                                        && old.end_seconds == s.end_seconds
                                })
                                .map(|i| voice.skipped_regions[i])
                                .unwrap_or(false)
                        })
                        .collect();
                    voice.regions = regions;
                }
            }
            if let (Some(current), Some(next)) = (&state.current, &mut state.pending_next) {
                next.plan = crate::playback_regions::constrain(
                    next.plan.clone().into(),
                    current.regions.clone(),
                    crate::playback_regions::get(&next.source),
                    current.info.duration_seconds,
                    next.duration_seconds,
                )
                .into();
                next.start_seconds = next.plan.cue_seconds;
            }
            if let (Some(current), Some(incoming), Some(transition)) =
                (&state.current, &mut state.incoming, &mut state.transition)
            {
                if !transition.swap && transition.phase == Phase::Arming {
                    transition.plan = crate::playback_regions::constrain(
                        transition.plan.clone().into(),
                        current.regions.clone(),
                        incoming.regions.clone(),
                        current.info.duration_seconds,
                        incoming.info.duration_seconds,
                    )
                    .into();
                    transition.fade_frames =
                        (transition.plan.fade_seconds * state.device_rate as f64) as u64;
                    let cue = incoming.regions.next_allowed(transition.plan.cue_seconds);
                    if incoming.decoder.seek_seconds(cue).is_ok() {
                        incoming.reset_processing();
                    }
                }
            }
        }
        Command::SetSleepGain(gain) => {
            state.sleep_gain = if gain.is_finite() {
                gain.clamp(0.0, 1.0)
            } else {
                1.0
            }
        }
        Command::HoldTrackEnd(hold) => {
            state.hold_end = hold;
            if hold {
                if state.transition.as_ref().is_some_and(|t| t.handed_off) {
                    state.current = state.incoming.take();
                    if let Some(voice) = &mut state.current {
                        voice.release_transition_match();
                    }
                }
                state.pending_next = None;
                state.incoming = None;
                state.transition = None;
                if let Some(voice) = &mut state.current {
                    voice.gain = 1.0;
                }
            }
        }
        Command::SetVolume(v) => state.volume = v.clamp(0.0, 1.0),
        Command::SetBitPerfect(enabled) => {
            state.bit_perfect = enabled;
            if enabled {
                state.transition = None;
                state.incoming = None;
                state.pending_next = None;
                state.retiring.clear();
                state.bail_flush.store(false, Ordering::Release);
                state.flush_ring.store(true, Ordering::Release);
            }
            if let Some(current) = &mut state.current {
                current.bit_perfect = enabled;
            }
        }
        Command::SetCrossfadeWindow(s) => state.crossfade_window_s = s.max(0.0),
        Command::SetSpatial(enabled) => {
            state.spatial_enabled = enabled;
            if let Some(current) = &mut state.current {
                current
                    .spatial
                    .set_enabled(enabled && state.sound_mode == SoundMode::Enhanced);
            }
            if let Some(incoming) = &mut state.incoming {
                incoming
                    .spatial
                    .set_enabled(enabled && state.sound_mode == SoundMode::Enhanced);
            }
        }
        Command::SetHeadYaw(yaw) => {
            state.head_yaw = yaw;
            if let Some(current) = &mut state.current {
                current.spatial.set_head_yaw(yaw);
            }
            if let Some(incoming) = &mut state.incoming {
                incoming.spatial.set_head_yaw(yaw);
            }
        }
        Command::SetVoiceFilter {
            incoming,
            low_hz,
            high_hz,
        } => {
            let target = if incoming {
                state.incoming.as_mut()
            } else {
                state.current.as_mut()
            };
            if let Some(voice) = target {
                voice.filter.set_cutoffs(low_hz, high_hz);
            }
        }
        Command::OpenFilters => {
            if let Some(current) = &mut state.current {
                current.filter.open();
            }
            if let Some(incoming) = &mut state.incoming {
                incoming.filter.open();
            }
        }
        Command::SetPlaybackSpeed(speed) => {
            state.playback_speed = speed.clamp(0.5, 2.0);
            if let Some(v) = &mut state.current {
                v.set_playback_speed(state.playback_speed, state.device_rate);
            }
            if let Some(v) = &mut state.incoming {
                v.set_playback_speed(state.playback_speed, state.device_rate);
            }
        }
        Command::SetSkipSilence(enabled) => {
            state.skip_silence = enabled;
            if let Some(v) = &mut state.current {
                v.skip_silence = enabled && state.sound_mode == SoundMode::Enhanced;
            }
            if let Some(v) = &mut state.incoming {
                v.skip_silence = enabled && state.sound_mode == SoundMode::Enhanced;
            }
        }
        Command::ObserveUnderruns(counter) => {
            state.observed_underruns = counter.load(Ordering::Relaxed);
            state.underrun_counter = Some(counter);
        }
        Command::SetDecodeChunk(frames) => crate::diagnostics::set_chunk(frames),
        Command::SetPreferAppleAac(value) => crate::diagnostics::set_prefer_apple_aac(value),
        Command::StartDiagnosticCapture { directory, seconds } => {
            if let Err(e) = crate::diagnostics::start(directory, seconds, state.device_rate) {
                state.events.error(e);
            } else if let Some(v) = state.current.as_ref() {
                crate::diagnostics::source(&v.info.source, v.info.sample_rate);
            }
        }
        Command::FinishDiagnosticCapture { reply } => {
            let settings = state.diagnostic_settings();
            let snapshot = state.nerd.lock().unwrap().clone();
            let events = state.events.clone();
            crate::diagnostics::finish(snapshot, settings, move |result| {
                if let Err(e) = &result {
                    events.error(e.clone());
                }
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            });
        }
        Command::SetSoundMode(mode) => {
            state.sound_mode = mode;
            state.clarity.set(mode, state.clarity_tuning.clone());
            state.protector.reset();
            state.eq.flush();
            state.pipeline_tail_frames = 0;
            if let Some(v) = state.current.as_mut() {
                let position = state.position_ms.load(Ordering::Relaxed) as f64 / 1000.0;
                if let Err(e) = v.decoder.seek_seconds(position) {
                    state.events.error(format!("sound mode seek: {e}"));
                }
                v.reset_processing();
                state.bail_flush.store(true, Ordering::Release);
            }
            for voice in state.current.iter_mut().chain(state.incoming.iter_mut()) {
                if mode == SoundMode::Transparent {
                    voice.smoothed_normalization = 1.0;
                }
                voice.spatial.set_enabled(
                    mode == SoundMode::Enhanced
                        && state.spatial_enabled
                        && voice.info.channels >= 2,
                );
                voice.skip_silence = mode == SoundMode::Enhanced && state.skip_silence;
            }
        }
        Command::SetClarityTuning(tuning) => {
            state.clarity_tuning = tuning.clone();
            state.clarity.set(state.sound_mode, tuning);
        }
        Command::SetLoudnessMeasurement(m) => {
            if let Some(v) = state.current.as_mut() {
                v.measured_loudness = Some(m);
            }
        }
        Command::SetLoudnessMode(mode) => {
            state.loudness_mode = mode;
            state.loudness_enabled = mode != LoudnessMode::Off;
        }
        Command::SetLoudnessEnabled(enabled) => {
            state.loudness_enabled = enabled;
            state.loudness_mode = if enabled {
                LoudnessMode::Track
            } else {
                LoudnessMode::Off
            };
            // The readout follows the switch without a reload: the gain each
            // voice *would* apply is recomputed for the one that is audible.
            if let Some(current) = &state.current {
                let info = current.info.clone();
                let db = current.loudness_db;
                state.publish_nerd(&info, db);
            }
        }
        Command::SetEqTuning {
            enabled,
            gains_db,
            qs,
            preamp_db,
            balance,
        } => {
            state.eq.set_tuning(
                enabled,
                &EqCurve {
                    gains_db,
                    qs,
                    preamp_db,
                },
                balance,
            );
        }
        Command::SetOutputFormat {
            rate,
            producer,
            generation,
            acknowledged,
        } => {
            if generation < state.output_generation {
                if let Some(reply) = acknowledged {
                    let _ = reply.send(false);
                }
                return;
            }
            state.output_generation = generation;

            *ring = producer;
            let rate = rate.max(1);
            if rate != state.device_rate {
                log::info!("output format {} Hz -> {rate} Hz", state.device_rate);
                state.device_rate = rate;
                state.eq.retarget(rate, 2);
                state.clarity.retarget(rate);
                state.protector = Protector::new(rate);
                let spatial = state.spatial_enabled;
                let yaw = state.head_yaw;
                if let Some(v) = &mut state.current {
                    v.retarget_device(rate, spatial, yaw);
                }
                if let Some(v) = &mut state.incoming {
                    v.retarget_device(rate, spatial, yaw);
                }
                for ret in &mut state.retiring {
                    ret.voice.retarget_device(rate, spatial, yaw);
                }
            }
            // The ring we just abandoned held everything between the playhead
            // and the decoder's read head, and it is gone — nothing drained it,
            // because the unit it fed is the one being replaced. Re-point the
            // voice at the playhead so the song continues where the listener
            // was, instead of silently skipping the whole ring depth, which is
            // seconds of music, the moment the output route changes.
            let resume_s = state.position_ms.load(Ordering::Relaxed) as f64 / 1000.0;
            // Zero is a valid playhead: a muted freshly loaded voice may
            // already have decoded the entire short track into the old ring.
            if let Some(voice) = &mut state.current {
                match voice.decoder.seek_seconds(resume_s) {
                    Ok(()) => {
                        voice.base_position = 0.0;
                        voice.reset_processing();
                        state.eq.flush();
                        state.clarity.reset();
                        state.protector.reset();
                        state.pipeline_tail_frames = 0;
                        log::info!("output changed; resuming at {resume_s:.3}s");
                    }
                    Err(e) => log::warn!("re-seek after output change failed: {e}"),
                }
            }
            state.flush_ring.store(true, Ordering::Release);
            state.buffered_frames.store(0, Ordering::Relaxed);
            if let Some(reply) = acknowledged {
                let _ = reply.send(true);
            }
        }
        Command::Shutdown => {
            state.stop_all();
        }
    }
    if crate::diagnostics::enabled() {
        crate::diagnostics::record_settings(state.diagnostic_settings());
    }
}

/// Renders while the ring has room for a chunk.
fn render_available(state: &mut MixerState, ring: &mut Producer<f32>) {
    if state.underrun_counter.is_some()
        && (state.flush_ring.load(Ordering::Acquire) || state.bail_flush.load(Ordering::Acquire))
    {
        return;
    }
    if state.bit_perfect {
        render_bitperfect_available(state, ring);
        return;
    }
    for voice in state.current.iter_mut().chain(state.incoming.iter_mut()) {
        voice.normalization_mode = state.loudness_mode;
    }
    let chunk_samples = CHUNK_FRAMES * 2;
    loop {
        // Maintenance, evaluated per chunk rather than once per top-up.
        //
        // These decisions are about where the *audible* playhead is — arm the
        // incoming, start the fade — and this loop renders until the ring is
        // full, which is however much the device callback has drained since the
        // previous pass. While the callback keeps up, that is a handful of
        // chunks and checking once was equivalent. When it does not — a cold
        // ring, a slow decoder, a harness draining as fast as it can — a single
        // pass can render the whole fade window, or the whole track, before the
        // check runs again, and the blend lands wherever the ring ran out
        // instead of where the plan said.
        //
        // Upstream evaluates this on a 30 ms timer (`CrossfadeController.tick`),
        // which is the shape matched here: a cadence tied to the playhead, not
        // to buffer occupancy.
        state.consider_arm();
        state.consider_start_fade();
        if state.buffered_frames.load(Ordering::Relaxed)
            >= state.device_rate as u64 * state.queue_target_ms / 1000
        {
            break;
        }
        let free = ring.slots();
        if free < chunk_samples {
            break;
        }
        let mut scratch = vec![0.0f32; chunk_samples];
        let mut audible = false;
        // Samples actually rendered this pass — zero padding beyond this is
        // NOT audio and must not enter the ring (it would pin `buffered` and
        // stall every tail computation after the current track ends).
        let mut produced = 0usize;

        // Fade drive.
        let transition_done = state.drive_fade();
        if transition_done {
            state.finish_transition();
        }

        // Gapless fallback: current ended with an incoming already armed, or
        // a pending next that never got a fade window (unknown duration).
        let current_finished = state
            .current
            .as_ref()
            .is_some_and(|c| c.finished && c.pending_dev_cursor >= c.pending_dev.len());
        if current_finished && state.transition.is_none() {
            if state.incoming.is_some() {
                state.promote_incoming();
            } else if state.pending_next.is_some() {
                state.promote_pending();
            }
        }

        // Retiring voices (bail/skip ramps) keep decoding while they fade.
        let mut keep_retiring = Vec::new();
        while let Some(mut ret) = state.retiring.pop() {
            let elapsed = ret.started_at.elapsed().as_millis() as f64 / BAIL_MS as f64;
            if elapsed < 1.0 {
                let frames = ret.voice.pull(CHUNK_FRAMES, state.device_rate);
                if !frames.is_empty() {
                    audible = true;
                    produced = produced.max(frames.len());
                    ret.voice.mix_into(
                        &frames,
                        &mut scratch,
                        ret.from_gain * fall_gain(elapsed),
                        state.loudness_enabled && state.sound_mode == SoundMode::Enhanced,
                    );
                }
                keep_retiring.push(ret);
            }
        }
        state.retiring = keep_retiring;

        // A swap fades a recording into another copy of itself, so the one
        // number that decides whether the curve is right is how correlated the
        // two copies are, and only a real upgrade can supply it. The samples
        // are already decoded and aligned here, so the probe is nearly free.
        let swap_fade = state.transition.as_ref().is_some_and(|t| t.swap);
        if let Some(voice) = &mut state.current {
            if state.playing {
                let rate = state.device_rate;
                let speed = state.playback_speed;
                voice.drive_tempo_glide(rate, speed);
                let frames = voice.pull(CHUNK_FRAMES, rate);
                voice.note_glide_frames((frames.len() / 2) as u64);
                let effect_active = state.transition.as_ref().is_some_and(|t| {
                    t.phase == Phase::Fading
                        && t.fx.is_some()
                        && state
                            .incoming
                            .as_ref()
                            .is_some_and(|voice| voice.emitted_dev_frames >= t.effect_start_frame)
                });
                let effect = if effect_active {
                    state
                        .transition
                        .as_mut()
                        .filter(|t| t.phase == Phase::Fading)
                        .and_then(|t| {
                            let gain = *t.effect_input_gain.get_or_insert(voice.gain);
                            t.fx.as_mut().map(|fx| (fx, gain))
                        })
                } else {
                    None
                };
                if let Some((effect, input_gain)) = effect {
                    // The outgoing deck runs through its effect, and keeps
                    // running after its file ends: the tail is the point.
                    let mut dry = vec![0.0f32; frames.len()];
                    voice.mix_into(
                        &frames,
                        &mut dry,
                        input_gain,
                        state.loudness_enabled && state.sound_mode == SoundMode::Enhanced,
                    );
                    effect.render(&dry, &mut scratch, CHUNK_FRAMES);
                    audible = true;
                    produced = produced.max(chunk_samples);
                } else if !frames.is_empty() {
                    audible = true;
                    produced = produced.max(frames.len());
                    if swap_fade {
                        state.swap_probe.observe_current(&frames);
                    }
                    voice.mix_into(
                        &frames,
                        &mut scratch,
                        voice.gain,
                        state.loudness_enabled && state.sound_mode == SoundMode::Enhanced,
                    );
                }
            }
        }
        for voice in state.current.iter_mut().chain(state.incoming.iter_mut()) {
            if let Some(error) = voice.decode_error.take() {
                state.events.error(error);
            }
        }
        if state.playing {
            state.publish_audible_position();
        }

        // The incoming rises across the whole blend (the effect only dresses
        // the outgoing), so it is pulled from the first frame.
        let incoming_renders = state
            .transition
            .as_ref()
            .is_some_and(|t| t.phase == Phase::Fading);
        if let Some(voice) = &mut state.incoming {
            if state.playing && incoming_renders {
                let frames = voice.pull(CHUNK_FRAMES, state.device_rate);
                if !frames.is_empty() {
                    audible = true;
                    produced = produced.max(frames.len());
                    if swap_fade {
                        state.swap_probe.observe_incoming(&frames);
                    }
                    voice.mix_into(
                        &frames,
                        &mut scratch,
                        voice.gain,
                        state.loudness_enabled && state.sound_mode == SoundMode::Enhanced,
                    );
                }
            }
        }

        let draining_pipeline = produced == 0 && state.pipeline_tail_frames > 0;
        if produced > 0 && state.sound_mode == SoundMode::Enhanced {
            state.pipeline_tail_frames = state.protector.delay_frames()
                + state.clarity.delay_frames() as usize
                + state.device_rate as usize * 3;
        } else if produced == 0 && state.pipeline_tail_frames > 0 {
            let frames = state.pipeline_tail_frames.min(CHUNK_FRAMES);
            produced = frames * 2;
            state.pipeline_tail_frames -= frames;
            audible = true;
        }
        if !audible {
            // No voice produced audio this chunk. The queue is truly spent
            // only when nothing is loaded, mid-transition, or winding down —
            // a finished current voice alone still has the armed incoming
            // promotion (above) or must end the queue.
            let any_pending = state.current.as_ref().is_some_and(|c| !c.finished)
                || state.incoming.is_some()
                || state.pending_next.is_some()
                || state.transition.is_some();
            let ring_drained = state.buffered_frames.load(Ordering::Relaxed) == 0;
            if !any_pending && ring_drained && state.retiring.is_empty() {
                if state.playing {
                    log::info!("queue drained; natural end");
                    state.playing = false;
                    state.set_state(crate::PlaybackState::Stopped);
                    state.events.track_ended(
                        crate::TrackEndReason::Natural,
                        state
                            .current
                            .as_ref()
                            .map(|v| v.info.source.clone())
                            .unwrap_or_default(),
                    );
                }
                break;
            }
        }

        // Push only rendered audio. Protection carries detector and gain state
        // across render blocks, after the float-headroom processing stages.
        let mut pushed = 0;
        if produced > 0 {
            if state.sound_mode == SoundMode::Enhanced {
                state.eq.process(&mut scratch[..produced]);
            }
            state.clarity.process(&mut scratch[..produced]);
            state.protector.process(
                &mut scratch[..produced],
                state.sound_mode == SoundMode::Enhanced,
            );
            if let Ok(mut nerd) = state.nerd.lock() {
                let mut stages = Vec::new();
                let primary = if state.transition.as_ref().is_some_and(|t| t.handed_off) {
                    state.incoming.as_ref().or(state.current.as_ref())
                } else {
                    state.current.as_ref()
                };
                if let Some(v) = primary {
                    if !v.resampler.passthrough() {
                        stages.push("Rate conversion (libsoxr HQ)".into());
                    }
                    nerd.converter_delay_frames = v.resampler.delay_frames();
                    nerd.decoder_implementation = v.decoder.implementation().into();
                    nerd.decoded_layout = v.decoder.layout();
                    nerd.loudness_origin = if v.replay_gain.gain_db(v.normalization_mode).is_some()
                    {
                        "ReplayGain metadata"
                    } else if v.measured_loudness.is_some() {
                        "EBU R128 / BS.1770 LUFS"
                    } else if v.loudness_db.is_some() {
                        "YouTube relative loudness (not LUFS)"
                    } else {
                        "No measurement"
                    }
                    .into();
                    if v.info.channels != 2 {
                        stages.push("Channel mapping".into());
                    }
                    if v.user_stretch.is_some() {
                        stages.push("Pitch-preserving speed".into());
                    }
                    if v.stretch.is_some() {
                        stages.push("Automix tempo matching".into());
                    }
                    if v.spatial.enabled() {
                        stages.push("Spatial / head tracking".into());
                    }
                    if v.skip_silence {
                        stages.push("Silence trimming".into());
                    }
                }
                if state.transition.is_some() {
                    stages.push("Transition".into());
                }
                if state.volume != 1.0 || state.sleep_gain != 1.0 {
                    stages.push("Application volume".into());
                }
                if state.clarity.active() {
                    stages.push("Enhanced clarity (4× exciter)".into());
                    if state.eq.active() {
                        stages.push("Manual EQ / tone".into());
                    }
                    stages.push("True-peak protection".into());
                }
                for v in state.current.iter().chain(state.incoming.iter()) {
                    for (active, name) in [
                        (v.spatial.enabled(), "Spatial / head tracking"),
                        (v.stretch.is_some(), "Automix tempo matching"),
                        (v.user_stretch.is_some(), "Pitch-preserving speed"),
                        (v.filter.active(), "Transition filters"),
                    ] {
                        if active && !stages.iter().any(|s| s == name) {
                            stages.push(name.into());
                        }
                    }
                }
                if let Some(v) = primary {
                    nerd.loudness_gain_db = normalization_gain(
                        v,
                        state.loudness_enabled && state.sound_mode == SoundMode::Enhanced,
                    )
                    .1;
                }
                if nerd.loudness_gain_db.is_some() && state.sound_mode == SoundMode::Enhanced {
                    stages.push("Normalization".into());
                } else if state
                    .current
                    .as_ref()
                    .is_some_and(|v| v.smoothed_normalization != 1.0)
                {
                    stages.push("Normalization settling".into());
                }
                nerd.active_stages = stages;
                nerd.protection_reduction_db = state.protector.reduction_db;
                nerd.protection_interventions = state.protector.interventions
                    + state
                        .current
                        .as_ref()
                        .map(|v| v.decoder.sanitized_samples())
                        .unwrap_or(0);
                nerd.queued_target_ms = state.queue_target_ms;
                nerd.build_revision = env!("BITCHORD_CORE_REVISION").into();
            }

            if draining_pipeline
                && state.pipeline_tail_frames < state.device_rate as usize * 3
                && scratch[..produced].iter().all(|s| s.abs() < 1e-7)
            {
                state.pipeline_tail_frames = 0;
            }
            crate::diagnostics::capture(2, &scratch[..produced]);
            state
                .buffered_frames
                .fetch_add(produced as u64 / 2, Ordering::Release);
            for sample in scratch.into_iter().take(produced) {
                if ring.push(sample).is_ok() {
                    pushed += 1;
                } else {
                    break;
                }
            }
        }
        if pushed < produced {
            let missing = ((produced - pushed) / 2) as u64;
            let _ = state
                .buffered_frames
                .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub(missing))
                });
        }

        break;
    }
}

/// Direct one-voice render for the qualified integer output path. It avoids
/// gain multiplication, EQ, fades, clamping, and crossfade scheduling so the
/// decoded PCM reaches the integer output packer unchanged.
fn render_bitperfect_available(state: &mut MixerState, ring: &mut Producer<f32>) {
    if state.underrun_counter.is_some()
        && (state.flush_ring.load(Ordering::Acquire) || state.bail_flush.load(Ordering::Acquire))
    {
        return;
    }
    let chunk_samples = CHUNK_FRAMES * 2;
    while ring.slots() >= chunk_samples {
        if state.buffered_frames.load(Ordering::Relaxed)
            >= state.device_rate as u64 * state.queue_target_ms / 1000
        {
            break;
        }
        if !state.playing {
            break;
        }
        let frames = match &mut state.current {
            Some(voice) if voice.bit_perfect && voice.info.sample_rate == state.device_rate => {
                voice.pull(CHUNK_FRAMES, state.device_rate)
            }
            Some(_) => {
                // A route changed underneath an active direct stream. Do not
                // lie about the clock match; drop back to the resampling path.
                state.bit_perfect = false;
                return render_available(state, ring);
            }
            None => Vec::new(),
        };
        if frames.is_empty() {
            let exhausted = state.current.as_ref().is_none_or(|voice| voice.finished);
            if exhausted && state.buffered_frames.load(Ordering::Relaxed) == 0 {
                state.playing = false;
                state.set_state(crate::PlaybackState::Stopped);
                state.events.track_ended(
                    crate::TrackEndReason::Natural,
                    state
                        .current
                        .as_ref()
                        .map(|v| v.info.source.clone())
                        .unwrap_or_default(),
                );
            }
            break;
        }
        state.publish_audible_position();
        if let Ok(mut nerd) = state.nerd.lock() {
            nerd.active_stages.clear();
            nerd.converter_delay_frames = 0.0;
            nerd.protection_reduction_db = 0.0;
            nerd.protection_interventions = 0;
            nerd.loudness_gain_db = None;
            nerd.loudness_origin = "Bypassed for qualified integer output".into();
            nerd.queued_target_ms = state.queue_target_ms;
            nerd.build_revision = env!("BITCHORD_CORE_REVISION").into();
            if let Some(v) = state.current.as_ref() {
                nerd.decoder_implementation = v.decoder.implementation().into();
                nerd.decoded_layout = v.decoder.layout();
            }
        }
        let frames_len = frames.len() as u64;
        state
            .buffered_frames
            .fetch_add(frames_len / 2, Ordering::Release);
        let mut pushed = 0u64;
        for sample in frames {
            if ring.push(sample).is_ok() {
                pushed += 1;
            } else {
                break;
            }
        }
        if pushed < frames_len {
            let _ = state
                .buffered_frames
                .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub((frames_len - pushed) / 2))
                });
        }
        break;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::{Arc, Mutex};

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
                let power: f64 = chunk
                    .iter()
                    .map(|s| (*s as f64) * (*s as f64))
                    .sum::<f64>()
                    / chunk.len() as f64;
                10.0 * power.max(1e-12).log10()
            })
            .collect()
    }

    fn deck(lufs: Option<f64>, relative: Option<f64>, applied: Option<f64>) -> DeckLoudness {
        DeckLoudness {
            lufs,
            relative_db: relative,
            applied_normalization_db: applied,
        }
    }

    #[test]
    fn a_transition_matches_like_with_like_and_clamps_at_six_db() {
        let loud = deck(Some(-14.0), None, None);
        let quiet = deck(Some(-20.0), None, None);
        // Six LU apart: the incoming deck is boosted by six.
        let db = DeckLoudness::match_gain_db(quiet, loud).unwrap();
        assert!((db - 6.0).abs() < 1e-9, "{db}");
        // And the other way round it attenuates.
        let db = DeckLoudness::match_gain_db(loud, quiet).unwrap();
        assert!((db + 6.0).abs() < 1e-9, "{db}");
        // A twelve-LU difference is clamped: a transition corrects a level, it
        // does not re-master a track.
        let very_quiet = deck(Some(-26.0), None, None);
        let db = DeckLoudness::match_gain_db(very_quiet, loud).unwrap();
        assert!((db - MAX_TRANSITION_MATCH_DB).abs() < 1e-9, "{db}");

        // Relative figures use the player's own convention: positive is louder,
        // because `loudness_gain` attenuates by `-db`. A deck marked 6 dB louder
        // than the other is attenuated by 6, never boosted.
        let louder = deck(None, Some(6.0), None);
        let reference = deck(None, Some(0.0), None);
        let db = DeckLoudness::match_gain_db(louder, reference).unwrap();
        assert!((db + 6.0).abs() < 1e-9, "louder incoming got {db:+}");
        let db = DeckLoudness::match_gain_db(reference, louder).unwrap();
        assert!((db - 6.0).abs() < 1e-9, "quieter incoming got {db:+}");

        // An absolute figure and a relative one are not comparable.
        let mixed = deck(None, Some(3.0), None);
        assert_eq!(DeckLoudness::match_gain_db(mixed, loud), None);
        // Nothing to go on at all.
        assert_eq!(DeckLoudness::match_gain_db(deck(None, None, None), loud), None);
    }

    #[test]
    fn normalization_that_already_matched_the_decks_is_not_applied_twice() {
        // Two measured decks, both normalized to the target: their applied
        // gains are the exact inverse of their loudness, so after normalization
        // they are at the same level and the transition has nothing to correct.
        let loud = deck(Some(-20.0), None, Some(6.0));   // -20 LUFS + 6 dB → -14
        let quiet = deck(Some(-26.0), None, Some(12.0)); // -26 LUFS + 12 dB → -14
        assert_eq!(DeckLoudness::match_gain_db(quiet, loud), Some(0.0));

        // With normalization off the same two decks are six apart, and that is
        // the difference the transition has to make up.
        let loud_off = deck(Some(-20.0), None, None);
        let quiet_off = deck(Some(-26.0), None, None);
        let db = DeckLoudness::match_gain_db(quiet_off, loud_off).unwrap();
        assert!((db - 6.0).abs() < 1e-9, "{db}");

        // A relative deck under normalization keeps whatever the attenuation
        // could not reach: boosting is refused, so the residual is the match.
        let quiet_relative = deck(None, Some(-6.0), None);
        let reference = deck(None, Some(0.0), Some(0.0));
        let db = DeckLoudness::match_gain_db(quiet_relative, reference).unwrap();
        assert!((db - 6.0).abs() < 1e-9, "{db}");
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
            let sample =
                (2.0 * std::f32::consts::PI * freq * t).sin() * amplitude * i16::MAX as f32;
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
        Voice::open(&transition_source(path), false, 0.0, 48_000, 1.0, false, false).unwrap()
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
    fn noise_wav(
        path: &std::path::Path,
        lead_seconds: f32,
        seconds: f32,
        amplitude: f32,
        seed: u32,
    ) {
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
    fn kick_wav(
        path: &std::path::Path,
        seconds: f32,
        seed: u32,
        kick: f32,
        noise: f32,
    ) {
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
        let sum: f64 = samples[start..end].iter().map(|s| (*s as f64).powi(2)).sum();
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
            bed_fraction: if matches!(
                style,
                TransitionStyle::DjBlend | TransitionStyle::DjFilter
            ) {
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

    /// F: the bridge captures one measured unit and replays it without
    /// advancing the decoder, so the record continues from exactly where it
    /// stopped. Both voices are the same file, so the assertion is sample
    /// equality, not a resemblance.
    #[test]
    fn a_bridge_repeats_the_unit_and_continues_where_it_stopped() {
        let dir = std::env::temp_dir().join(format!("bitchord-bridge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bridge.wav");
        level_wav(&path, 12.0, 220.0, 0.4);
        let mut plain = open_voice(&path);
        let mut bridged = open_voice(&path);
        let rate = 48_000u32;
        let delay = 2 * rate as usize;
        let unit = rate as usize; // one second
        bridged.set_bridge(delay as u64, unit as u64, 1);
        let plain_frames = delay + 3 * unit;
        let mut a: Vec<f32> = Vec::new();
        let mut b: Vec<f32> = Vec::new();
        while a.len() / 2 < plain_frames {
            a.extend(plain.pull(512, rate));
        }
        let total_b = plain_frames + unit;
        while b.len() / 2 < total_b {
            let want = (total_b - b.len() / 2).min(512);
            b.extend(bridged.pull(want, rate));
        }
        let frame = |samples: &[f32], at: usize| -> (f32, f32) {
            (samples[at * 2], samples[at * 2 + 1])
        };
        let same = |x: &[f32], x_at: usize, y: &[f32], y_at: usize, frames: usize| -> f64 {
            let mut worst = 0.0f64;
            for i in 0..frames {
                let (a_l, a_r) = frame(x, x_at + i);
                let (b_l, b_r) = frame(y, y_at + i);
                worst = worst.max((a_l - b_l).abs() as f64);
                worst = worst.max((a_r - b_r).abs() as f64);
            }
            worst
        };
        // Before the unit, the two voices are the same performance.
        assert!(same(&a, 0, &b, 0, delay) < 1e-6, "the delay changed the audio");
        // The first pass is unchanged and the unit is captured from it.
        assert!(same(&a, delay, &b, delay, unit) < 1e-6, "the first pass changed");
        // The replay is the same unit, after the 10 ms equal-gain seam.
        let seam = (rate as usize / 100).min(unit / 4);
        assert!(
            same(&a, delay + seam, &b, delay + unit + seam, unit - seam) < 1e-6,
            "the replay is not the captured unit"
        );
        // And the record continues at the point it was paused, one unit later.
        assert!(
            same(&a, delay + unit, &b, delay + 2 * unit, unit) < 1e-6,
            "the record did not continue where it stopped"
        );
        // The seam itself is a crossfade, so it is near the unit's own level,
        // not a hole.
        let level = |samples: &[f32], at: usize, frames: usize| -> f64 {
            let mut sum = 0.0f64;
            for i in 0..frames {
                let (l, r) = frame(samples, at + i);
                sum += l as f64 * l as f64 + r as f64 * r as f64;
            }
            (sum / (frames * 2) as f64).sqrt()
        };
        let seam_level = level(&b, delay + unit, seam);
        let unit_level = level(&a, delay, seam);
        assert!(
            seam_level > unit_level * 0.7 && seam_level < unit_level * 1.3,
            "the seam is a hole or a bump ({seam_level} vs {unit_level})"
        );
        std::fs::remove_file(&path).ok();
    }

    /// G: the transition match and the incoming's normalization are targets
    /// **before** the first audible frame, not ramps through the first 0.4 s.
    /// The owner hears the old shape as "the next song blasts, then corrects".
    #[test]
    fn a_transition_match_is_settled_before_the_first_audible_frame() {
        let dir = std::env::temp_dir().join(format!("bitchord-match-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let loud = dir.join("loud.wav");
        let quiet = dir.join("quiet.wav");
        noise_wav(&loud, 0.0, 6.0, 0.5, 0x51DC_1A2B);
        noise_wav(&quiet, 0.0, 6.0, 0.5, 0x77EE_3C4D);
        let mut outgoing_source = transition_source(&loud);
        outgoing_source.loudness_db = Some(-2.0);
        let mut incoming_source = transition_source(&quiet);
        incoming_source.loudness_db = Some(-8.0);
        let mut outgoing =
            Voice::open(&outgoing_source, false, 0.0, 48_000, 1.0, false, false).unwrap();
        let mut incoming =
            Voice::open(&incoming_source, false, 0.0, 48_000, 1.0, false, false).unwrap();
        incoming.arm_transition_match(Some(&outgoing), false);
        assert!(
            (incoming.match_db - 6.0).abs() < 1e-6,
            "expected a +6 dB match, got {}",
            incoming.match_db
        );
        let target = db_to_gain(6.0) as f32;
        assert!(
            (incoming.smoothed_match - target).abs() < 1e-6,
            "the match is set at arm, not ramped at the fade: {} vs {target}",
            incoming.smoothed_match
        );
        let rate = 48_000usize;
        let mut levels = Vec::new();
        for window in 0..3 {
            let frames = incoming.pull(rate, rate as u32);
            let mut mixed = vec![0.0f32; frames.len()];
            incoming.mix_into(&frames, &mut mixed, 1.0, false);
            if window == 0 {
                assert!(
                    (mixed[0] / frames[0] - target).abs() < 1e-4,
                    "the first audible sample is not at the match: {} vs {target}",
                    mixed[0] / frames[0]
                );
            }
            levels.push(rms_window(&mixed, 0, rate));
        }
        for pair in levels.windows(2) {
            let jump = 20.0 * (pair[1] / pair[0]).log10();
            assert!(
                jump.abs() < 1.0,
                "the incoming's first seconds jump by {jump:.2} dB: {levels:?}"
            );
        }
        std::fs::remove_file(&loud).ok();
        std::fs::remove_file(&quiet).ok();
    }

    /// G: a beat-matched DjBlend of two records with the same kick pattern is
    /// the correlated case B's independent-noise harness cannot see. The bass
    /// swap has to keep the aligned kicks from summing to +3 dB.
    #[test]
    fn a_correlated_dj_blend_does_not_bump_at_the_midpoint() {
        const RATE: usize = 48_000;
        const BEAT: f64 = 0.5;
        const FADE: f64 = 8.0;
        const CHUNK: usize = 512;
        let dir = std::env::temp_dir().join(format!("bitchord-djcorr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let outgoing_path = dir.join("kick-a.wav");
        let incoming_path = dir.join("kick-b.wav");
        kick_wav(&outgoing_path, 12.0, 0x1234_5678, 0.45, 0.15);
        kick_wav(&incoming_path, 12.0, 0x9E37_79B9, 0.45, 0.15);
        let mut outgoing = open_voice(&outgoing_path);
        let mut incoming = open_voice(&incoming_path);
        incoming.gain = 0.0;
        incoming.applied_gain = 0.0;
        prime_voice(&mut incoming, RATE as u32);
        let plan = TransitionPlan {
            style: TransitionStyle::DjBlend,
            bass_swap: true,
            bass_swap_fraction: 0.62,
            bed_fraction: 0.4,
            bed_gain_db: -14.0,
            dip_depth: 0.2,
            dip_width: 0.08,
            fade_seconds: FADE,
            beat_seconds: BEAT,
            ..Default::default()
        };
        let fade_frames = (FADE * RATE as f64) as usize;
        let mut mixed = Vec::with_capacity(fade_frames * 2);
        let mut out_raw = Vec::with_capacity(fade_frames * 2);
        let mut in_raw = Vec::with_capacity(fade_frames * 2);
        let mut done = 0usize;
        while done < fade_frames {
            let p = done as f64 / fade_frames as f64;
            ride_filters(p, &plan, &mut outgoing.filter, &mut incoming.filter);
            let (rise, fall) = automix_gains(p, &plan, 0.0);
            let a = outgoing.pull(CHUNK, RATE as u32);
            let b = incoming.pull(CHUNK, RATE as u32);
            let mut out = vec![0.0f32; CHUNK * 2];
            outgoing.mix_into(&a, &mut out, fall, false);
            incoming.mix_into(&b, &mut out, rise, false);
            let mut a_pad = a;
            a_pad.resize(CHUNK * 2, 0.0);
            let mut b_pad = b;
            b_pad.resize(CHUNK * 2, 0.0);
            out_raw.extend_from_slice(&a_pad);
            in_raw.extend_from_slice(&b_pad);
            mixed.extend_from_slice(&out);
            done += CHUNK;
        }
        let window = 3 * RATE;
        let span = fade_frames.saturating_sub(window);
        let mut dip = 0.0f64;
        let mut rise = 0.0f64;
        let mut at = 0;
        while at <= span {
            let m = 20.0 * rms_window(&mixed, at, window).max(1e-9).log10();
            let o = 20.0 * rms_window(&out_raw, at, window).max(1e-9).log10();
            let i = 20.0 * rms_window(&in_raw, at, window).max(1e-9).log10();
            dip = dip.min(m - o.min(i));
            rise = rise.max(m - o.max(i));
            at += 4_800;
        }
        println!("DjBlend correlated: 3s dip {dip:.2} dB, rise {rise:.2} dB");
        assert!(dip > -3.0, "the correlated blend dipped {dip:.2} dB");
        assert!(
            rise < 2.0,
            "the aligned kicks lift the blend by {rise:.2} dB; the bass swap did not separate them"
        );
        std::fs::remove_file(&outgoing_path).ok();
        std::fs::remove_file(&incoming_path).ok();
    }

    #[test]
    fn priming_a_voice_does_not_advance_its_audible_position() {
        let dir =
            std::env::temp_dir().join(format!("bitchord-mixer-prime-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("prime.wav");
        level_wav(&path, 2.0, 440.0, 0.2);
        let mut voice = open_voice(&path);
        // A beatmatched handoff runs the incoming slightly off unity; the
        // pre-roll must be decoded at that rate, not at the rate of a fresh
        // decoder.
        voice.base_plan_rate = 1.03;
        let before = voice.emitted_position_seconds();
        prime_voice(&mut voice, 48_000);
        assert_eq!(voice.emitted_dev_frames, 0, "pre-roll must not be audible yet");
        assert!(
            (voice.emitted_position_seconds() - before).abs() < 0.03,
            "pre-roll moved the audible position from {before:.3}s to {:.3}s",
            voice.emitted_position_seconds()
        );
        let first = voice.pull(512, 48_000);
        assert!(!first.is_empty(), "the primed voice must serve its cue");
    }

    #[test]
    fn a_planned_cue_inside_a_quiet_lead_in_starts_at_the_audible_edge() {
        let dir = std::env::temp_dir().join(format!("bitchord-mixer-cue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cue-lead.wav");
        noise_wav(&path, 1.5, 4.0, 0.2, 0xDEAD_BEEF);
        crate::playback_regions::put(
            path.display().to_string(),
            crate::PlaybackRegions {
                audible_start_seconds: 1.5,
                audible_end_seconds: None,
                excluded: Vec::new(),
            },
        );
        let mut request = transition_source(&path);
        // The planner's cue lands inside the quiet lead-in; the audible edge
        // has to win, not the raw cue.
        request.plan.cue_seconds = 0.5;
        request.plan.playback_rate = 1.0;
        let mut voice =
            Voice::open(&request, false, 0.0, 48_000, 1.0, false, true).unwrap();
        let pcm = voice.pull(512, 48_000);
        let rms = (pcm.iter().map(|s| (*s as f64).powi(2)).sum::<f64>()
            / (pcm.len() / 2).max(1) as f64)
            .sqrt();
        assert!(rms > 0.01, "the cue started inside the quiet lead-in ({rms})");
        assert!(
            voice.emitted_position_seconds() > 1.45,
            "the audible position is {:.2}s, not the trimmed edge",
            voice.emitted_position_seconds()
        );
        drop(voice);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn mix_into_ramps_between_chunk_targets() {
        let dir = std::env::temp_dir().join(format!("bitchord-mixer-ramp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ramp.wav");
        level_wav(&path, 1.0, 440.0, 0.2);
        let mut voice = open_voice(&path);
        // A constant input makes the output the gain curve itself.
        let source = vec![1.0f32; 512 * 2];
        let mut out = vec![0.0f32; 512 * 2];
        voice.mix_into(&source, &mut out, 0.0, false);
        let left: Vec<f32> = out.chunks_exact(2).map(|frame| frame[0]).collect();
        let step = left
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(step < 0.01, "max inter-sample gain step {step}");
        assert!(left[0] > 0.99, "first sample {}", left[0]);
        assert!(*left.last().unwrap() < 0.01, "last sample {}", left.last().unwrap());

        // The next chunk starts where the previous one ended: no step at a
        // 512-frame boundary even though the target jumped to 1.0.
        let mut next = vec![0.0f32; 512 * 2];
        voice.mix_into(&source, &mut next, 1.0, false);
        let left: Vec<f32> = next.chunks_exact(2).map(|frame| frame[0]).collect();
        assert!(left[0] < 0.01, "chunk boundary step {}", left[0]);
        assert!(*left.last().unwrap() > 0.99, "last sample {}", left.last().unwrap());
        let step = left
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(step < 0.01, "max inter-sample gain step {step}");
    }

    #[test]
    fn every_style_has_no_hole_at_the_handover() {
        let dir =
            std::env::temp_dir().join(format!("bitchord-mixer-hole-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let outgoing = dir.join("out.wav");
        let incoming = dir.join("in.wav");
        noise_wav(&outgoing, 0.0, 12.0, 0.5, 0x1234_5678);
        // 1.5 s of quiet lead-in, long enough that the cue must trim it and
        // short enough to sit inside the arming window.
        noise_wav(&incoming, 1.5, 30.0, 0.5, 0x9E37_79B9);
        crate::playback_regions::put(
            incoming.display().to_string(),
            crate::PlaybackRegions {
                audible_start_seconds: 1.5,
                audible_end_seconds: None,
                excluded: Vec::new(),
            },
        );
        for style in [
            TransitionStyle::EchoOut,
            TransitionStyle::LoopRoll,
            TransitionStyle::ReverbWash,
            TransitionStyle::DropCut,
            TransitionStyle::EqualPower,
            TransitionStyle::DjBlend,
            TransitionStyle::DjFilter,
        ] {
            let rendered = render_transition_style(style, &outgoing, &incoming, 0.0);
            let frames = rendered.fade_frames;
            let first_in = rms_window(&rendered.incoming, 0, 480);
            assert!(
                first_in > 0.05,
                "{style:?}: the primed incoming is silent at the blend start ({first_in}) \
                 — the lead-in was not trimmed or the buffer was not primed"
            );

            // B's floor: across the landing ±200 ms the summed output must
            // never sit more than 6 dB below the quieter record.
            let start = rendered.land_frame.saturating_sub(9_600);
            let end = (rendered.land_frame + 9_600).min(frames);
            let mut worst = f64::INFINITY;
            let mut worst_at = start;
            let mut hole = None;
            let mut at = start;
            while at + 480 <= end {
                let m = rms_window(&rendered.mixed, at, 480);
                let o = rms_window(&rendered.outgoing, at, 480);
                let i = rms_window(&rendered.incoming, at, 480);
                let reference = o.min(i);
                if reference > 1e-6 && m > 0.0 {
                    let ratio = 20.0 * (m / reference).log10();
                    if ratio < worst {
                        worst = ratio;
                        worst_at = at;
                    }
                }
                // A 20 ms run below −60 dBFS is a hole, not a quiet moment.
                if hole.is_none() && at + 960 <= end && rms_window(&rendered.mixed, at, 960) < 0.001
                {
                    hole = Some(at);
                }
                at += 480;
            }
            let worst = if worst.is_finite() { worst } else { 0.0 };
            assert!(
                hole.is_none(),
                "{style:?}: a 20 ms hole at frame {:?} (land {})",
                hole,
                rendered.land_frame
            );
            assert!(
                worst > -6.0,
                "{style:?}: dipped to {worst:.2} dB at frame {worst_at} (land {})",
                rendered.land_frame
            );

            // Continuity of level, too: 3 s short-term RMS stays within 3 dB
            // below the quieter neighbour and 2 dB above the louder one.
            let window = 3 * 48_000usize;
            let span = frames.saturating_sub(window);
            let mut dip = 0.0f64;
            let mut rise = 0.0f64;
            let mut rise_at = 0usize;
            let mut rise_values = (0.0f64, 0.0f64, 0.0f64);
            let mut at = 0;
            while at <= span {
                let m = 20.0 * rms_window(&rendered.mixed, at, window).max(1e-9).log10();
                let o = 20.0 * rms_window(&rendered.outgoing, at, window).max(1e-9).log10();
                let i = 20.0 * rms_window(&rendered.incoming, at, window).max(1e-9).log10();
                dip = dip.min(m - o.min(i));
                if m - o.max(i) > rise {
                    rise = m - o.max(i);
                    rise_at = at;
                    rise_values = (m, o, i);
                }
                at += 4_800;
            }
            println!(
                "{style:?}: land {}f, floor {worst:.2} dB, 3s dip {dip:.2} dB, rise {rise:.2} dB \
                 at {rise_at} (m/o/i {rise_values:?})",
                rendered.land_frame
            );
            assert!(
                dip > -3.0,
                "{style:?}: the 3 s level dipped {dip:.2} dB below the quieter record"
            );
            assert!(
                rise < 2.0,
                "{style:?}: the 3 s level rose {rise:.2} dB above the louder record"
            );
        }
    }


    /// E-2 in numbers: a cold ending holds the outgoing through its final
    /// chord and enters the next record underneath it, so the summed level does
    /// not lift at the chord and then has a decay to cross over. The gate is the
    /// same B gate — no dip below the quieter record, no rise above the louder
    /// one — on the new hold shape.
    #[test]
    fn a_cold_ending_renders_flat_through_its_chord() {
        const RATE: usize = 48_000;
        let dir =
            std::env::temp_dir().join(format!("bitchord-mixer-cold-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let outgoing = dir.join("out.wav");
        let incoming = dir.join("in.wav");
        noise_wav(&outgoing, 0.0, 12.0, 0.5, 0x2468_ACE0);
        noise_wav(&incoming, 1.5, 30.0, 0.5, 0x1357_9BDF);
        crate::playback_regions::put(
            incoming.display().to_string(),
            crate::PlaybackRegions {
                audible_start_seconds: 1.5,
                audible_end_seconds: None,
                excluded: Vec::new(),
            },
        );
        // Pair 5's phrase inside the 8 s harness: the chord is struck at 3.36 s
        // and the remaining 4.64 s is the crossing.
        let hold = 3.36;
        let rendered =
            render_transition_style(TransitionStyle::EqualPower, &outgoing, &incoming, hold);
        let frames = rendered.fade_frames;
        let chord = (hold * RATE as f64) as usize;
        let at_chord = rms_window(&rendered.mixed, chord, 480);
        let out_chord = rms_window(&rendered.outgoing, chord, 480).max(1e-9);
        let chord_db = 20.0 * (at_chord / out_chord).log10();
        assert!(
            (-1.0..1.6).contains(&chord_db),
            "the chord frame is {chord_db:+.2} dB against the outgoing record — the hold is not flat"
        );

        let window = 3 * RATE;
        let span = frames.saturating_sub(window);
        let mut dip = 0.0f64;
        let mut rise = 0.0f64;
        let mut at = 0;
        while at <= span {
            let m = 20.0 * rms_window(&rendered.mixed, at, window).max(1e-9).log10();
            let o = 20.0 * rms_window(&rendered.outgoing, at, window).max(1e-9).log10();
            let i = 20.0 * rms_window(&rendered.incoming, at, window).max(1e-9).log10();
            dip = dip.min(m - o.min(i));
            rise = rise.max(m - o.max(i));
            at += 4_800;
        }
        println!(
            "ColdEnd: chord {chord}f {chord_db:+.2} dB, 3s dip {dip:.2} dB, rise {rise:.2} dB"
        );
        assert!(dip > -3.0, "the cold ending dipped {dip:.2} dB");
        assert!(rise < 2.0, "the cold ending rose {rise:.2} dB");
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

    /// The production path, end to end: the playing deck gets its measurement
    /// from `SetLoudnessMeasurement`, the queued deck is opened by the arming
    /// path with only the player's relative figure — and the two still match.
    ///
    /// No voice is configured by hand here: the tracks go in through `Load` and
    /// `QueueNext`, exactly as the engine receives them, and the correction is
    /// read back from the published nerd snapshot.
    #[test]
    fn the_queue_path_matches_a_measured_deck_to_a_relative_one() {
        // Short files: the blend is scheduled at the end of the first one, and
        // the mixer runs in real time.
        let dir = std::env::temp_dir().join(format!("bitchord-mixer-queue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let loud = dir.join("a.wav");
        let quiet = dir.join("b.wav");
        level_wav(&loud, 3.0, 1_000.0, 0.5);
        level_wav(&quiet, 3.0, 1_500.0, 0.25);

        let events = std::sync::Arc::new(RecordedEvents::default());
        let (tx, rx) = crossbeam_channel::unbounded::<Command>();
        let (consumer_side, mut consumer) = rtrb::RingBuffer::<f32>::new(44_100 * 2 * 4);
        let buffered = std::sync::Arc::new(AtomicU64::new(0));
        let position = std::sync::Arc::new(AtomicU64::new(0));
        let duration = std::sync::Arc::new(AtomicU64::new(0));
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let nerd = std::sync::Arc::new(Mutex::new(NerdSnapshot::default()));

        let handle = std::thread::spawn({
            let events = events.clone();
            let buffered = buffered.clone();
            let position = position.clone();
            let duration = duration.clone();
            let shutdown = shutdown.clone();
            let nerd = nerd.clone();
            move || {
                run_mixer(
                    rx,
                    consumer_side,
                    buffered,
                    position,
                    duration,
                    44_100,
                    events,
                    shutdown,
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(AtomicBool::new(false)),
                    nerd,
                )
            }
        });

        let source = |path: &std::path::Path, loudness: f64| TrackSource {
            source: path.display().to_string(),
            title: path.display().to_string(),
            artist: String::new(),
            start_seconds: 0.0,
            plan: TransitionPlan::default(),
            headers: std::collections::HashMap::new(),
            claimed_kbps: 0,
            loudness_db: Some(loudness),
            duration_seconds: 3.0,
        };
        tx.send(Command::Load {
            request: source(&loud, 0.0),
            reply: crossbeam_channel::bounded(1).0,
        })
        .unwrap();
        // The current deck is the only one production measures.
        tx.send(Command::SetLoudnessMeasurement(measured(-14.0)))
            .unwrap();
        // The queued deck is opened by the arming path and has only the player's
        // relative figure.
        tx.send(Command::QueueNext {
            request: source(&quiet, -6.0),
        })
        .unwrap();

        // The mixer blocks on a full ring, so the drain never stops. Arming is
        // what this test is about: it happens as the outgoing deck approaches
        // its end, well before the fade itself. Waiting for the handoff as well
        // would make the test about the crossfade's schedule.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        // The device callback's side of the ring: every popped sample has to
        // leave the shared counter, or the mixer believes the ring is full and
        // never looks at its command channel again.
        fn drain(consumer: &mut rtrb::Consumer<f32>, buffered: &AtomicU64) {
            while consumer.pop().is_ok() {
                let current = buffered.load(Ordering::Relaxed);
                if current > 0 {
                    let _ = buffered.compare_exchange(
                        current,
                        current - 1,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                }
            }
        }

        let mut armed = None;
        loop {
            drain(&mut consumer, &buffered);
            armed = nerd.lock().unwrap().transition_match_db;
            if armed.is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the transition was never armed"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        // `Command::Shutdown` only stops the voices; the thread watches its own
        // flag, and the drain has to continue until it has seen it — the mixer
        // is blocked on a full ring otherwise.
        shutdown.store(true, Ordering::Release);
        let stop = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !handle.is_finished() {
            drain(&mut consumer, &buffered);
            assert!(std::time::Instant::now() < stop, "the mixer did not stop");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        handle.join().unwrap();

        assert_eq!(
            armed,
            Some(6.0),
            "a measured outgoing deck and a relative incoming one declined to match"
        );
    }

    #[test]
    fn the_voice_path_matches_a_six_lu_blend_and_releases() {
        let (loud, quiet) = levels();
        let mut outgoing = open_voice(&loud);
        let mut incoming = open_voice(&quiet);
        outgoing.measured_loudness = Some(measured(-14.0));
        incoming.measured_loudness = Some(measured(-20.0));

        incoming.arm_transition_match(Some(&outgoing), false);
        assert!((incoming.match_db - 6.0).abs() < 1e-6, "armed {}", incoming.match_db);
        // G: the correction is in place at arm, before the incoming is audible.
        // A ramp here is the owner's "blasts, then corrects" complaint.
        assert!(
            (incoming.smoothed_match - db_to_gain(6.0) as f32).abs() < 1e-6,
            "the match was not in place at arm: {}",
            incoming.smoothed_match
        );

        let mixed = render_blend(&mut outgoing, &mut incoming, false);
        let (spread, peak) = level_spread(&mixed);
        assert!(spread < 1.0, "level moved {spread:.2} dB across the blend");
        assert!(peak <= 1.0, "peak {peak} clipped");
        assert!(
            incoming.smoothed_match > 1.9,
            "the ramp did not arrive: {}",
            incoming.smoothed_match
        );

        // Release walks the correction home instead of stepping it away.
        incoming.release_transition_match();
        assert_eq!(incoming.match_db, 0.0);
        let mut previous = incoming.smoothed_match;
        for _ in 0..120 {
            let frames = incoming.pull(1024, 48_000);
            let mut out = vec![0.0f32; frames.len()];
            incoming.mix_into(&frames, &mut out, 1.0, false);
            assert!(
                incoming.smoothed_match <= previous + 1e-6,
                "release went back up: {} > {previous}",
                incoming.smoothed_match
            );
            previous = incoming.smoothed_match;
        }
        assert!(previous < 1.5, "release stalled at {}", previous);
    }

    #[test]
    fn the_same_blend_without_the_match_dips() {
        let (loud, quiet) = levels();
        let mut outgoing = open_voice(&loud);
        let mut incoming = open_voice(&quiet);
        outgoing.measured_loudness = Some(measured(-14.0));
        incoming.measured_loudness = Some(measured(-20.0));

        let mixed = render_blend(&mut outgoing, &mut incoming, false);
        let (spread, _) = level_spread(&mixed);
        assert!(spread > 1.0, "the unmatched blend should dip, moved {spread:.2} dB");
    }

    #[test]
    fn a_normalized_blend_is_not_corrected_twice() {
        let (loud, quiet) = levels();
        let mut outgoing = open_voice(&loud);
        let mut incoming = open_voice(&quiet);
        outgoing.normalization_mode = LoudnessMode::Track;
        incoming.normalization_mode = LoudnessMode::Track;
        outgoing.measured_loudness = Some(measured(-14.0));
        incoming.measured_loudness = Some(measured(-20.0));

        // Normalization boosts the quieter deck by exactly the difference, so
        // both decks reach the blend at the target level and the transition has
        // nothing left to correct.
        incoming.arm_transition_match(Some(&outgoing), true);
        assert_eq!(incoming.match_db, 0.0, "the same difference was corrected twice");

        let mixed = render_blend(&mut outgoing, &mut incoming, true);
        let (spread, peak) = level_spread(&mixed);
        assert!(spread < 1.0, "normalized blend moved {spread:.2} dB");
        assert!(peak <= 1.0, "peak {peak} clipped");
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
        let mut current =
            Voice::open(&source(outgoing, "out"), false, 0.0, 44_100, 1.0, false, false).unwrap();
        let incoming_voice =
            Voice::open(&source(incoming, "in"), false, 0.0, 44_100, 1.0, false, false).unwrap();
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

    #[test]
    fn automix_winner_classifies_safety_and_analyzed_plans() {
        assert_eq!(
            automix_plan_winner(&TransitionPlan {
                fade_seconds: 4.0,
                reason: "safety:".into(),
                ..TransitionPlan::default()
            }),
            Some("safety")
        );
        assert_eq!(
            automix_plan_winner(&TransitionPlan {
                fade_seconds: 2.0,
                reason: "tempo fit".into(),
                ..TransitionPlan::default()
            }),
            Some("analyzed")
        );
        assert_eq!(automix_plan_winner(&TransitionPlan::default()), None);
        assert_eq!(
            automix_plan_winner(&TransitionPlan {
                style: TransitionStyle::EchoOut,
                fade_seconds: 1.0,
                reason: "skip-mix:".into(),
                ..TransitionPlan::default()
            }),
            None
        );
    }

    #[test]
    fn analyzed_plan_replaces_safety_plan_only_while_arming() {
        let dir = std::env::temp_dir()
            .join(format!("bitchord-am4-upgrade-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let outgoing = dir.join("out.wav");
        let incoming = dir.join("in.wav");
        test_wav(&outgoing, 12.0, 440.0);
        test_wav(&incoming, 12.0, 660.0);

        let mut arming = armed_test_state(&outgoing, &incoming, Phase::Arming);
        arming.upgrade_armed_plan(analyzed_upgrade(&incoming));
        let retained = &arming.transition.as_ref().unwrap().plan;
        assert_eq!(automix_plan_winner(retained), Some("analyzed"));
        assert_eq!(retained.style, TransitionStyle::DjBlend);
        assert_eq!(retained.cue_seconds, 1.0);

        let mut fading = armed_test_state(&outgoing, &incoming, Phase::Fading);
        fading.upgrade_armed_plan(analyzed_upgrade(&incoming));
        let retained = &fading.transition.as_ref().unwrap().plan;
        assert_eq!(automix_plan_winner(retained), Some("safety"));
        assert_eq!(retained.style, TransitionStyle::EqualPower);
    }

    #[test]
    fn skip_mix_starts_a_two_beat_echo_when_an_armed_grid_exists() {
        let dir = std::env::temp_dir()
            .join(format!("bitchord-am5-skip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let outgoing = dir.join("out.wav");
        let incoming = dir.join("in.wav");
        test_wav(&outgoing, 12.0, 440.0);
        test_wav(&incoming, 12.0, 660.0);

        let mut state = armed_test_state(&outgoing, &incoming, Phase::Arming);
        state.transition.as_mut().unwrap().plan.beat_seconds = 0.5;
        assert!(state.begin_skip_mix().is_some());
        state.consider_start_fade();

        let transition = state.transition.as_ref().unwrap();
        assert!(transition.phase == Phase::Fading);
        assert_eq!(transition.plan.style, TransitionStyle::EchoOut);
        assert_eq!(transition.fade_frames, 44_100);
        assert_eq!(
            transition.fx.as_ref().unwrap().window_frames(),
            transition.fade_frames
        );
        assert_eq!(transition.plan.reason, "skip-mix:");
    }

    #[test]
    fn skip_mix_without_a_grid_keeps_the_existing_cut_fallback() {
        let dir = std::env::temp_dir()
            .join(format!("bitchord-am5-no-grid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let outgoing = dir.join("out.wav");
        let incoming = dir.join("in.wav");
        test_wav(&outgoing, 12.0, 440.0);
        test_wav(&incoming, 12.0, 660.0);

        let mut state = armed_test_state(&outgoing, &incoming, Phase::Arming);
        assert!(state.begin_skip_mix().is_none());
        state.promote_incoming();
        assert_eq!(state.current.as_ref().unwrap().info.title, "in");
        assert!(state.transition.is_none());
    }

    #[test]
    fn throttled_intro_fixture_keeps_analyzed_wins_at_or_above_ninety_percent() {
        const DOWNLOAD_RATE_KIB_PER_SECOND: u64 = 64;
        const ANALYSIS_TIME_MS: u64 = 350;
        const FADE_START_MS: u64 = 10_000;
        const INTRO_KIB: [u64; 10] = [16, 64, 128, 256, 320, 384, 448, 512, 576, 1_024];

        // Fixed download rate and analysis time form a deterministic throttled
        // arrival fixture. Nine plans reach the mixer during Arming; the tenth
        // finishes analysis after the safety fade has started.
        let analyzed_wins = INTRO_KIB
            .into_iter()
            .filter(|intro_kib| {
                let download_ms =
                    (*intro_kib * 1_000 + DOWNLOAD_RATE_KIB_PER_SECOND - 1)
                        / DOWNLOAD_RATE_KIB_PER_SECOND;
                let analysis_ready_ms = download_ms + ANALYSIS_TIME_MS;
                let phase = if analysis_ready_ms < FADE_START_MS {
                    Phase::Arming
                } else {
                    Phase::Fading
                };
                accepts_analyzed_plan(phase, false)
            })
            .count();
        assert_eq!(analyzed_wins, 9);
        assert!(
            analyzed_wins * 100 >= INTRO_KIB.len() * 90,
            "only {analyzed_wins}/{} throttled intros kept the analyzed plan at {DOWNLOAD_RATE_KIB_PER_SECOND} KiB/s with {ANALYSIS_TIME_MS} ms analysis",
            INTRO_KIB.len()
        );
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

    #[test]
    fn gapless_queue_advances_and_fires_handoff() {
        let dir = std::env::temp_dir().join("bitchord-mixer-test");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        test_wav(&a, 1.0, 440.0);
        test_wav(&b, 1.0, 660.0);

        let events = std::sync::Arc::new(RecordedEvents::default());
        let (tx, rx) = crossbeam_channel::unbounded::<Command>();
        let (consumer_side, mut consumer) = rtrb::RingBuffer::<f32>::new(44_100 * 2 * 2);
        let buffered = std::sync::Arc::new(AtomicU64::new(0));
        let position = std::sync::Arc::new(AtomicU64::new(0));
        let duration = std::sync::Arc::new(AtomicU64::new(0));
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));

        let handle = std::thread::spawn({
            let events = events.clone();
            let buffered = buffered.clone();
            let position = position.clone();
            let duration = duration.clone();
            let shutdown = shutdown.clone();
            move || {
                run_mixer(
                    rx,
                    consumer_side,
                    buffered,
                    position,
                    duration,
                    44_100,
                    events,
                    shutdown,
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(Mutex::new(NerdSnapshot::default())),
                )
            }
        });

        tx.send(Command::Load {
            request: TrackSource {
                source: a.display().to_string(),
                title: "A".into(),
                artist: String::new(),
                start_seconds: 0.0,
                plan: TransitionPlan::default(),
                headers: std::collections::HashMap::new(),
                claimed_kbps: 0,
                loudness_db: None,
                duration_seconds: 0.0,
            },
            reply: crossbeam_channel::bounded(1).0,
        })
        .unwrap();
        tx.send(Command::QueueNext {
            request: TrackSource {
                source: b.display().to_string(),
                title: "B".into(),
                artist: String::new(),
                start_seconds: 0.0,
                plan: TransitionPlan::default(),
                headers: std::collections::HashMap::new(),
                claimed_kbps: 0,
                loudness_db: None,
                duration_seconds: 0.0,
            },
        })
        .unwrap();

        // Drain the ring the way the device callback would, saturating the
        // shared counter (regression: a plain fetch_sub wrapped u64 and
        // wedged every tail computation — no handoff ever fired).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            while let Ok(_) = consumer.pop() {
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
            let handoffs = events.handoffs.lock().unwrap();
            if !handoffs.is_empty() || std::time::Instant::now() > deadline {
                break;
            }
            drop(handoffs);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let handoffs = events.handoffs.lock().unwrap().clone();
        assert_eq!(handoffs, vec!["B".to_string()], "handoff to B must fire");

        shutdown.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }

    /// Repeat-one / single-item repeat-all with Automix: the same file must be
    /// accepted as pending-next when it carries a fade, so the track can blend
    /// into itself instead of stopping at EOS.
    #[test]
    fn a_planned_self_mix_is_accepted_and_hands_off() {
        let dir = std::env::temp_dir().join("bitchord-mixer-self-mix");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("loop.wav");
        test_wav(&a, 3.0, 440.0);

        let events = std::sync::Arc::new(RecordedEvents::default());
        let (tx, rx) = crossbeam_channel::unbounded::<Command>();
        let (consumer_side, mut consumer) = rtrb::RingBuffer::<f32>::new(44_100 * 2 * 4);
        let buffered = std::sync::Arc::new(AtomicU64::new(0));
        let position = std::sync::Arc::new(AtomicU64::new(0));
        let duration = std::sync::Arc::new(AtomicU64::new(0));
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));

        let handle = std::thread::spawn({
            let events = events.clone();
            let buffered = buffered.clone();
            let position = position.clone();
            let duration = duration.clone();
            let shutdown = shutdown.clone();
            move || {
                run_mixer(
                    rx,
                    consumer_side,
                    buffered,
                    position,
                    duration,
                    44_100,
                    events,
                    shutdown,
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(Mutex::new(NerdSnapshot::default())),
                )
            }
        });

        let path = a.display().to_string();
        tx.send(Command::Load {
            request: TrackSource {
                source: path.clone(),
                title: "Loop".into(),
                artist: String::new(),
                start_seconds: 0.0,
                plan: TransitionPlan::default(),
                headers: std::collections::HashMap::new(),
                claimed_kbps: 0,
                loudness_db: None,
                duration_seconds: 3.0,
            },
            reply: crossbeam_channel::bounded(1).0,
        })
        .unwrap();
        tx.send(Command::QueueNext {
            request: TrackSource {
                source: path,
                title: "Loop".into(),
                artist: String::new(),
                start_seconds: 0.0,
                plan: TransitionPlan {
                    fade_seconds: 1.0,
                    effect_seconds: 0.0,
                    effect_start_seconds: 0.0,
                    ..TransitionPlan::default()
                },
                headers: std::collections::HashMap::new(),
                claimed_kbps: 0,
                loudness_db: None,
                duration_seconds: 3.0,
            },
        })
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            while let Ok(_) = consumer.pop() {
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
            let handoffs = events.handoffs.lock().unwrap();
            if !handoffs.is_empty() || std::time::Instant::now() > deadline {
                break;
            }
            drop(handoffs);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let handoffs = events.handoffs.lock().unwrap().clone();
        let ended = events.ended.lock().unwrap().clone();
        assert_eq!(
            handoffs,
            vec!["Loop".to_string()],
            "self-mix must hand off into the same title, not drain: ended={ended:?}"
        );

        shutdown.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }

    #[test]
    fn load_pushes_current_voice_into_the_ring() {
        let dir = std::env::temp_dir().join("bitchord-mixer-current");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("solo.wav");
        test_wav(&a, 0.5, 440.0);

        let events = std::sync::Arc::new(RecordedEvents::default());
        let (tx, rx) = crossbeam_channel::unbounded::<Command>();
        let (consumer_side, mut consumer) = rtrb::RingBuffer::<f32>::new(44_100 * 2 * 2);
        let buffered = std::sync::Arc::new(AtomicU64::new(0));
        let position = std::sync::Arc::new(AtomicU64::new(0));
        let duration = std::sync::Arc::new(AtomicU64::new(0));
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));

        let handle = std::thread::spawn({
            let events = events.clone();
            let buffered = buffered.clone();
            let position = position.clone();
            let duration = duration.clone();
            let shutdown = shutdown.clone();
            move || {
                run_mixer(
                    rx,
                    consumer_side,
                    buffered,
                    position,
                    duration,
                    44_100,
                    events,
                    shutdown,
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(Mutex::new(NerdSnapshot::default())),
                )
            }
        });

        tx.send(Command::Load {
            request: TrackSource {
                source: a.display().to_string(),
                title: "solo".into(),
                artist: String::new(),
                start_seconds: 0.0,
                plan: TransitionPlan::default(),
                headers: std::collections::HashMap::new(),
                claimed_kbps: 0,
                loudness_db: None,
                duration_seconds: 0.0,
            },
            reply: crossbeam_channel::bounded(1).0,
        })
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut samples = 0usize;
        while std::time::Instant::now() < deadline && samples == 0 {
            while consumer.pop().is_ok() {
                samples += 1;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            samples > 0,
            "current voice must reach the ring (got {samples})"
        );

        shutdown.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }

    #[test]
    fn cold_resume_seek_at_180_seconds_outputs_pcm_from_that_position() {
        // A seekable source must start directly at the requested timestamp:
        // restoring a saved playhead must not decode 180 seconds of audio, and
        // the first ring samples must contain the signal at that position.
        let a =
            std::env::temp_dir().join(format!("bitchord-mixer-resume-{}.wav", std::process::id()));
        test_wav(&a, 190.0, 997.0);

        let events = Arc::new(RecordedEvents::default());
        let (tx, rx) = crossbeam_channel::unbounded::<Command>();
        let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(44_100 * 2 * 2);
        let buffered = Arc::new(AtomicU64::new(0));
        let position = Arc::new(AtomicU64::new(0));
        let duration = Arc::new(AtomicU64::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle = std::thread::spawn({
            let (events, buffered, position, duration, shutdown) = (
                events.clone(),
                buffered.clone(),
                position.clone(),
                duration.clone(),
                shutdown.clone(),
            );
            move || {
                run_mixer(
                    rx,
                    producer,
                    buffered,
                    position,
                    duration,
                    44_100,
                    events,
                    shutdown,
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(Mutex::new(NerdSnapshot::default())),
                )
            }
        });

        let (reply, result) = crossbeam_channel::bounded(1);
        tx.send(Command::Load {
            request: TrackSource {
                source: a.display().to_string(),
                title: "resume".into(),
                artist: String::new(),
                start_seconds: 180.0,
                plan: TransitionPlan::default(),
                headers: std::collections::HashMap::new(),
                claimed_kbps: 0,
                loudness_db: None,
                duration_seconds: 0.0,
            },
            reply,
        })
        .unwrap();
        result
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap()
            .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut first_samples = Vec::new();
        while std::time::Instant::now() < deadline && first_samples.len() < 256 {
            while first_samples.len() < 256 {
                match consumer.pop() {
                    Ok(sample) => first_samples.push(sample),
                    Err(_) => break,
                }
            }
            if first_samples.len() < 256 {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        assert_eq!(
            first_samples.len(),
            256,
            "mixer did not fill the output ring"
        );
        let peak = first_samples
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0, f32::max);
        assert!(peak > 0.1, "first ring PCM is silent (peak {peak})");
        let started_at = position.load(Ordering::Relaxed) as f64 / 1000.0;
        assert!(
            (179.9..181.0).contains(&started_at),
            "audible position should start at 180 s, got {started_at:.3} s"
        );

        shutdown.store(true, Ordering::Relaxed);
        let _ = handle.join();
        let _ = std::fs::remove_file(a);
    }

    #[test]
    fn silence_floor_uses_device_rate_not_source_rate() {
        // The bug: 44100 device frames at 96 kHz is 0.46 s, well under the
        // 1 s floor. Dividing by a 44.1 kHz source rate treated it as 1 s
        // and ate musical pauses.
        assert!(!super::silence_exceeds_floor(44_100, 96_000));
        assert!(!super::silence_exceeds_floor(48_000, 48_000));
        assert!(super::silence_exceeds_floor(48_001, 48_000));
        assert!(super::silence_exceeds_floor(96_001, 96_000));
    }

    #[test]
    fn silence_classifier_matches_media3_int16_threshold() {
        let just_under = 1023.0 / 32768.0;
        let just_over = 1025.0 / 32768.0;
        assert!(super::is_silent(&vec![just_under; 64]));
        assert!(!super::is_silent(&vec![just_over; 64]));
        let mut mixed = vec![0.0f32; 64];
        mixed[10] = just_over;
        assert!(!super::is_silent(&mixed));
        assert!(!super::is_silent(&[]));
    }

    #[test]
    fn cue_in_the_outro_starts_at_zero() {
        assert_eq!(super::clamp_start_seconds(0.0, 180.0), 0.0);
        assert_eq!(super::clamp_start_seconds(12.0, 180.0), 12.0);
        assert_eq!(super::clamp_start_seconds(176.0, 180.0), 0.0);
        assert_eq!(super::clamp_start_seconds(175.0, 180.0), 0.0);
        assert_eq!(super::clamp_start_seconds(174.9, 180.0), 174.9);
        assert_eq!(super::clamp_start_seconds(8.0, 0.0), 8.0);
    }

    /// A plan cannot talk the decoder into starting a record a minute in.
    ///
    /// Regression-shaped, and deliberately redundant with the planner's own
    /// bounds: the reason the deep cue reached the decoder at all is that every
    /// layer trusted the one above it, so the last layer before `seek_seconds`
    /// gets to disagree.
    #[test]
    fn a_planned_mix_in_cannot_reach_past_the_depth_bound() {
        let planned = TransitionPlan {
            cue_seconds: 45.0,
            ..TransitionPlan::default()
        };
        // The shape of the original bug: a plan asking to start a minute in.
        assert_eq!(super::bound_mix_in_depth(45.0, &planned, 210.0), 0.0);
        assert_eq!(super::bound_mix_in_depth(48.0, &planned, 210.0), 0.0);
        // A legal mix-in passes untouched, on either bound.
        let legal = TransitionPlan {
            cue_seconds: 6.0,
            ..TransitionPlan::default()
        };
        assert_eq!(super::bound_mix_in_depth(6.0, &legal, 210.0), 6.0);
        assert_eq!(super::bound_mix_in_depth(7.5, &legal, 210.0), 7.5);
        // The fraction is a floor for short material, not a ceiling for long:
        // eight seconds is a fifth of a one-minute clip and nothing at all in a
        // four-minute song.
        let eight = TransitionPlan {
            cue_seconds: 8.0,
            ..TransitionPlan::default()
        };
        assert_eq!(super::bound_mix_in_depth(8.0, &eight, 240.0), 8.0);
        assert_eq!(super::bound_mix_in_depth(8.0, &eight, 60.0), 0.0);
        assert_eq!(super::bound_mix_in_depth(3.5, &eight, 60.0), 3.5);
    }

    /// ...and a resume seek is none of its business.
    ///
    /// `start_seconds` carries two different intents: a mix-in point from the
    /// planner, and — from `loadCurrent(startAt:)` after a cold restore — a
    /// timestamp minutes into a track the listener was partway through. Only the
    /// first is a mix-in, and the tell is the plan: a resume carries none.
    #[test]
    fn a_resume_seek_is_not_treated_as_a_mix_in() {
        let no_plan = TransitionPlan::default();
        assert_eq!(no_plan.cue_seconds, 0.0);
        assert_eq!(super::bound_mix_in_depth(180.0, &no_plan, 240.0), 180.0);
        assert_eq!(super::bound_mix_in_depth(45.0, &no_plan, 210.0), 45.0);
        // And an unknown duration cannot be bounded, so it is passed through for
        // the outro rule to deal with.
        assert_eq!(
            super::bound_mix_in_depth(45.0, &planned_at(45.0), 0.0),
            45.0
        );
    }

    fn planned_at(cue: f64) -> TransitionPlan {
        TransitionPlan {
            cue_seconds: cue,
            ..TransitionPlan::default()
        }
    }

    /// A queued transition must *overlap*: the incoming track has to be
    /// audible while the outgoing one still has music left, or there is no
    /// transition, there is a cut.
    ///
    /// This is the whole claim Automix makes, asserted directly rather than
    /// inferred from gains. The handoff is the engine saying "the incoming is
    /// audible now", so what matters at that moment is how much of the outgoing
    /// track is still to play — if the answer is nothing, the two never
    /// overlapped and the listener heard a song end and a song start.
    #[test]
    fn a_planned_transition_overlaps_rather_than_waits_for_the_end() {
        let dir = std::env::temp_dir().join("bitchord-mixer-overlap");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        // Eight seconds each, so there is room to be early by a real margin.
        test_wav(&a, 8.0, 440.0);
        test_wav(&b, 8.0, 660.0);

        let events = std::sync::Arc::new(RecordedEvents::default());
        let (tx, rx) = crossbeam_channel::unbounded::<Command>();
        let (consumer_side, mut consumer) = rtrb::RingBuffer::<f32>::new(44_100 * 2 * 2);
        let buffered = std::sync::Arc::new(AtomicU64::new(0));
        let position = std::sync::Arc::new(AtomicU64::new(0));
        let duration = std::sync::Arc::new(AtomicU64::new(0));
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));

        let handle = std::thread::spawn({
            let events = events.clone();
            let buffered = buffered.clone();
            let position = position.clone();
            let duration = duration.clone();
            let shutdown = shutdown.clone();
            move || {
                run_mixer(
                    rx,
                    consumer_side,
                    buffered,
                    position,
                    duration,
                    44_100,
                    events,
                    shutdown,
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(Mutex::new(NerdSnapshot::default())),
                )
            }
        });

        // Two seconds of blend ending at 6 s into an 8 s track: the handoff must
        // land while two more seconds of the outgoing are still coming.
        let fade = 2.0f64;
        tx.send(Command::Load {
            request: TrackSource {
                source: a.display().to_string(),
                title: "A".into(),
                artist: String::new(),
                start_seconds: 0.0,
                plan: TransitionPlan::default(),
                headers: std::collections::HashMap::new(),
                claimed_kbps: 0,
                loudness_db: None,
                duration_seconds: 0.0,
            },
            reply: crossbeam_channel::bounded(1).0,
        })
        .unwrap();
        tx.send(Command::QueueNext {
            request: TrackSource {
                source: b.display().to_string(),
                title: "B".into(),
                artist: String::new(),
                start_seconds: 0.0,
                plan: TransitionPlan {
                    style: TransitionStyle::DjBlend,
                    bass_swap: false,
                    fade_seconds: fade,
                    effect_seconds: 0.0,
                    effect_start_seconds: 0.0,
                    transition_end_seconds: 6.0,
                    ..TransitionPlan::default()
                },
                headers: std::collections::HashMap::new(),
                claimed_kbps: 0,
                loudness_db: None,
                duration_seconds: 0.0,
            },
        })
        .unwrap();

        let at = drain_until_handoff(&mut consumer, &buffered, &position, &events, 20)
            .expect("no handoff fired");
        assert!(
            at <= 6.0 + 0.6,
            "the handoff fired at {at:.2}s, past the 6 s blend anchor"
        );
        assert!(
            at <= 8.0 - 0.5,
            "the handoff fired at {at:.2}s of an 8 s track — that is a cut at the end, not a mix"
        );

        shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
        tx.send(Command::Shutdown).ok();
        let _ = handle.join();
    }

    /// 116 BPM clicks, sped up to 120, stay on the outgoing 120 BPM grid through
    /// the overlap, then settle back to 116 once the post-blend glide ends.
    #[test]
    fn a_beatmatched_blend_holds_the_grid_then_glides_home() {
        let dir = std::env::temp_dir().join("bitchord-mixer-beats");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("out.wav");
        let b = dir.join("in.wav");
        click_wav(&a, 10.0, 120.0, true);
        click_wav(&b, 36.0, 116.0, false);

        let events = std::sync::Arc::new(RecordedEvents::default());
        let (tx, rx) = crossbeam_channel::unbounded::<Command>();
        let (consumer_side, mut consumer) = rtrb::RingBuffer::<f32>::new(44_100 * 2);
        let buffered = std::sync::Arc::new(AtomicU64::new(0));
        let position = std::sync::Arc::new(AtomicU64::new(0));
        let duration = std::sync::Arc::new(AtomicU64::new(0));
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let handle = std::thread::spawn({
            let events = events.clone();
            let buffered = buffered.clone();
            let position = position.clone();
            let duration = duration.clone();
            let shutdown = shutdown.clone();
            move || {
                run_mixer(
                    rx,
                    consumer_side,
                    buffered,
                    position,
                    duration,
                    44_100,
                    events,
                    shutdown,
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(Mutex::new(NerdSnapshot::default())),
                )
            }
        });

        let rate = 44_100usize;
        // Eight seconds of blend ending one second before the outgoing file,
        // so the bed (before the bass swap) is long enough to compare grids.
        let fade = 8.0;
        let end = 9.0;
        tx.send(Command::Load {
            request: TrackSource {
                source: a.display().to_string(),
                title: "A".into(),
                artist: String::new(),
                start_seconds: 0.0,
                plan: TransitionPlan::default(),
                headers: std::collections::HashMap::new(),
                claimed_kbps: 0,
                loudness_db: None,
                duration_seconds: 10.0,
            },
            reply: crossbeam_channel::bounded(1).0,
        })
        .unwrap();
        tx.send(Command::QueueNext {
            request: TrackSource {
                source: b.display().to_string(),
                title: "B".into(),
                artist: String::new(),
                start_seconds: 0.0,
                plan: TransitionPlan {
                    style: TransitionStyle::DjBlend,
                    bass_swap: true,
                    bass_swap_fraction: 0.6,
                    fade_seconds: fade,
                    effect_seconds: 0.0,
                    effect_start_seconds: 0.0,
                    transition_end_seconds: end,
                    playback_rate: 120.0 / 116.0,
                    bed_fraction: 0.45,
                    bed_gain_db: -14.0,
                    dip_depth: 0.0,
                    dip_width: 0.05,
                    // 32 beats of the matched 120 BPM grid.
                    post_glide_seconds: 32.0 * 0.5,
                    ..TransitionPlan::default()
                },
                headers: std::collections::HashMap::new(),
                claimed_kbps: 0,
                loudness_db: None,
                duration_seconds: 36.0,
            },
        })
        .unwrap();

        let want = rate * 28;
        let mut samples = Vec::with_capacity(want * 2);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while samples.len() / 2 < want && std::time::Instant::now() < deadline {
            match consumer.pop() {
                Ok(sample) => {
                    samples.push(sample);
                    if samples.len() % 2 == 0 {
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
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(2)),
            }
        }
        shutdown.store(true, Ordering::Relaxed);
        tx.send(Command::Shutdown).ok();
        let _ = handle.join();
        assert!(
            samples.len() / 2 > rate * 27,
            "only captured {:.1}s",
            samples.len() as f64 / 2.0 / rate as f64
        );

        let onsets = |channel: usize, from_s: f64, until_s: f64| -> Vec<usize> {
            let frames = samples.len() / 2;
            let start = ((from_s * rate as f64) as usize).min(frames);
            let end = ((until_s * rate as f64) as usize).min(frames);
            let mut peaks = Vec::new();
            let mut i = start.max(1);
            while i + 1 < end {
                let level = samples[i * 2 + channel].abs();
                if level > 0.05
                    && level >= samples[(i - 1) * 2 + channel].abs()
                    && level >= samples[(i + 1) * 2 + channel].abs()
                {
                    peaks.push(i);
                    i += rate / 10;
                } else {
                    i += 1;
                }
            }
            peaks
        };

        // Fade runs 1s..9s; the bass swap is at 0.6 of that, so 1.4s..5.4s is
        // still the bed — both records full-band, the incoming underneath.
        let left = onsets(0, 1.4, 5.4);
        let right = onsets(1, 1.4, 5.4);
        assert!(
            left.len() >= 4,
            "outgoing clicks in the overlap: {}",
            left.len()
        );
        assert!(
            right.len() >= 4,
            "incoming clicks in the overlap: {}",
            right.len()
        );
        let mut worst = 0.0f64;
        for beat in &left {
            let nearest = right
                .iter()
                .map(|other| (*other as isize - *beat as isize).unsigned_abs())
                .min()
                .unwrap_or(usize::MAX);
            let ms = nearest as f64 / rate as f64 * 1000.0;
            worst = worst.max(ms);
        }
        assert!(
            worst < 15.0,
            "beats drifted by {worst:.1} ms during the blend"
        );

        // The glide is 16 s from the end of the blend (t = 9), so past t = 25
        // the incoming record is back at its own 116 BPM.
        let home = onsets(1, 25.4, 27.6);
        assert!(
            home.len() >= 3,
            "not enough clicks after the glide: {}",
            home.len()
        );
        let expected = rate as f64 * 60.0 / 116.0;
        for pair in home.windows(2) {
            let gap = (pair[1] - pair[0]) as f64;
            let error = (gap - expected).abs() / expected;
            assert!(
                error < 0.02,
                "after the glide the spacing was {gap:.0} samples, {error:.3} off 116 BPM"
            );
        }
    }

    /// The engine's length for a track, and which of the two sources wins.
    ///
    /// The container's own figure is measured from the bytes being decoded and
    /// is the better answer; the caller's is the only answer when the container
    /// declares none, which is the case that decides whether a transition gets
    /// scheduled at all.
    #[test]
    fn a_container_without_a_duration_falls_back_to_the_callers_figure() {
        assert_eq!(
            super::known_duration(Some(210.0), 209.0),
            210.0,
            "the container wins"
        );
        assert_eq!(
            super::known_duration(Some(0.0), 210.0),
            210.0,
            "a zero is not a duration"
        );
        assert_eq!(
            super::known_duration(None, 210.0),
            210.0,
            "the caller fills the gap"
        );
        assert_eq!(
            super::known_duration(None, 0.0),
            0.0,
            "and unknown stays unknown"
        );
        assert_eq!(super::known_duration(None, f64::NAN), 0.0);
        assert_eq!(super::known_duration(Some(f64::NAN), 210.0), 210.0);
    }

    /// The mix-out anchor is a preferred musical point near the end, distinct
    /// from the recording's duration. That anchor schedules the fade; this
    /// duration still limits the fade and protects the recording's tail.
    #[test]
    fn the_recording_duration_takes_precedence_over_its_mix_out_anchor() {
        let dir = std::env::temp_dir().join("bitchord-mixer-anchor-duration");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("duration.wav");
        test_wav(&path, 10.0, 440.0);
        let request = TrackSource {
            source: path.display().to_string(),
            title: "A".into(),
            artist: String::new(),
            start_seconds: 0.0,
            plan: TransitionPlan {
                transition_end_seconds: 8.0,
                outgoing_duration_seconds: 10.0,
                fade_seconds: 1.0,
                effect_seconds: 0.0,
                effect_start_seconds: 0.0,
                ..TransitionPlan::default()
            },
            headers: std::collections::HashMap::new(),
            claimed_kbps: 0,
            loudness_db: None,
            duration_seconds: 10.0,
        };
        let voice = Voice::open(&request, false, 0.0, 44_100, 1.0, false, false).unwrap();
        let state = MixerState {
            current: Some(voice),
            incoming: None,
            pending_next: None,
            warned_no_duration: false,
            transition: None,
            retiring: Vec::new(),
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
            loudness_enabled: false,
            loudness_mode: LoudnessMode::Off,
            swap_probe: SwapProbe::default(),
        };
        let current = state.current.as_ref().unwrap();
        assert_eq!(state.outgoing_end_s(&request.plan, current), 8.0);
        let late_anchor = TransitionPlan {
            transition_end_seconds: 12.0,
            ..request.plan.clone()
        };
        assert_eq!(state.outgoing_end_s(&late_anchor, current), 10.0);
        assert_eq!(
            state.effective_fade_seconds(current.known_duration.unwrap()),
            10.0 / 3.0
        );
    }

    #[test]
    fn outgoing_eof_does_not_finish_an_incoming_fade_early() {
        let dir = std::env::temp_dir().join("bitchord-mixer-outgoing-eof");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("outgoing.wav");
        let b = dir.join("incoming.wav");
        test_wav(&a, 8.0, 440.0);
        test_wav(&b, 8.0, 660.0);

        let request = |path: &std::path::Path, title: &str| TrackSource {
            source: path.display().to_string(),
            title: title.into(),
            artist: String::new(),
            start_seconds: 0.0,
            plan: TransitionPlan::default(),
            headers: std::collections::HashMap::new(),
            claimed_kbps: 0,
            loudness_db: None,
            duration_seconds: 8.0,
        };
        let mut current =
            Voice::open(&request(&a, "A"), false, 0.0, 44_100, 1.0, false, false).unwrap();
        current.finished = true;
        let mut incoming =
            Voice::open(&request(&b, "B"), false, 0.0, 44_100, 1.0, false, false).unwrap();
        incoming.emitted_dev_frames = 2 * 44_100;

        let mut state = MixerState {
            current: Some(current),
            incoming: Some(incoming),
            pending_next: None,
            warned_no_duration: false,
            transition: Some(TransitionState {
                phase: Phase::Fading,
                fade_frames: 4 * 44_100,
                handed_off: true,
                plan: TransitionPlan::default(),
                arm_deadline: Instant::now(),
                swap: false,
                fx: None,
                effect_start_frame: 0,
                effect_input_gain: None,
            }),
            retiring: Vec::new(),
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
            loudness_enabled: false,
            loudness_mode: LoudnessMode::Off,
            swap_probe: SwapProbe::default(),
        };

        assert!(
            !state.drive_fade(),
            "outgoing EOF must not promote the incoming track before the fade completes"
        );
        let incoming_gain = state.incoming.as_ref().unwrap().gain;
        assert!(incoming_gain > 0.0 && incoming_gain < 1.0);

        state.incoming.as_mut().unwrap().emitted_dev_frames = 4 * 44_100;
        assert!(
            state.drive_fade(),
            "the fade completes at its planned duration"
        );
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

    #[test]
    fn crossfade_handoff_playhead_is_incoming_not_outgoing_tail() {
        let dir = std::env::temp_dir().join("bitchord-mixer-xfade-pos");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("out.wav");
        let b = dir.join("in.wav");
        test_wav(&a, 3.0, 440.0);
        test_wav(&b, 3.0, 660.0);

        let position = std::sync::Arc::new(AtomicU64::new(0));
        let events = std::sync::Arc::new(RecordedEvents {
            position_ms: Some(position.clone()),
            ..RecordedEvents::default()
        });
        let (tx, rx) = crossbeam_channel::unbounded::<Command>();
        let (consumer_side, mut consumer) = rtrb::RingBuffer::<f32>::new(44_100 * 2 * 4);
        let buffered = std::sync::Arc::new(AtomicU64::new(0));
        let duration = std::sync::Arc::new(AtomicU64::new(0));
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));

        let handle = std::thread::spawn({
            let events = events.clone();
            let buffered = buffered.clone();
            let position = position.clone();
            let duration = duration.clone();
            let shutdown = shutdown.clone();
            move || {
                run_mixer(
                    rx,
                    consumer_side,
                    buffered,
                    position,
                    duration,
                    44_100,
                    events,
                    shutdown,
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(Mutex::new(NerdSnapshot::default())),
                )
            }
        });

        tx.send(Command::SetCrossfadeWindow(1.0)).unwrap();
        tx.send(Command::Load {
            request: TrackSource {
                source: a.display().to_string(),
                title: "A".into(),
                artist: String::new(),
                start_seconds: 0.0,
                plan: TransitionPlan::default(),
                headers: std::collections::HashMap::new(),
                claimed_kbps: 0,
                loudness_db: None,
                duration_seconds: 0.0,
            },
            reply: crossbeam_channel::bounded(1).0,
        })
        .unwrap();
        tx.send(Command::QueueNext {
            request: TrackSource {
                source: b.display().to_string(),
                title: "B".into(),
                artist: String::new(),
                start_seconds: 0.0,
                plan: TransitionPlan::default(),
                headers: std::collections::HashMap::new(),
                claimed_kbps: 0,
                loudness_db: None,
                duration_seconds: 0.0,
            },
        })
        .unwrap();

        drain_until_handoff(&mut consumer, &buffered, &position, &events, 20);

        let handoffs = events.handoffs.lock().unwrap().clone();
        assert_eq!(handoffs, vec!["B".to_string()], "handoff to B must fire");
        let at = events.position_at_handoff.lock().unwrap().clone();
        assert_eq!(at.len(), 1);
        // Outgoing is 3 s; fade starts with ~1 s left, so the old playhead
        // would be ~2000 ms. Incoming must be at (or very near) its cue.
        assert!(
            at[0] < 1_500,
            "handoff playhead {} ms is the outgoing tail, not the incoming start",
            at[0]
        );

        shutdown.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }

    /// The blend has to end where the *plan* says, not at the file's last byte.
    ///
    /// Regression: the mixer armed the fade from `duration`, so a plan whose
    /// anchor sits inside the track — which is the whole point of the ranked
    /// mix-out anchor, and is allowed to be 12 s early — had its entire blend
    /// pushed to the end, taking the bass swap and both filter rides with it.
    ///
    /// The ring is deliberately small here so the decoder cannot run far ahead
    /// of what has been drained, which is what makes the count below a direct
    /// reading of "how far into the outgoing track the blend started".
    #[test]
    fn the_blend_ends_at_the_plans_anchor_not_the_file_end() {
        let dir = std::env::temp_dir().join("bitchord-mixer-anchor");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("out.wav");
        let b = dir.join("in.wav");
        test_wav(&a, 10.0, 440.0);
        test_wav(&b, 10.0, 660.0);

        let events = std::sync::Arc::new(RecordedEvents::default());
        let (tx, rx) = crossbeam_channel::unbounded::<Command>();
        // ~250 ms: small enough that pre-buffering cannot mask the anchor.
        let (consumer_side, mut consumer) = rtrb::RingBuffer::<f32>::new(44_100 / 4 * 2);
        let buffered = std::sync::Arc::new(AtomicU64::new(0));
        let position = std::sync::Arc::new(AtomicU64::new(0));
        let duration = std::sync::Arc::new(AtomicU64::new(0));
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let handle = std::thread::spawn({
            let events = events.clone();
            let buffered = buffered.clone();
            let position = position.clone();
            let duration = duration.clone();
            let shutdown = shutdown.clone();
            move || {
                run_mixer(
                    rx,
                    consumer_side,
                    buffered,
                    position,
                    duration,
                    44_100,
                    events,
                    shutdown,
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(AtomicBool::new(false)),
                    std::sync::Arc::new(Mutex::new(NerdSnapshot::default())),
                )
            }
        });

        let source = |path: &std::path::Path, title: &str, plan: TransitionPlan| TrackSource {
            source: path.display().to_string(),
            title: title.into(),
            artist: String::new(),
            start_seconds: 0.0,
            plan,
            headers: std::collections::HashMap::new(),
            claimed_kbps: 0,
            loudness_db: None,
            duration_seconds: 0.0,
        };

        tx.send(Command::SetCrossfadeWindow(1.0)).unwrap();
        tx.send(Command::Load {
            request: source(&a, "A", TransitionPlan::default()),
            reply: crossbeam_channel::bounded(1).0,
        })
        .unwrap();
        // A 10 s outgoing track, but the blend is wanted 3 s in with a 1 s fade,
        // so it must start at roughly 2 s of outgoing audio — not at 9 s.
        tx.send(Command::QueueNext {
            request: source(
                &b,
                "B",
                TransitionPlan {
                    fade_seconds: 1.0,
                    effect_seconds: 0.0,
                    effect_start_seconds: 0.0,
                    transition_end_seconds: 3.0,
                    ..TransitionPlan::default()
                },
            ),
        })
        .unwrap();

        let mut drained = 0usize;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            while consumer.pop().is_ok() {
                drained += 1;
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
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        assert!(
            !events.handoffs.lock().unwrap().is_empty(),
            "the blend must happen at all"
        );
        let started_at = drained as f64 / 2.0 / 44_100.0;
        assert!(
            started_at < 5.0,
            "the blend started {started_at:.2}s in, which is the file end — the plan's anchor was ignored"
        );
        assert!(
            started_at > 0.5,
            "the blend started {started_at:.2}s in, implausibly early"
        );

        shutdown.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }

    /// The two crossfade curves, and why they have to differ.
    ///
    /// Regression: a source swap used the equal-power pair, so the same
    /// recording on both sides — correlated, and aligned by construction —
    /// summed to √2 through the middle of the fade, and every quality upgrade
    /// was heard as the volume lifting and settling back.
    #[test]
    fn swap_fade_is_level_flat_while_a_track_fade_is_power_flat() {
        let mut worst_swap_db = 0.0f64;
        let mut worst_track_db = 0.0f64;
        for step in 0..=1000 {
            let p = step as f64 / 1000.0;

            // Correlated sides: amplitudes add, so the sum must stay at 1.
            let (rise, fall) = fade_gains(p, true);
            let sum = rise as f64 + fall as f64;
            worst_swap_db = worst_swap_db.max(20.0 * sum.log10().abs());

            // Uncorrelated sides: powers add, so the power sum must stay at 1.
            let (rise, fall) = fade_gains(p, false);
            let power = (rise as f64).powi(2) + (fall as f64).powi(2);
            worst_track_db = worst_track_db.max(10.0 * power.log10().abs());
        }
        assert!(
            worst_swap_db < 1e-4,
            "swap fade swells by {worst_swap_db} dB"
        );
        assert!(
            worst_track_db < 1e-4,
            "track fade dips by {worst_track_db} dB"
        );

        // What a plain equal-power pair costs when both sides are the same
        // signal — recorded so the branch above cannot quietly be undone, and
        // so the reason the incoming no longer uses one is written down where
        // the arithmetic is.
        let quarter = core::f64::consts::FRAC_PI_4;
        let bump_db = 20.0 * (quarter.sin() + quarter.cos()).log10();
        assert!(
            (bump_db - 3.01).abs() < 0.02,
            "expected the √2 bump at the midpoint, got {bump_db} dB"
        );
    }

    /// A plain crossfade is equal-power: silent at the open, unity at the
    /// close, and level-flat the whole way. A DJ blend is a different curve.
    #[test]
    fn a_plain_crossfade_is_equal_power_and_starts_from_silence() {
        let (first_rise, first_fall) = fade_gains(0.0, false);
        let (last_rise, last_fall) = fade_gains(1.0, false);
        assert!(
            first_rise.abs() < 1e-6,
            "a crossfade opens on silence, got {first_rise}"
        );
        assert!((first_fall - 1.0).abs() < 1e-6);
        assert!((last_rise - 1.0).abs() < 1e-6);
        assert!(last_fall.abs() < 1e-6);
        let mut worst_db = 0.0f64;
        for step in 0..=1000 {
            let (rise, fall) = fade_gains(step as f64 / 1000.0, false);
            let power = (rise as f64).powi(2) + (fall as f64).powi(2);
            worst_db = worst_db.max(10.0 * power.log10().abs());
        }
        assert!(
            worst_db < 1e-4,
            "equal-power moved the level by {worst_db} dB"
        );
    }

    /// E-2: the outgoing holds unity through the final chord's attack, the
    /// incoming waits underneath, and the two cross over the decay without the
    /// summed level lifting or dipping by even a decibel.
    #[test]
    fn an_effect_intro_has_both_records_playing_before_the_throw() {
        let hold = 0.6;
        // Across the intro the outgoing is at unity and the incoming climbs
        // to its intro level, reaching it exactly at the throw.
        let (mid_rise, mid_fall) = effect_intro_gains(0.3, hold, -4.0);
        assert_eq!(mid_fall, 1.0);
        assert!(mid_rise > 0.2, "the next record is audible mid-intro: {mid_rise}");
        let (at_throw, fall) = effect_intro_gains(hold, hold, -4.0);
        assert!((at_throw as f64 - db_to_gain(-4.0)).abs() < 1e-4);
        assert_eq!(fall, 1.0);
        // No step anywhere, and unity at the end.
        let mut last = 0.0f32;
        for step in 0..=1000 {
            let (rise, _) = effect_intro_gains(step as f64 / 1000.0, hold, -4.0);
            assert!(rise >= last - 1e-6 && rise - last < 0.01, "step at {step}");
            last = rise;
        }
        assert!((last - 1.0).abs() < 1e-6);
        // The bass waits for the landing; the top opens across the throw.
        let mut filter = TransitionFilter::new(2, 48_000);
        let landing = effect_landing(
            &TransitionPlan { style: TransitionStyle::EchoOut, beat_seconds: 0.5, ..Default::default() },
            hold,
        );
        assert!(landing > hold && landing < 1.0);
        ride_effect_intro(0.3, hold, landing, 1.0, &mut filter);
        assert!(filter.active(), "filtered under the outgoing");
    }

    #[test]
    fn a_cold_ending_holds_the_chord_at_unity() {
        // Pair 5's shape: a 3.36 s phrase inside a 4.86 s blend, so the chord's
        // decay is the last 1.5 s.
        let hold = 3.36 / 4.86;
        let (rise, fall) = cold_end_gains(hold, hold);
        assert!(
            (fall - 1.0).abs() < 1e-6,
            "the final chord is faded through: {fall}"
        );
        assert!(
            (rise as f64 - db_to_gain(COLD_END_UNDERLAY_DB)).abs() < 1e-6,
            "the incoming is not underneath at the chord: {rise}"
        );
        // Relative to the incoming's own matched level, never an absolute one.
        let underlay_db = 20.0 * (rise as f64).log10();
        assert!(
            (underlay_db + 12.0).abs() < 0.01,
            "the underlay is {underlay_db:.2} dB, not −12 dB under the matched level"
        );
        // It has started to leave immediately after the chord.
        let (_, after) = cold_end_gains(hold + 0.02, hold);
        assert!(
            after < 0.999,
            "the outgoing did not begin to fall after the chord: {after}"
        );
        // No +3 dB bump while the outgoing is held: the underlay is what keeps
        // the sum near unity, and this is the assertion that stops it drifting.
        let mut worst_db = 0.0f64;
        for step in 0..=1000 {
            let (rise, fall) = cold_end_gains(step as f64 / 1000.0, hold);
            let power = (rise as f64).powi(2) + (fall as f64).powi(2);
            worst_db = worst_db.max(10.0 * power.log10().abs());
        }
        assert!(
            worst_db < 1.0,
            "the cold-ending hold moved the summed level by {worst_db} dB"
        );
        let (end_rise, end_fall) = cold_end_gains(1.0, hold);
        assert!((end_rise - 1.0).abs() < 1e-6 && end_fall.abs() < 1e-6);
        // A plan without a hold keeps the plain equal-power pair.
        let (plain_rise, plain_fall) = cold_end_gains(0.5, 0.0);
        let (fade_rise, fade_fall) = fade_gains(0.5, false);
        assert!((plain_rise - fade_rise).abs() < 1e-6);
        assert!((plain_fall - fade_fall).abs() < 1e-6);
    }

    /// Bed, then a rise, then the outgoing lets go. The incoming is already
    /// audible while the outgoing is still at full level, and a one-beat dip
    /// sits just before the swap.
    #[test]
    fn a_dj_blend_beds_the_incoming_track_then_swaps() {
        let plan = TransitionPlan {
            style: TransitionStyle::DjBlend,
            bass_swap: true,
            bass_swap_fraction: 0.62,
            bed_fraction: 0.4,
            bed_gain_db: -14.0,
            dip_depth: 0.5,
            dip_width: 0.08,
            fade_seconds: 12.0,
            effect_seconds: 0.0,
            effect_start_seconds: 0.0,
            ..TransitionPlan::default()
        };
        let bed = 10.0_f64.powf(-14.0 / 20.0) as f32;
        let (open_rise, open_fall) = automix_gains(0.0, &plan, 0.0);
        assert!(
            open_rise.abs() < 1e-4,
            "the bed eases in from silence, got {open_rise}"
        );
        assert!(
            (open_fall - 1.0).abs() < 1e-4,
            "the outgoing stays up, got {open_fall}"
        );

        let (bed_rise, bed_fall) = automix_gains(0.2, &plan, 0.0);
        assert!(
            bed_rise > bed * 0.5 && bed_rise < 0.45,
            "early in the blend the incoming is underneath, got {bed_rise}"
        );
        assert!(
            (bed_fall - 1.0).abs() < 1e-4,
            "the outgoing is still the record"
        );

        let (late_rise, late_fall) = automix_gains(0.95, &plan, 0.0);
        assert!(
            late_rise > 0.95,
            "after the swap the incoming is the record, got {late_rise}"
        );
        assert!(
            late_fall < 0.35,
            "after the swap the outgoing is leaving, got {late_fall}"
        );

        // The dip is on the incoming, just before the swap, and the outgoing
        // gain at that moment is still full. Sample a half-width either side
        // of the notch so the comparison is the dip, not the climb into it.
        let before = automix_gains(0.54, &plan, 0.0).0;
        let notch = automix_gains(0.58, &plan, 0.0).0;
        let after = automix_gains(0.70, &plan, 0.0).0;
        assert!(
            notch < before * 0.85 && after > notch,
            "expected a dip then a recovery, got {before} -> {notch} -> {after}"
        );
        let outgoing_at_dip = automix_gains(0.58, &plan, 0.0).1;
        assert!(
            outgoing_at_dip > 0.75,
            "the dip is on the incoming; the outgoing is still leaving slowly, got {outgoing_at_dip}"
        );
    }

    /// A handoff stretch that is stepped back to unity at the end of a blend is
    /// a pitch step, because the stretch is a resample.
    ///
    /// Regression-shaped: `release_plan_stretch` used to fire at the fade's end
    /// and land the rate on 1.0 in a single frame — a 3 % stretch is 51 cents,
    /// arriving in one sample. The glide walks there instead, so the release
    /// finds unity already reached and changes nothing.
    #[test]
    fn the_handoff_tempo_glide_lands_on_unity_rather_than_stepping() {
        let base = 1.03f64;
        assert!(
            (base - 1.0).abs() > 1e-4,
            "this test is about a non-unity rate"
        );
        // Monotone, and strictly inside the range the whole way.
        let mut previous = f64::INFINITY;
        for step in 0..=1000 {
            let p = step as f64 / 1000.0;
            let target = glided_plan_rate(base, p);
            assert!(
                target <= previous + 1e-12 && target >= 1.0 - 1e-12,
                "glide at {p} was {target}, outside (1.0, {previous}]"
            );
            previous = target;
        }
        // Exactly unity at the end, so `release_plan_stretch`'s early-out is
        // what actually runs and there is nothing to snap.
        assert_eq!(glided_plan_rate(base, 1.0), 1.0);
        // A unity plan is not glided at all.
        assert_eq!(glided_plan_rate(1.0, 0.5), 1.0);
    }

    /// A beatmatched handoff stretches the incoming track so the two share a
    /// grid for the blend. That stretch must not survive the blend.
    ///
    /// Regression: the port baked `plan_rate` into the voice's speed resampler
    /// at `Voice::open` and nothing ever cleared it, so every track that
    /// arrived through a beatmatched or phrase-switch blend played ~3 % fast for
    /// its entire length — +51 cents, with the published position running at the
    /// stretched rate too. Upstream resets the player's speed in both
    /// `finish()` and `retire()`.
    ///
    /// The stretch is a WSOLA stage now rather than a rate change, so the
    /// assertion is on the stage: engaged at the plan's rate while the blend
    /// runs, gone and drained once it ends.
    #[test]
    fn a_promoted_voice_drops_the_handoff_tempo_stretch() {
        let dir = std::env::temp_dir().join("bitchord-mixer-stretch");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");
        test_wav(&path, 4.0, 440.0);

        let request = TrackSource {
            source: path.display().to_string(),
            title: "A".into(),
            artist: String::new(),
            start_seconds: 0.0,
            // What the planner hands a beatmatched handoff.
            plan: TransitionPlan {
                playback_rate: 1.03,
                ..TransitionPlan::default()
            },
            headers: std::collections::HashMap::new(),
            claimed_kbps: 0,
            loudness_db: None,
            duration_seconds: 0.0,
        };
        let mut voice = Voice::open(&request, false, 0.0, 44_100, 1.0, false, false).unwrap();
        // The listener's own speed is untouched by the handoff — that is the
        // point of routing the stretch through its own stage. It used to be
        // multiplied by the plan rate, which is the bug in its other form.
        assert!(
            (voice.effective_speed - 1.0).abs() < 1e-6,
            "the handoff must not touch the speed control, got {}",
            voice.effective_speed
        );
        // And nothing is running yet: the stage is for a blend, and this voice
        // has not entered one. A `Load` — a tap on the queue, a skip — carries
        // the same plan record, and engaging here would leave the track playing
        // 3 % slow for its whole length with nothing to release it.
        assert!(
            voice.stretch.is_none(),
            "a fresh open must not engage the handoff stretch"
        );
        assert_eq!(
            voice.base_plan_rate, 1.03,
            "but the plan's ratio is remembered"
        );

        // The blend engages it...
        voice.set_plan_rate(voice.base_plan_rate);
        assert!(voice.stretch.is_some());
        assert!((voice.stretch.as_ref().unwrap().rate() - 1.03).abs() < 1e-9);

        // The blend holds the rate. Walking it during the fade is the flam.
        assert!((voice.plan_rate - 1.03).abs() < 1e-9);
        // With no post-blend glide requested, release is immediate and arms
        // the drain. The stage is still holding a frame; dropping it there
        // would punch a hole. It goes on the next pull.
        voice.release_plan_stretch(1.0, 44_100);
        assert!(
            voice.stretch_draining,
            "release must drain the stage rather than drop it"
        );
        assert!(
            (voice.effective_speed - 1.0).abs() < 1e-6,
            "the stretch outlived the blend: {}",
            voice.effective_speed
        );
        assert!((voice.plan_rate - 1.0).abs() < 1e-9);
        let _ = voice.pull(512, 44_100);
        assert!(!voice.stretch_draining);
        assert!(voice.stretch.is_none(), "the stretch outlived the blend");

        // A later user speed change must not resurrect it.
        voice.set_playback_speed(1.25, 44_100);
        assert!(
            (voice.effective_speed - 1.25).abs() < 1e-6,
            "a speed change re-stacked the stretch: {}",
            voice.effective_speed
        );
        assert!(
            voice.stretch.is_none(),
            "a speed change must not bring the handoff stage back"
        );
    }

    /// Retiring the stretch must not swallow the samples it was still holding.
    ///
    /// The stage buffers roughly a frame of input it has not placed. Dropping it
    /// on release would punch a ~12 ms hole in the track at the exact moment the
    /// blend hands it over and its gain reaches unity — the worst possible place
    /// for a discontinuity, and one no level assertion on the blend would catch
    /// because the blend is already over.
    #[test]
    fn retiring_the_stretch_drains_rather_than_drops() {
        let dir = std::env::temp_dir().join("bitchord-mixer-stretch");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drain.wav");
        test_wav(&path, 3.0, 440.0);

        let request = TrackSource {
            source: path.display().to_string(),
            title: "A".into(),
            artist: String::new(),
            start_seconds: 0.0,
            plan: TransitionPlan {
                playback_rate: 1.03,
                ..TransitionPlan::default()
            },
            headers: std::collections::HashMap::new(),
            claimed_kbps: 0,
            loudness_db: None,
            duration_seconds: 0.0,
        };
        let mut voice = Voice::open(&request, false, 0.0, 44_100, 1.0, false, false).unwrap();
        // Run the blend part of the way so the stage has something buffered.
        for _ in 0..40 {
            let frames = voice.pull(512, 44_100);
            assert!(!frames.is_empty(), "the voice stopped producing audio");
        }
        voice.set_plan_rate(voice.base_plan_rate);
        voice.release_plan_stretch(1.0, 44_100);
        let before = voice.emitted_dev_frames;
        let frames = voice.pull(512, 44_100);
        assert!(
            !frames.is_empty(),
            "the drain must hand over what the stage was holding, not return nothing"
        );
        assert!(
            voice.emitted_dev_frames > before,
            "the drained samples were never emitted"
        );
        assert!(
            voice.stretch.is_none(),
            "the stage should be gone after the drain"
        );
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

    /// How well a swap actually lines up.
    ///
    /// Swapping to a byte-identical copy makes alignment directly measurable:
    /// if the two sides are sample-aligned and the fade is linear, the blend
    /// *is* the original signal at unity and the level cannot move. Whatever
    /// deviation shows up here is comb cancellation — what a listener hears as
    /// a hollow, phasey moment instead of a quality change.
    #[test]
    fn swap_to_an_identical_copy_reports_its_alignment() {
        let (mut harness, low, _) = SwapHarness::new("swap-level");
        let copy = low.with_file_name("low-copy.wav");
        std::fs::copy(&low, &copy).unwrap();

        harness
            .tx
            .send(Command::Load {
                request: SwapHarness::source(&low, "Low", 96),
                reply: crossbeam_channel::bounded(1).0,
            })
            .unwrap();
        drain_frames(&mut harness.consumer, &harness.buffered, 44_100);
        let before = rms_frames(&mut harness.consumer, &harness.buffered, 44_100 / 4);
        assert!(before > 0.0, "the track must be producing audio");

        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::SwapSource {
                request: SwapHarness::source(&copy, "Copy", 320),
                crossfade_seconds: SWAP_CROSSFADE_MS / 1000.0,
                reply: reply_tx,
            })
            .unwrap();
        reply_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the mixer must answer a swap")
            .expect("the swap must be accepted");

        // Short windows, so a 550 ms fade cannot be averaged away by the
        // single-voice audio either side of it. Drain until the probe reports,
        // which is the moment the fade completed.
        let mut worst_db = 0.0f64;
        let mut correlation: Option<f64> = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            let window = rms_frames(&mut harness.consumer, &harness.buffered, 44_100 / 20);
            if window > 0.0 {
                let db = 20.0 * (window / before).log10();
                if db.abs() > worst_db.abs() {
                    worst_db = db;
                }
            }
            correlation = harness.nerd.lock().unwrap().swap_correlation;
            if correlation.is_some() {
                break;
            }
        }
        eprintln!(
            "[swap-alignment] worst level deviation through the fade: {worst_db:.2} dB, rho={correlation:?}"
        );
        // Measured at 0.00 dB: the two sides are sample-aligned and the linear
        // fade makes the blend the original signal. 2 dB leaves room for a
        // future rate change between the two sources while still catching a
        // genuine misalignment, which reads as a deep comb notch.
        assert!(
            worst_db.abs() < 2.0,
            "the swap is misaligned by {worst_db} dB of comb"
        );
        // The probe has to be able to say "these are the same signal" — that is
        // the measurement everything else here rests on.
        let rho = correlation.expect("a completed swap must report a correlation");
        assert!(
            rho > 0.99,
            "two byte-identical copies must correlate at 1.0, got {rho}"
        );
        harness.finish();
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

    #[test]
    fn stale_preparation_cannot_modify_a_new_selection() {
        let (mut harness, low, high) = SwapHarness::new("stale-preparation");
        let (reply, loaded) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::Load {
                request: SwapHarness::source(&high, "Selected", 320),
                reply,
            })
            .unwrap();
        loaded
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let (reply, swapped) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::ForSource {
                source: low.display().to_string(),
                command: Box::new(Command::SwapSource {
                    request: SwapHarness::source(&low, "Obsolete", 96),
                    crossfade_seconds: 0.1,
                    reply,
                }),
            })
            .unwrap();
        assert!(swapped
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err());
        harness
            .tx
            .send(Command::ForSource {
                source: low.display().to_string(),
                command: Box::new(Command::QueueNext {
                    request: SwapHarness::source(&low, "Obsolete", 96),
                }),
            })
            .unwrap();
        drain_frames(&mut harness.consumer, &harness.buffered, 44_100 * 10);
        assert!(
            harness.events.handoffs.lock().unwrap().is_empty(),
            "stale queued audio must never become audible"
        );
        harness.finish();
    }

    /// A manual Next must not re-resolve: the upcoming track is already armed,
    /// so the skip promotes it now — peek names it, the skip opens it and
    /// reports the handoff, all without a catalogue round-trip.
    #[test]
    fn skip_promotes_queued_track_immediately() {
        let (harness, low, high) = SwapHarness::new("skip-queued");
        let (reply, loaded) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::Load {
                request: SwapHarness::source(&low, "First", 96),
                reply,
            })
            .unwrap();
        loaded
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        harness
            .tx
            .send(Command::QueueNext {
                request: SwapHarness::source(&high, "Second", 320),
            })
            .unwrap();
        // The peek names the armed successor without touching it.
        let (reply, peeked) = crossbeam_channel::bounded(1);
        harness.tx.send(Command::PendingTrack { reply }).unwrap();
        let queued = peeked
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .expect("a queued track must be visible to the skip peek");
        assert_eq!(queued.title, "Second");
        // The skip opens it now and reports the handoff, like a load would.
        let (reply, skipped) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::SkipToPending {
                skip_mix_enabled: true,
                reply,
            })
            .unwrap();
        let info = skipped
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .expect("an armed track must be skippable");
        assert_eq!(info.title, "Second");
        assert!(
            harness
                .events
                .handoffs
                .lock()
                .unwrap()
                .contains(&"Second".to_string()),
            "the skip must report the handoff the app flips its queue on"
        );
        // Nothing left armed: a second skip is refused, not invented.
        let (reply, again) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::SkipToPending {
                skip_mix_enabled: false,
                reply,
            })
            .unwrap();
        assert!(again.recv_timeout(Duration::from_secs(5)).unwrap().is_err());
        harness.finish();
    }

    #[test]
    fn skip_with_nothing_armed_is_refused() {
        let (harness, _, _) = SwapHarness::new("skip-empty");
        let (reply, peeked) = crossbeam_channel::bounded(1);
        harness.tx.send(Command::PendingTrack { reply }).unwrap();
        assert!(peeked
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_none());
        let (reply, skipped) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::SkipToPending {
                skip_mix_enabled: false,
                reply,
            })
            .unwrap();
        assert!(
            skipped
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .is_err(),
            "with no armed successor the caller must fall back to a full load"
        );
        harness.finish();
    }

    #[test]
    fn source_swap_without_a_current_track_is_refused() {
        let (harness, low, _) = SwapHarness::new("swap-refuse");
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::SwapSource {
                request: SwapHarness::source(&low, "Low", 96),
                crossfade_seconds: SWAP_CROSSFADE_MS / 1000.0,
                reply: reply_tx,
            })
            .unwrap();
        let reply = reply_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the mixer must answer a swap");
        assert!(
            reply.is_err(),
            "nothing is playing, so there is nothing to swap"
        );
        harness.finish();
    }

    /// The regression this guards: upgrading the source used to be a `loadTrack`
    /// — stop, open, seek — which put an audible hole in the song and reset the
    /// playhead. A swap must instead crossfade in place, keep the timeline, and
    /// never be reported as the queue moving on.
    #[test]
    fn source_swap_promotes_the_replacement_without_a_handoff() {
        let (mut harness, low, _) = SwapHarness::new("swap-crossfade");
        let high = low.with_file_name("high.wav");
        std::fs::copy(&low, &high).unwrap();
        harness
            .tx
            .send(Command::Load {
                request: SwapHarness::source(&low, "Low", 96),
                reply: crossbeam_channel::bounded(1).0,
            })
            .unwrap();

        drain_frames(&mut harness.consumer, &harness.buffered, 44_100);
        let _ = rms_frames(&mut harness.consumer, &harness.buffered, 44_100 / 4);
        let before = harness.position.load(Ordering::Relaxed);
        assert!(before > 0, "the track must be advancing before the swap");

        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::SwapSource {
                request: SwapHarness::source(&high, "High", 320),
                crossfade_seconds: SWAP_CROSSFADE_MS / 1000.0,
                reply: reply_tx,
            })
            .unwrap();
        let reply = reply_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the mixer must answer a swap");
        assert!(reply.is_ok(), "swap must be accepted, got {reply:?}");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            let _ = rms_frames(&mut harness.consumer, &harness.buffered, 44_100 / 20);
            if harness.nerd.lock().unwrap().kbps == 320 {
                break;
            }
        }

        let kbps = harness.nerd.lock().unwrap().kbps;
        assert_eq!(
            kbps, 320,
            "the promoted source must be the one the readout names"
        );
        assert!(
            harness.events.handoffs.lock().unwrap().is_empty(),
            "a source swap is not a track change and must not fire a handoff"
        );
        let after = harness.position.load(Ordering::Relaxed);
        assert!(
            after >= before,
            "the playhead moved backwards across a swap: {before} ms -> {after} ms"
        );
        harness.finish();
    }

    /// When a candidate replacement is a mismatched recording or corrupt transcode
    /// (Pearson correlation rho < 0.85), the source swap must be rejected:
    /// the replacement voice is dropped, the original track remains playing at unity gain,
    /// and the nerd readout does not promote to the new source.
    #[test]
    fn source_swap_with_mismatched_recording_is_rejected() {
        let (mut harness, low, _) = SwapHarness::new("swap-rejected");
        let dir = low.parent().unwrap();
        let mismatched = dir.join("mismatched.wav");
        test_wav(&mismatched, 8.0, 660.0); // 660 Hz vs 440 Hz -> rho near 0

        harness
            .tx
            .send(Command::Load {
                request: SwapHarness::source(&low, "Low", 96),
                reply: crossbeam_channel::bounded(1).0,
            })
            .unwrap();

        drain_frames(&mut harness.consumer, &harness.buffered, 44_100 * 2);

        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::SwapSource {
                request: SwapHarness::source(&mismatched, "Mismatched", 320),
                crossfade_seconds: SWAP_CROSSFADE_MS / 1000.0,
                reply: reply_tx,
            })
            .unwrap();
        let reply = reply_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the mixer must answer a swap");
        assert!(reply.is_ok(), "swap initiation should succeed");

        // Drain enough frames for the 550ms crossfade to run and finish_transition to trigger
        for _ in 0..40 {
            drain_frames(&mut harness.consumer, &harness.buffered, 44_100 / 4);
            if harness.nerd.lock().unwrap().swap_correlation.is_some() {
                break;
            }
        }

        let (rho, kbps) = {
            let nerd = harness.nerd.lock().unwrap();
            (
                nerd.swap_correlation
                    .expect("swap correlation must be measured"),
                nerd.kbps,
            )
        };
        assert!(
            rho < 0.85,
            "mismatched 440Hz vs 660Hz must yield rho < 0.85, got {rho}"
        );
        assert_eq!(
            kbps, 96,
            "kbps must NOT be promoted to 320 when swap is rejected"
        );
        harness.finish();
    }

    /// A route change replaces the ring. Everything between the playhead and
    /// the decoder's read head is in the abandoned ring and nothing has played
    /// it, so the voice has to be re-pointed at the playhead — otherwise the
    /// listener loses the whole ring depth, seconds of music, the moment their
    /// headphones disconnect.
    #[test]
    fn output_format_change_resumes_at_the_playhead() {
        let (mut harness, low, _) = SwapHarness::new("route-change");
        harness
            .tx
            .send(Command::Load {
                request: SwapHarness::source(&low, "Low", 96),
                reply: crossbeam_channel::bounded(1).0,
            })
            .unwrap();

        // Long enough that the playhead is well inside the track and the ring
        // is deep enough for a skip to be unmistakable.
        drain_frames(&mut harness.consumer, &harness.buffered, 44_100 * 2);
        let before = harness.position.load(Ordering::Relaxed);
        assert!(before > 0, "the track must be advancing");

        // Replace the output exactly as a route-change rebuild does.
        let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(48_000 * 2 * 2);
        harness
            .tx
            .send(Command::SetOutputFormat {
                rate: 48_000,
                producer,
                generation: 1,
                acknowledged: None,
            })
            .unwrap();

        // The old consumer is dead now; drain the replacement.
        drain_frames(&mut consumer, &harness.buffered, 48_000 / 2);
        let after = harness.position.load(Ordering::Relaxed);

        assert!(
            after + 500 >= before,
            "the playhead restarted instead of continuing: {before} ms -> {after} ms"
        );
        assert!(
            after < before + 1_500,
            "the playhead skipped the abandoned ring: {before} ms -> {after} ms"
        );
        harness.finish();
    }

    /// Skipping a track (Command::Load) must trigger a deferred bail flush
    /// rather than an immediate hard cut of the ring, so the audio ramps out
    /// over 120 ms without a click.
    #[test]
    fn skip_load_triggers_bail_flush_not_instant_flush() {
        let (mut harness, low, high) = SwapHarness::new("skip-bail");
        harness
            .tx
            .send(Command::Load {
                request: SwapHarness::source(&low, "Low", 96),
                reply: crossbeam_channel::bounded(1).0,
            })
            .unwrap();

        // Drain some frames so playback is active.
        drain_frames(&mut harness.consumer, &harness.buffered, 44_100);

        // Reset the bail_flush and flush_ring flags in harness
        harness.bail_flush.store(false, Ordering::Relaxed);
        harness.flush_ring.store(false, Ordering::Relaxed);

        // Issue a skip by loading a new track.
        harness
            .tx
            .send(Command::Load {
                request: SwapHarness::source(&high, "High", 320),
                reply: crossbeam_channel::bounded(1).0,
            })
            .unwrap();

        // Wait for mixer to process Command::Load.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !harness.bail_flush.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        assert!(
            harness.bail_flush.load(Ordering::Acquire),
            "Command::Load must set bail_flush for the 120 ms ramp"
        );
        assert!(
            !harness.flush_ring.load(Ordering::Acquire),
            "Command::Load must not perform an immediate hard flush_ring"
        );

        harness.finish();
    }
    #[test]
    fn a_voice_skips_regions_once_and_releases_its_file_reader() {
        let path =
            std::env::temp_dir().join(format!("bitchord-voice-regions-{}.wav", std::process::id()));
        test_wav(&path, 3.0, 440.0);
        let source = path.to_string_lossy().to_string();
        crate::playback_regions::put(
            source.clone(),
            crate::PlaybackRegions {
                excluded: vec![crate::PlaybackInterval {
                    start_seconds: 1.0,
                    end_seconds: 2.0,
                }],
                ..Default::default()
            },
        );
        let request = TrackSource {
            source: source.clone(),
            title: "Fixture".into(),
            artist: "".into(),
            start_seconds: 0.0,
            plan: TransitionPlan::default(),
            headers: std::collections::HashMap::new(),
            claimed_kbps: 0,
            loudness_db: None,
            duration_seconds: 3.0,
        };
        let mut voice = Voice::open(&request, false, 0.0, 44100, 1.0, false, false).unwrap();
        assert!(crate::playback_regions::source_has_playback_readers(
            source.clone()
        ));
        let mut frames = 0;
        while !voice.finished || voice.pending_dev_cursor < voice.pending_dev.len() {
            frames += voice.pull(1024, 44100).len() / 2;
        }
        assert!((frames as f64 / 44100.0 - 2.0).abs() < 0.03);
        assert!(voice.skipped_regions[0]);
        voice.decoder.seek_seconds(1.2).unwrap();
        voice.reset_processing();
        voice.skipped_regions.fill(true);
        let replay = voice.pull(1024, 44100);
        assert!(!replay.is_empty());
        assert!(voice.decoder.position_seconds() < 2.0);
        drop(voice);
        assert!(!crate::playback_regions::source_has_playback_readers(
            source
        ));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn aac_region_cuts_advance_without_reseeking_preroll() {
        let (done, result) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let fixture = include_bytes!("../../tests/fixtures/seek-tone.m4a");
            for late in [false, true] {
                let path = std::env::temp_dir().join(format!(
                    "bitchord-aac-regions-{}-{late}.m4a",
                    std::process::id()
                ));
                std::fs::write(&path, fixture).unwrap();
                let source = path.to_string_lossy().to_string();
                let regions = crate::PlaybackRegions {
                    // Deliberately between source samples.
                    audible_start_seconds: 0.190001,
                    excluded: vec![crate::PlaybackInterval {
                        start_seconds: 1.0,
                        end_seconds: 1.5,
                    }],
                    ..Default::default()
                };
                if !late {
                    crate::playback_regions::put(source.clone(), regions.clone());
                }
                let request = TrackSource {
                    source: source.clone(),
                    title: "AAC fixture".into(),
                    artist: "".into(),
                    start_seconds: 0.0,
                    plan: TransitionPlan::default(),
                    headers: Default::default(),
                    claimed_kbps: 0,
                    loudness_db: None,
                    duration_seconds: 3.0,
                };
                let mut voice = Voice::open(&request, false, 0.0, 48000, 1.0, false, true).unwrap();
                if late {
                    voice.regions = regions;
                    voice.skipped_regions = vec![false];
                }
                let mut frames = 0;
                let mut peak = 0.0_f32;
                for _ in 0..200 {
                    let pcm = voice.pull(1024, 48000);
                    frames += pcm.len() / 2;
                    peak = pcm.iter().fold(peak, |a, b| a.max(b.abs()));
                    if voice.finished && voice.pending_dev_cursor == voice.pending_dev.len() {
                        break;
                    }
                }
                assert!(voice.finished, "AAC voice must complete");
                assert!(peak > 0.1, "AAC voice must produce audible PCM");
                assert!(
                    (frames as f64 / 48000.0 - 2.309999).abs() < 0.03,
                    "trim and skip must retain exact source timing: {frames} frames"
                );
                assert!(voice.skipped_regions[0]);
                drop(voice);
                std::fs::remove_file(path).unwrap();
            }
            done.send(()).unwrap();
        });
        result
            .recv_timeout(Duration::from_secs(3))
            .expect("AAC boundary seek stalled the mixer");
    }

    #[test]
    fn after_song_hold_rejects_late_successors() {
        let (mut harness, low, high) = SwapHarness::new("sleep-hold");
        let (reply, loaded) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::Load {
                request: SwapHarness::source(&low, "First", 96),
                reply,
            })
            .unwrap();
        loaded
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        harness
            .tx
            .send(Command::QueueNext {
                request: SwapHarness::source(&high, "Second", 320),
            })
            .unwrap();
        harness.tx.send(Command::HoldTrackEnd(true)).unwrap();
        harness
            .tx
            .send(Command::QueueNext {
                request: SwapHarness::source(&high, "Late", 320),
            })
            .unwrap();
        let (reply, peeked) = crossbeam_channel::bounded(1);
        harness.tx.send(Command::PendingTrack { reply }).unwrap();
        assert!(peeked
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_none());
        drain_frames(&mut harness.consumer, &harness.buffered, 44100 * 8);
        std::thread::sleep(Duration::from_millis(20));
        assert!(harness.events.handoffs.lock().unwrap().is_empty());
        assert_eq!(harness.events.ended.lock().unwrap().len(), 1);
        harness.finish();
    }
}
