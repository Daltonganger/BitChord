//! Per-deck measurements: silence detection, loudness matching, known
//! durations, and the bounds on where a mix may start.

use super::types::TransitionPlan;
use super::voice::Voice;
use super::{MAX_CUE_FRACTION, MAX_CUE_SECONDS, MIN_INCOMING_CLEARANCE_S};
use crate::time_stretch::TimeStretch;

/// Media3 `DEFAULT_SILENCE_THRESHOLD_LEVEL` (1024 of int16 full scale).
pub(super) const SILENCE_THRESHOLD: f32 = 1024.0 / 32768.0;
/// Upstream `PlaybackService.MIN_SILENCE_US` — shortest gap we'll trim.
pub(super) const MIN_SILENCE_SECS: f64 = 1.0;
/// Upstream Media3 `DEFAULT_MAX_SILENCE_TO_KEEP_DURATION_US` (2 s) — cap on the
/// sliver of a trimmed gap that is kept.
pub(super) const MAX_SILENCE_KEEP_SECS: f64 = 2.0;
/// Upstream Media3 `DEFAULT_MIN_VOLUME_TO_KEEP_PERCENTAGE` (10) — the kept
/// sliver is faded to −20 dB rather than true digital silence.
pub(super) const MIN_VOLUME_TO_KEEP: f32 = 0.1;

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
    /// The deck's measured true peak in dBTP, when it has been measured.
    pub true_peak_dbtp: Option<f64>,
}

impl DeckLoudness {
    pub(super) fn applied(self) -> f64 {
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
    pub(super) fn shared_level(incoming: Self, outgoing: Self) -> Option<(f64, f64)> {
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
    ///
    /// A boost is held much lower, to [MAX_TRANSITION_BOOST_DB] and to the
    /// incoming deck's measured headroom when there is one. Streamed masters
    /// already peak at or above full scale once decoded, so turning one up is
    /// turning it into the limiter — a +6 dB match put 81% of one track's
    /// 10 ms blocks over full scale. Cutting a louder track costs nothing.
    pub fn match_gain_db(incoming: DeckLoudness, outgoing: DeckLoudness) -> Option<f64> {
        let (a, b) = DeckLoudness::shared_level(incoming, outgoing)?;
        let delta = b - a;
        if !delta.is_finite() {
            return None;
        }
        let headroom = incoming
            .true_peak_dbtp
            .filter(|p| p.is_finite())
            .map(|peak| (TRANSITION_PEAK_CEILING_DBTP - peak - incoming.applied()).max(0.0))
            .unwrap_or(MAX_TRANSITION_BOOST_DB);
        Some(delta.clamp(
            -MAX_TRANSITION_MATCH_DB,
            MAX_TRANSITION_BOOST_DB.min(headroom),
        ))
    }
}

/// The loudest cut a transition may apply.
pub const MAX_TRANSITION_MATCH_DB: f64 = 6.0;
/// The loudest boost a transition may apply, before the deck's own headroom.
pub const MAX_TRANSITION_BOOST_DB: f64 = 2.0;
/// Where a boosted deck's true peak may reach.
const TRANSITION_PEAK_CEILING_DBTP: f64 = -1.0;

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
pub(super) fn known_duration(container: Option<f64>, declared: f64) -> f64 {
    container
        .filter(|d| d.is_finite() && *d > 0.0)
        .or_else(|| Some(declared).filter(|d| d.is_finite() && *d > 0.0))
        .unwrap_or(0.0)
}

/// `.grow` without `.complete` is a stream that has not finished arriving.
pub(super) fn source_still_growing(path: &str) -> bool {
    if path.is_empty() || path.contains("://") {
        return false;
    }
    let grow = format!("{path}.grow");
    let complete = format!("{path}.complete");
    std::path::Path::new(&grow).is_file() && !std::path::Path::new(&complete).exists()
}

pub(super) fn user_stretcher(device_rate: u32, speed: f32) -> Option<TimeStretch> {
    if (speed - 1.0).abs() < 1e-6 {
        None
    } else {
        let mut stretch = TimeStretch::with_sample_rate(speed as f64, device_rate);
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
pub(super) fn glided_plan_rate(base: f64, p: f64) -> f64 {
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

pub(super) fn is_silent(interleaved: &[f32]) -> bool {
    // Empty is "no audio yet" (resampler still priming), not a gap to trim.
    if interleaved.is_empty() {
        return false;
    }
    interleaved.iter().all(|s| s.abs() < SILENCE_THRESHOLD)
}

pub(super) fn silence_exceeds_floor(silent_dev_frames: u64, device_rate: u32) -> bool {
    silent_dev_frames as f64 / device_rate.max(1) as f64 > MIN_SILENCE_SECS
}

/// A voice's fade gain with the loudness correction folded in.
///
/// The fade gain (`voice.gain`) is the mix; the loudness gain is the level.
/// Multiplying here rather than in `Voice::pull` keeps the measurement (peak
/// classification for skip-silence) on the uncorrected signal — a quiet track
/// turned up is still a quiet track as far as gap detection is concerned.
pub(super) fn normalization_gain(voice: &Voice, enabled: bool) -> (f32, Option<f32>) {
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
pub(super) fn clamp_start_seconds(requested: f64, duration: f64) -> f64 {
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
pub(super) fn bound_mix_in_depth(requested: f64, plan: &TransitionPlan, duration: f64) -> f64 {
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
