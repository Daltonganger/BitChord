//! Compact port of upstream `TransitionPolicy` + `planTransition` for a pair of files.

mod analysis;
mod bridge;
mod cache;
mod choose;
mod cue;
mod edges;
mod key;
mod mix_out;
mod technique;
mod vocal_overlap;
mod wsola;

#[cfg(test)]
mod tests;

pub(crate) use cache::clear_analysis_cache;
pub use cache::{last_analysis_sources, next_energy_dip, seed_analysis_overlay, AnalysisOverlay};
pub use cue::{last_transition_edges, TransitionEdges};

use super::audio_analysis;
use super::beat;
use super::resample;
use super::vocal;
use crate::mixer::{TransitionPlan, TransitionStyle};

use analysis::Analysis;
use bridge::apply_bridge;
use cache::{analyze_cached, last_sources};
use choose::plan_from_with_phrase_bars;
use cue::record_transition_edges;
use edges::respect_music_edges;
use mix_out::resolve_mix_out_anchor;
use vocal_overlap::vocal_activity_between;
use wsola::phrase_switch;

const MIN_BEATMATCH_CONFIDENCE: f64 = 0.55;
const MIN_DJ_CONFIDENCE: f64 = 0.2;
const MIN_BPM: f64 = 40.0;
const MAX_BPM: f64 = 220.0;
/// How far a Beatmatched pair may drift from unity and still stretch.
///
/// Apple AutoMix's documented failure mode is comic warps (reviewers report
/// ~0.75×–1.5×). Beatmatched stretch stays inside this band; DjAssisted never
/// stretches at all — it rides filters instead.
const MAX_STRETCH_DEVIATION: f64 = 0.04;
/// Alias used at call sites that stretch an incoming track onto the outgoing.
const MAX_BEATMATCH_STRETCH: f64 = MAX_STRETCH_DEVIATION;
const VOCAL_ACTIVE_THRESHOLD: f64 = 0.6;
const FILTER_SWEEP: f64 = 1.0;
const AUTO_TRANSITION_MAX_BEATS: f64 = 16.0;
const AUTO_TRANSITION_MAX_SECONDS: f64 = 12.0;
/// F's bridged transition may run longer than an ordinary one: the owner wants
/// long, natural overlaps, and the 12 s ceiling was upstream's choice rather
/// than a listening result. The base fade still respects 12 s; only the bridge
/// may take the total to this.
const MAX_BRIDGED_SECONDS: f64 = 16.0;
const AUTO_MIN_SECONDS: f64 = 4.0;
const AUTO_FAST_TRACK_MIN_SECONDS: f64 = 6.0;
/// Upstream `AUTO_FALLBACK_SECONDS` — the fade used when no analysis landed.
const AUTO_FALLBACK_SECONDS: f64 = 8.0;
/// Upstream `MIN_SMART_DURATION_SECONDS` — a track shorter than this gets a
/// plain crossfade rather than a smart transition.
const MIN_SMART_DURATION_SECONDS: f64 = 45.0;
/// Upstream `TrackAnalyzer.MIN_DECODED_FRACTION` — how much of the container's
/// stated duration must actually decode before the whole-track pass is trusted.
///
/// Not 1.0: a decoder legitimately comes up a frame or two short of the
/// container's rounding, and refusing over that would refuse everything. Not
/// lower either: the failure this catches is a still-downloading file, and the
/// cost of trusting one is a mix-out anchor placed where the bytes ran out.
const MIN_DECODED_FRACTION: f64 = 0.95;
/// Pickup inside this margin of the end is the outro, not a mix-in
/// (upstream `incomingCuePoint`: `pickup < duration - 10`).
const MIX_IN_END_MARGIN_SECONDS: f64 = 10.0;
/// How deep a mix-in may reach, in seconds, in beats, and as a fraction of the
/// incoming track.
///
/// A DJ enters the next record at the top of its own intro. Each bound exists
/// for a different track:
///
/// * `MAX_CUE_SECONDS` is the one that normally bites — four bars is about 7 s
///   at 140 BPM, so anything past that is a skip, not a mix-in.
/// * `MAX_CUE_BEATS` stops a slow track turning 16 beats into half a minute,
///   which is what the seconds bound would otherwise allow at 40 BPM.
/// * `MAX_CUE_FRACTION` is a floor under both, for short material: 6 % of a
///   one-minute clip is 3.6 s, and an 8 s skip there is 13 % of the record. It
///   never binds above about 133 s, where the seconds bound takes over.
const MAX_CUE_SECONDS: f64 = 8.0;
const MAX_CUE_BEATS: f64 = 16.0;
const MAX_CUE_FRACTION: f64 = 0.06;
/// Upstream bass-swap tuning constants (TransitionPlanner.kt:351-364).
const HANDOFF_FRACTION: f64 = 0.5;
const DEFAULT_BASS_SWAP_FRACTION: f64 = 0.7;
const MAX_BASS_SWAP_FRACTION: f64 = 0.85;
const MIN_BASS_STRUCTURE_SCORE: f64 = 0.25;
const BASS_SWAP_MAX_SECONDS: f64 = 6.0;
/// Album tracks played in order still join, they just do not get a DJ blend.
///
/// Upstream uses 0.12 s here (TransitionPlanner.kt:901), which is a splice.
/// A few seconds of equal-power is the shortest ramp that still reads as one
/// record giving way to the next.
const GAPLESS_FADE_SECONDS: f64 = AUTO_MIN_SECONDS;
/// Upstream `MAX_DISCARDED_MUSIC_SECONDS` — a mix-out anchor may not skip more
/// than this much audible music.
const MAX_DISCARDED_MUSIC_SECONDS: f64 = 12.0;
/// Upstream `AUDIBLE_ENERGY_FRACTION` — the energy threshold that counts a
/// point as audible when measuring skipped music.
const AUDIBLE_ENERGY_FRACTION: f64 = 0.1;
/// Upstream WSOLA phrase-switch constants (TransitionPlanner.kt:329-374),
/// plus the long blend a confident, unclashing pair is allowed.
const MIN_FADE_BEATS: usize = 4;
/// Upstream ceiling. A confident beat match may go further; see
/// [`LONG_BLEND_BEATS`].
const UPSTREAM_MAX_FADE_BEATS: usize = 16;
const UPSTREAM_MAX_OVERLAP_SECONDS: f64 = 16.0;
/// Apple-style long blend: up to 64 beats / 32 s when both grids are trusted
/// and the vocal-clash loop does not shrink it. 64 beats at 120 BPM is 32 s.
const LONG_BLEND_BEATS: usize = 64;
const LONG_BLEND_SECONDS: f64 = 32.0;
/// After the blend, walk the matched tempo back to the record's own over
/// this many beats of the tempo it was matched to.
const POST_BLEND_GLIDE_BEATS: f64 = 32.0;
/// Incoming level while it plays underneath, before the swap.
const BED_GAIN_DB: f64 = -14.0;
const ARRANGEMENT_OVERLAP_BEATS: f64 = 8.0;
const MIN_CLEARANCE_SECONDS: f64 = 5.0;
const VOCAL_CLASH_TOLERANCE: f64 = 0.05;

pub fn plan_pair(
    outgoing_path: &str,
    incoming_path: &str,
    outgoing_text: &str,
    incoming_text: &str,
    album_sequential: bool,
    crossfade_seconds: f64,
    outgoing_duration_hint: f64,
    incoming_duration_hint: f64,
    skip_vocals: bool,
    tier_label: &str,
    allow_effects: bool,
    decode: impl Fn(&str, f64, f64, bool) -> Option<(Vec<f32>, u32, f64)>,
    duration_of: impl Fn(&str) -> f64,
) -> TransitionPlan {
    plan_pair_with_phrase_bars(
        outgoing_path,
        incoming_path,
        outgoing_text,
        incoming_text,
        album_sequential,
        crossfade_seconds,
        outgoing_duration_hint,
        incoming_duration_hint,
        skip_vocals,
        tier_label,
        allow_effects,
        0,
        decode,
        duration_of,
    )
}

pub fn plan_pair_with_phrase_bars(
    outgoing_path: &str,
    incoming_path: &str,
    outgoing_text: &str,
    incoming_text: &str,
    album_sequential: bool,
    crossfade_seconds: f64,
    outgoing_duration_hint: f64,
    incoming_duration_hint: f64,
    skip_vocals: bool,
    tier_label: &str,
    allow_effects: bool,
    phrase_bars: u32,
    decode: impl Fn(&str, f64, f64, bool) -> Option<(Vec<f32>, u32, f64)>,
    duration_of: impl Fn(&str) -> f64,
) -> TransitionPlan {
    let fade = if crossfade_seconds > 0.0 {
        crossfade_seconds.clamp(1.0, 12.0)
    } else {
        AUTO_FALLBACK_SECONDS
    };
    // Upstream `BLOCKED_TEXT`: spoken or already-performed material is never
    // smart-mixed — fall back to a plain equal-power crossfade.
    //
    // Even this one carries the outgoing track's length, because a plain
    // crossfade is still *scheduled* against the end of the outgoing track and
    // the mixer has to know where that is.
    let hinted_duration = |hint: f64, path: &str| {
        if hint.is_finite() && hint > 0.0 {
            hint
        } else {
            duration_of(path)
        }
    };
    let outgoing_duration = hinted_duration(outgoing_duration_hint, outgoing_path);
    let out = analyze_cached(outgoing_path, skip_vocals, &decode, outgoing_duration);
    // Spoken or already-performed material is never smart-mixed. The fallback
    // is still anchored on the outgoing content end, so it does not wait out
    // the trailing silence and then cut.
    if blocked_text(outgoing_text) || blocked_text(incoming_text) {
        return plain_fallback(fade, &out);
    }

    // Upstream gapless path (TransitionPlanner.kt:893-905): an album played
    // through in order skips the DJ blend, unless the outgoing track has an
    // interior energy cliff worth mixing out of. The join itself is still a
    // ramp — upstream's 0.12 s handoff is a cut.
    if album_sequential {
        let (anchor, _) = resolve_mix_out_anchor(&out);
        let end = if out.content_end > 0.0 {
            out.content_end
        } else {
            out.duration
        };
        let has_interior_mix_out = anchor > 0.0 && anchor < end - 1.0;
        if !has_interior_mix_out {
            return TransitionPlan {
                style: TransitionStyle::Gapless,
                bass_swap: false,
                bass_swap_fraction: DEFAULT_BASS_SWAP_FRACTION,
                filter_sweep: 0.0,
                vocal_overlap: 0.0,
                fade_seconds: GAPLESS_FADE_SECONDS,
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
                outgoing_duration_seconds: outgoing_duration,
                beat_seconds: 0.0,
                reason: String::new(),
            };
        }
    }

    let incoming_duration = hinted_duration(incoming_duration_hint, incoming_path);
    let inc = analyze_cached(incoming_path, skip_vocals, &decode, incoming_duration);

    // The most ambitious move: a beat-matched, harmonically-compatible pair gets
    // a WSOLA phrase-switch before the adaptive overlap (upstream `phraseSwitch`).
    let mut plan =
        phrase_switch(&out, &inc).unwrap_or_else(|| {
            plan_from_with_phrase_bars(&out, &inc, fade, allow_effects, phrase_bars)
        });
    respect_music_edges(&out, &inc, &mut plan);
    apply_bridge(&out, &mut plan);
    record_transition_edges(&out, &inc, &plan);
    log_plan(&out, &inc, &plan, tier_label);
    plan
}

/// One line per planned transition, with the evidence that produced it.
///
/// The bug this exists to make diagnosable was invisible from the outside: a
/// track that lost forty-five seconds to a mix-in sounded exactly like a track
/// that had always been cued that way, and the mixer's `voice opened: …` line
/// reports the symptom (a large `start`) without the cause. These are the
/// inputs to [`incoming_cue`] and to [`assess`], so a wrong cue can be traced to
/// the read that produced it.
fn log_plan(outgoing: &Analysis, incoming: &Analysis, plan: &TransitionPlan, tier: &str) {
    let grid = |a: &Analysis| {
        format!(
            "bpm={:.1} conf={:.2} beat={:.3} downbeats={} audible={:.1} content_end={:.1} dur={:.1} vocal_p={:.2} \
             key={}@{:.2}",
            a.bpm,
            a.beat_confidence,
            a.beat_interval,
            a.downbeats.len(),
            a.audible_start_time,
            a.content_end,
            a.duration,
            a.vocal_probability,
            if a.key.is_empty() { "?" } else { a.key.as_str() },
            a.key_confidence,
        )
    };
    // What the plan could see about the voice, beside the whole-track scalar
    // the effect rule used to read. `vocal_p` is one number for the record; the
    // tail and the head are the two windows an actual handover covers.
    let window_vocal = |a: &Analysis, from: f64, to: f64| {
        vocal_activity_between(a, from, to)
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "-".into())
    };
    let out_beat = if outgoing.beat_interval > 0.0 {
        outgoing.beat_interval
    } else {
        0.5
    };
    let in_beat = if incoming.beat_interval > 0.0 {
        incoming.beat_interval
    } else {
        0.5
    };
    let out_end = if outgoing.content_end > 0.0 {
        outgoing.content_end
    } else {
        outgoing.duration
    };
    let in_start = incoming.audible_start_time.max(0.0);
    log::info!(
        "automix plan [{}] style={:?} cue={:.2}s fade={:.2}s end={:.1}s rate={:.4} \
         bed={:.2}@{:.0}dB dip={:.2} glide={:.1}s vocal_overlap={:.2} bass_swap={}@{:.2}",
        tier,
        plan.style,
        plan.cue_seconds,
        plan.fade_seconds,
        plan.transition_end_seconds,
        plan.playback_rate,
        plan.bed_fraction,
        plan.bed_gain_db,
        plan.dip_depth,
        plan.post_glide_seconds,
        plan.vocal_overlap,
        plan.bass_swap,
        plan.bass_swap_fraction,
    );
    log::info!(
        "automix out [{}]: {} vocal_tail={}",
        if outgoing.analysis_source.is_empty() {
            "dsp"
        } else {
            &outgoing.analysis_source
        },
        grid(outgoing),
        window_vocal(outgoing, out_end - 8.0 * out_beat, out_end)
    );
    log::info!(
        "automix in [{}]: {} vocal_head={}",
        if incoming.analysis_source.is_empty() {
            "dsp"
        } else {
            &incoming.analysis_source
        },
        grid(incoming),
        window_vocal(incoming, in_start, in_start + 8.0 * in_beat)
    );
    last_sources::set(&outgoing.analysis_source, &incoming.analysis_source);
}

/// The plain equal-power fallback shared by the speech guard and the tier.
///
/// Anchored on the outgoing mix-out (or its content end), never on the file's
/// last byte — that is the silence after the song, and fading there is why a
/// transition waited for one record to finish before the next began.
fn plain_fallback(fade: f64, outgoing: &Analysis) -> TransitionPlan {
    let mut end = resolve_mix_out_anchor(outgoing).0;
    if end <= 0.0 {
        end = if outgoing.content_end > 0.0 {
            outgoing.content_end
        } else {
            outgoing.duration
        };
    }
    TransitionPlan {
        style: TransitionStyle::EqualPower,
        bass_swap: false,
        bass_swap_fraction: DEFAULT_BASS_SWAP_FRACTION,
        filter_sweep: 0.0,
        vocal_overlap: 0.0,
        fade_seconds: fade,
        outgoing_hold_seconds: 0.0,
        bridge_start_seconds: 0.0,
        bridge_seconds: 0.0,
        bridge_repeats: 0,
        effect_seconds: 0.0,
        effect_start_seconds: 0.0,
        transition_end_seconds: end,
        cue_seconds: 0.0,
        playback_rate: 1.0,
        bed_fraction: 0.0,
        bed_gain_db: 0.0,
        dip_depth: 0.0,
        dip_width: 0.0,
        post_glide_seconds: 0.0,
        outgoing_duration_seconds: outgoing.duration,
        beat_seconds: 0.0,
        reason: String::new(),
    }
}

/// Upstream `BLOCKED_TEXT` regex — `\b(podcast|episode|audiobook|live|concert|performance)\b`
/// case-insensitive — checked with word boundaries, no regex dependency.
fn blocked_text(text: &str) -> bool {
    const BLOCKED: [&str; 6] = [
        "podcast",
        "episode",
        "audiobook",
        "live",
        "concert",
        "performance",
    ];
    let lower = text.to_lowercase();
    let bytes = lower.as_bytes();
    for word in BLOCKED {
        let mut start = 0;
        while let Some(relative) = lower[start..].find(word) {
            let absolute = start + relative;
            let before_ok = absolute == 0 || !bytes[absolute - 1].is_ascii_alphanumeric();
            let end = absolute + word.len();
            let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return true;
            }
            start = absolute + word.len();
        }
    }
    false
}
