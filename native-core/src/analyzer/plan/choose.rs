//! Choosing a pair's plan: the tier, the mix shape, and whether an effect is
//! earned.

use super::analysis::{Analysis, Tier};
use super::cue::{
    aligned_transition_start, cue_for, incoming_entry, incoming_handoff, realized_window,
};
use super::edges::{
    align_tempo_octave, assess, normalized_tempo_ratio, section_fade_cap, transparent_playback_rate,
};
use super::key::{key_distance, keys_clash, trusted_key};
use super::mix_out::{bass_swap_fraction_for, resolve_mix_out_anchor};
use super::technique::{
    clean_crossfade, handover_is_busy, last_strong_downbeat, long_blend_allowed, technique_plan,
};
use super::vocal_overlap::{handover_vocal_clash, planned_vocal_overlap};
use super::{
    plain_fallback, ARRANGEMENT_OVERLAP_BEATS, AUTO_FAST_TRACK_MIN_SECONDS, AUTO_MIN_SECONDS,
    AUTO_TRANSITION_MAX_BEATS, AUTO_TRANSITION_MAX_SECONDS, BED_GAIN_DB,
    DEFAULT_BASS_SWAP_FRACTION, FILTER_SWEEP, LONG_BLEND_BEATS, LONG_BLEND_SECONDS,
    MAX_BEATMATCH_STRETCH, MAX_DISCARDED_MUSIC_SECONDS, MIN_CLEARANCE_SECONDS,
    MIN_SMART_DURATION_SECONDS, POST_BLEND_GLIDE_BEATS,
};
use crate::mixer::{TransitionPlan, TransitionStyle, MAX_DUCK_DEPTH};

pub(super) fn plan_from(
    outgoing: &Analysis,
    incoming: &Analysis,
    fade: f64,
    allow_effects: bool,
) -> TransitionPlan {
    let entry = incoming_entry(incoming);
    // Upstream `MIN_SMART_DURATION_SECONDS`: a track too short to spend a smart
    // transition on gets a plain crossfade (TransitionPlanner.kt:880-882).
    let too_short = (outgoing.duration > 0.0 && outgoing.duration < MIN_SMART_DURATION_SECONDS)
        || (incoming.duration > 0.0 && incoming.duration < MIN_SMART_DURATION_SECONDS);
    let tier = assess(outgoing, incoming);
    // The analysed blend is the default; an effect only seasons a pair the
    // blend cannot take. A track too short to spend a smart transition on gets
    // a plain fade, as upstream does.
    if too_short {
        let mut plan = plain_fallback(fade, outgoing);
        plan.cue_seconds = entry;
        plan.reason = "fade: a track is too short for a technique".into();
        return plan;
    }
    if matches!(tier, Tier::Plain) {
        if allow_effects {
            if let Some(plan) = technique_plan(outgoing, incoming, tier, entry, None, None) {
                return plan;
            }
        }
        let mut plan = plain_fallback(fade, outgoing);
        plan.cue_seconds = entry;
        plan.reason = "fade: no usable tempo on either side".into();
        return plan;
    }

    let out_bpm = outgoing.bpm;
    let in_bpm = incoming.bpm;
    let ratio = normalized_tempo_ratio(out_bpm, in_bpm);
    let same_beat = (1.0 - ratio).abs() <= 0.05
        && (outgoing.beat_confidence >= 0.2 || incoming.beat_confidence >= 0.2);
    // Only Beatmatched may stretch, and only inside MAX_BEATMATCH_STRETCH.
    // DjAssisted used to warp up to ±10% on the 0.9–1.1 band — that is the
    // Apple failure mode (tempo morph that draws attention to itself). A weak
    // grid keeps unity rate and lets the filter ride mask the gap instead.
    let playback_rate = if matches!(tier, Tier::Beatmatched) {
        transparent_playback_rate(out_bpm, in_bpm, MAX_BEATMATCH_STRETCH).unwrap_or(1.0)
    } else {
        1.0
    };
    // The whole-track vocal probability cannot answer "does this handover put
    // two singers together": it sits at 0.82–0.86 on all thirteen corpus
    // tracks, sung or not. The windows the handover actually covers do
    // discriminate — 0.28 through an instrumental outro, 0.91 through a sung
    // one — so the clash is read there.
    let (anchor, _) = resolve_mix_out_anchor(outgoing);
    let vocal_conflict = handover_vocal_clash(outgoing, incoming, anchor, entry);
    // A confident beat match with both voices out of the way earns the long
    // blend. Two singers, or a weak grid, stays inside upstream's 8–16 beats.
    let long = long_blend_allowed(outgoing, incoming) && !vocal_conflict;
    let key_distance = key_distance(&trusted_key(outgoing), &trusted_key(incoming));
    let overlap_beats = if long {
        LONG_BLEND_BEATS as f64
    } else if !vocal_conflict && ((1.0 - ratio).abs() > 0.07 || key_distance.is_some_and(|d| d > 4))
    {
        16.0
    } else {
        8.0
    };
    let beat_seconds = if out_bpm > 0.0 { 60.0 / out_bpm } else { 0.5 };
    let min_overlap = if out_bpm >= 140.0 {
        AUTO_FAST_TRACK_MIN_SECONDS
    } else {
        AUTO_MIN_SECONDS
    };
    let max_seconds = if long {
        LONG_BLEND_SECONDS
    } else {
        AUTO_TRANSITION_MAX_SECONDS
    };
    let max_beats_cap = if long {
        LONG_BLEND_BEATS as f64
    } else {
        AUTO_TRANSITION_MAX_BEATS
    };
    let overlap = (overlap_beats * beat_seconds).clamp(min_overlap, max_seconds);

    // Where the outgoing record should be gone: its mix-out anchor, pulled
    // forward by an arrangement overlap when that anchor is just the content
    // end (upstream `outgoingArrangementOverlap`).
    let content_end = if outgoing.content_end > 0.0 {
        outgoing.content_end
    } else {
        outgoing.duration
    };
    let arrangement = if same_beat && (anchor - content_end).abs() < 0.05 && out_bpm > 0.0 {
        (ARRANGEMENT_OVERLAP_BEATS * 60.0 / out_bpm).min(MAX_DISCARDED_MUSIC_SECONDS)
    } else {
        0.0
    };
    let mix_end = (anchor - arrangement).max(0.0);
    let mut max_overlap = (max_beats_cap * beat_seconds)
        .min(max_seconds)
        .min((mix_end * 0.4).max(min_overlap))
        .min(if incoming.duration > 0.0 {
            (incoming.duration * 0.4).max(min_overlap)
        } else {
            max_seconds
        })
        .max(min_overlap);
    // Mixxx Auto DJ: fade length ≈ min(usable outro, usable intro) when both
    // sections are known. Caps the overlap before handoff math so the cue and
    // the fade agree.
    if let Some(cap) = section_fade_cap(outgoing, incoming) {
        max_overlap = max_overlap.min(cap.max(min_overlap));
    }

    let handoff_beats = if same_beat { 8.0 } else { 4.0 };
    let handoff_seconds = if out_bpm > 0.0 {
        (handoff_beats * 60.0 / out_bpm).clamp(2.0, if same_beat { 6.0 } else { 5.0 })
    } else {
        4.0
    };

    // The drop is the handoff, not the cue. Playback starts `overlap` earlier,
    // and never more than eight seconds past the entry.
    let drop = incoming_handoff(incoming, max_overlap);
    let aligned_in = align_tempo_octave(out_bpm, in_bpm);
    let requested_handoff = if same_beat && aligned_in > 0.0 {
        drop + ARRANGEMENT_OVERLAP_BEATS * 60.0 / aligned_in
    } else {
        drop
    };
    let max_in_handoff = incoming.duration - MIN_CLEARANCE_SECONDS;
    let handoff = if max_in_handoff >= drop {
        requested_handoff.min(max_in_handoff)
    } else {
        drop
    };

    // A Beatmatched pair with a transparent stretch earns DjBlend. Anything
    // else — DjAssisted, far tempo, refused stretch — rides filters so the
    // ear never hears a chipmunk or a slow-mo vocal.
    let beatmatched_blend = matches!(tier, Tier::Beatmatched)
        && same_beat
        && (playback_rate - 1.0).abs() <= MAX_BEATMATCH_STRETCH + 1e-6;

    let (transition_start, fade_seconds) = if beatmatched_blend && beat_seconds > 0.0 {
        let intro_span = handoff / playback_rate.max(0.8);
        let total = intro_span.clamp(12.0_f64.min(max_overlap), max_overlap);
        let target = (mix_end - total).max(0.0);
        let earliest = (mix_end - max_overlap).max(0.0);
        let start = aligned_transition_start(
            outgoing,
            target,
            (mix_end - 0.05).max(earliest),
            true,
            earliest,
        );
        realized_window(mix_end, start, total, min_overlap)
    } else {
        let desired = overlap.max(handoff_seconds * 0.42);
        let actual = desired.clamp(handoff_seconds.min(max_overlap), max_overlap);
        let target = (mix_end - actual).max(0.0);
        let earliest = (mix_end - max_overlap).max(0.0);
        let start = aligned_transition_start(
            outgoing,
            target,
            (mix_end - 0.05).max(earliest),
            desired > overlap + 0.5,
            earliest,
        );
        realized_window(mix_end, start, actual, min_overlap)
    };
    let cue = cue_for(entry, handoff, fade_seconds, playback_rate);

    let incoming_beat_seconds = if in_bpm > 0.0 { 60.0 / in_bpm } else { 0.5 };
    let bass_swap_fraction = if beatmatched_blend {
        bass_swap_fraction_for(
            outgoing,
            incoming,
            transition_start,
            cue,
            beat_seconds,
            incoming_beat_seconds,
            fade_seconds,
            overlap_beats as usize,
        )
    } else {
        DEFAULT_BASS_SWAP_FRACTION
    };
    let vocal_overlap = planned_vocal_overlap(
        outgoing,
        incoming,
        transition_start,
        transition_start + fade_seconds,
        cue,
        playback_rate,
    );
    let style = if beatmatched_blend {
        TransitionStyle::DjBlend
    } else {
        TransitionStyle::DjFilter
    };
    let shape = mix_shape(
        style,
        beatmatched_blend,
        vocal_overlap,
        vocal_conflict,
        fade_seconds,
        beat_seconds,
        bass_swap_fraction,
        playback_rate,
        out_bpm,
    );

    // A usable blend is the answer for every pair except a real clash: a key
    // the stretch cannot reconcile (so the two never overlap dry), a tempo gap
    // past the filter blend's 7 % band, or a vocal-on-vocal overlap the planner
    // could not shorten below the clash threshold. Only then may an effect
    // replace the blend; the v0.1.3 blend remains the default.
    let tempo_gap = (1.0 - ratio).abs();
    // A stretch does not change key; a wide key distance is a real clash on
    // its own. The corpus firing rates will decide whether that should earn an
    // effect or a shorter filtered blend.
    let key_clash = keys_clash(
        &outgoing.key,
        outgoing.key_confidence,
        &incoming.key,
        incoming.key_confidence,
    );
    // An unmatched tempo or a key clash earns a technique, and the session's
    // pacing (`allow_effects`, up to three transitions in four) decides whether it
    // gets one. The owner's call (2026-10-24): the effects are part of the
    // set, so a spent budget, not a falling tail, is what sends an unmatched
    // pair to the clean crossfade below (principle H, now the default
    // rather than the rule).
    let out_busy = handover_is_busy(outgoing, mix_end);
    let in_busy = handover_is_busy(incoming, cue);
    log::info!(
        "automix clash check [{:?}]: key_clash={key_clash} tempo_gap={tempo_gap:.3} \
         vocal_conflict={vocal_conflict} vocal_overlap={vocal_overlap:.3} \
         out_busy={out_busy} in_busy={in_busy}",
        tier
    );
    let effect_wanted = effect_earned(key_clash, tempo_gap);
    if effect_wanted && allow_effects {
        if let Some(plan) = technique_plan(
            outgoing,
            incoming,
            tier,
            entry,
            Some((transition_start, fade_seconds, cue)),
            if out_busy {
                None
            } else {
                last_strong_downbeat(outgoing, mix_end)
            },
        ) {
            return plan;
        }
    }
    if !key_clash && tempo_gap > 0.07 && falls_away(out_busy, in_busy) {
        // The tempo gap is the only reason an effect was on the table, and the
        // outgoing record falls away: cross over where it does, for as long as
        // it takes, at the rate the record already plays. Both sides busy is
        // the one case with nowhere to cross, so it is not offered here.
        if let Some(plan) = clean_crossfade(outgoing, mix_end, entry) {
            return plan;
        }
    }

    TransitionPlan {
        // Beatmatched + transparent stretch → DjBlend. DjAssisted used to
        // blend on tempo proximity alone; that stretched weak grids. Filter.
        style,
        bass_swap: true,
        bass_swap_fraction,
        filter_sweep: if same_beat { 0.0 } else { FILTER_SWEEP },
        vocal_overlap,
        fade_seconds,
        outgoing_hold_seconds: 0.0,
        bridge_start_seconds: 0.0,
        bridge_seconds: 0.0,
        bridge_repeats: 0,
        effect_seconds: 0.0,
        effect_start_seconds: 0.0,
        transition_end_seconds: transition_start + fade_seconds,
        cue_seconds: cue,
        playback_rate,
        bed_fraction: shape.bed_fraction,
        bed_gain_db: shape.bed_gain_db,
        dip_depth: shape.dip_depth,
        dip_width: shape.dip_width,
        post_glide_seconds: shape.post_glide_seconds,
        outgoing_duration_seconds: outgoing.duration,
        beat_seconds: 0.0,
        reason: if effect_wanted && !allow_effects {
            // The pair earned a technique by the planner's own rules and the
            // session's pacing budget said no.
            "pacing: effect budget spent, so the pair is blended".to_string()
        } else if vocal_conflict {
            format!(
                "blend: both records sing through the handover; mids ducked {:.0}% for {:.0}% of it",
                shape.dip_depth * 100.0,
                vocal_overlap * 100.0
            )
        } else {
            String::new()
        },
    }
}

pub(super) struct MixShape {
    pub(super) bed_fraction: f64,
    pub(super) bed_gain_db: f64,
    pub(super) dip_depth: f64,
    pub(super) dip_width: f64,
    pub(super) post_glide_seconds: f64,
}

/// Bed, dip and post-blend glide for one planned blend. A plain crossfade
/// gets zeros, which is how the mixer knows to stay equal-power.
pub(super) fn mix_shape(
    style: TransitionStyle,
    same_beat: bool,
    vocal_overlap: f64,
    vocal_conflict: bool,
    fade_seconds: f64,
    beat_seconds: f64,
    swap_fraction: f64,
    playback_rate: f64,
    bpm: f64,
) -> MixShape {
    if !matches!(style, TransitionStyle::DjBlend | TransitionStyle::DjFilter) {
        return MixShape {
            bed_fraction: 0.0,
            bed_gain_db: 0.0,
            dip_depth: 0.0,
            dip_width: 0.0,
            post_glide_seconds: 0.0,
        };
    }
    let clash = vocal_overlap
        .clamp(0.0, 1.0)
        .max(if vocal_conflict { 0.7 } else { 0.0 });
    let dip = if same_beat {
        (0.28 + 0.5 * clash).clamp(0.0, MAX_DUCK_DEPTH)
    } else {
        (0.2 * clash).clamp(0.0, MAX_DUCK_DEPTH)
    };
    let rise = if fade_seconds > 0.0 && beat_seconds > 0.0 {
        (2.0 * beat_seconds / fade_seconds).clamp(0.08, 0.3)
    } else {
        0.15
    };
    let bed = (swap_fraction - rise).clamp(0.15, 0.8);
    let dip_width = if fade_seconds > 0.0 && beat_seconds > 0.0 {
        (beat_seconds / fade_seconds).clamp(0.02, 0.2)
    } else {
        0.06
    };
    let post_glide_seconds = if same_beat && (playback_rate - 1.0).abs() > 1e-3 && bpm > 0.0 {
        POST_BLEND_GLIDE_BEATS * 60.0 / bpm
    } else {
        0.0
    };
    MixShape {
        bed_fraction: bed,
        bed_gain_db: BED_GAIN_DB,
        dip_depth: dip,
        dip_width,
        post_glide_seconds,
    }
}

// ---- DJ techniques ---------------------------------------------------------
//
// A blend needs two grids that agree. Most pairs an autoplay run produces do
// not: different tempos, different genres, a vocal on both sides. A DJ does not
// fade those — they finish the outgoing record with an effect and drop the next
// one on its own downbeat. [`technique_plan`] picks which effect, from what the
// two ends of the pair actually sound like, and returns `None` only when a
// blend is the better move (a real beatmatch) or nothing is known.

/// Mean energy over `[start, end]`, 0…1-ish; `None` with no curve.
pub(super) fn energy_between(analysis: &Analysis, start: f64, end: f64) -> Option<f64> {
    let values: Vec<f64> = analysis
        .energy_curve
        .iter()
        .filter(|p| p.time.is_finite() && p.time >= start && p.time <= end && p.energy.is_finite())
        .map(|p| p.energy)
        .collect();
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// Deterministic variety: the same pair always gets the same technique (so a
/// replan does not flip it), different pairs spread across the eligible set.
pub(super) fn pair_salt(outgoing: &Analysis, incoming: &Analysis) -> usize {
    let a = (outgoing.duration * 1000.0) as u64;
    let b = (incoming.duration * 1000.0) as u64;
    let c = (outgoing.bpm * 100.0) as u64 ^ ((incoming.bpm * 100.0) as u64).rotate_left(17);
    (a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.rotate_left(29) ^ c) as usize
}

/// Whether a pair's blend is a clash an effect should season. The 4–7 %
/// DjFilter band is still a usable blend; only a gap past seven percent or a key
/// the stretch cannot reconcile earns an effect.
///
/// A vocal overlap is deliberately *not* on this list. Two records singing at
/// once is what a filter blend and the mid duck are for — `mix_shape` deepens
/// the dip with the overlap, and the style falls to DjFilter — so the answer is
/// a ducked blend rather than an effect that throws the outgoing record away.
/// The overlap measured by `simultaneous_vocal_fraction` runs 0.0–1.0 across the
/// corpus, so it can tell a real one from silence; it is simply not evidence
/// that a blend is impossible.
pub(super) fn effect_earned(key_clash: bool, tempo_gap: f64) -> bool {
    key_clash || tempo_gap > 0.07
}

/// Does at least one side fall away under the handover?
///
/// Principle H: an unmatched pair is answered with a clean crossfade *when
/// there is somewhere to cross over*. That means the outgoing record's tail is
/// quieter than the record, or the incoming's opening is — the two cases where
/// an equal-power fade has room to work. Two records still at full energy on
/// both sides of the join leave nothing to fade through, and that is when a
/// technique is the musical answer.
pub(super) fn falls_away(out_busy: bool, in_busy: bool) -> bool {
    !(out_busy && in_busy)
}
