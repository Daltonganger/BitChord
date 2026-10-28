//! The mixer's public vocabulary: tracks, sources, transition plans, the
//! commands the engine sends, and the events it reports back.

use super::fx::{self};
use crate::sound::{ClarityTuning, LoudnessMode, SoundMode};
use crossbeam_channel::Sender;
use rtrb::Producer;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

/// Callback surface — the UniFFI-generated Swift side implements this.
pub trait EngineEvents: Send + Sync {
    fn state_changed(&self, state: crate::PlaybackState);
    fn track_ended(&self, reason: crate::TrackEndReason, source: String);
    fn error(&self, message: String);
    fn source_error(&self, _source: String, message: String) {
        self.error(message);
    }
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
    /// A short, tempo-synced flanger send that colors the outgoing phrase before handoff.
    Texture,
    /// A tape-style varispeed slowdown that brakes the outgoing phrase into the handoff.
    Brake,
}

impl TransitionStyle {
    /// Stable telemetry label shared by live transition records and the
    /// corpus scorecard. Keep this explicit so diagnostic output is not tied
    /// to Rust's `Debug` representation.
    pub const fn telemetry_name(self) -> &'static str {
        match self {
            Self::EqualPower => "equal_power",
            Self::DjFilter => "dj_filter",
            Self::DjBlend => "dj_blend",
            Self::Gapless => "gapless",
            Self::EchoOut => "echo_out",
            Self::LoopRoll => "loop_roll",
            Self::ReverbWash => "reverb_wash",
            Self::DropCut => "drop_cut",
            Self::Texture => "texture",
            Self::Brake => "brake",
        }
    }

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
    CancelAutomixProcessing,
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
