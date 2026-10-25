//! WSOLA tempo-matched transitions.

use super::analysis::{Analysis, Tier};
use super::choose::mix_shape;
use super::cue::{cue_for, incoming_entry, incoming_handoff};
use super::edges::{align_tempo_octave, assess, transparent_playback_rate};
use super::key::{harmonically_compatible, trusted_key};
use super::mix_out::{bass_swap_fraction_for, resolve_mix_out_anchor};
use super::technique::long_blend_allowed;
use super::vocal_overlap::{
    audible_start_of, clash_over, handover_vocal_clash, nearest_at_or_before, planned_vocal_overlap,
};
use super::{
    ARRANGEMENT_OVERLAP_BEATS, LONG_BLEND_BEATS, LONG_BLEND_SECONDS, MAX_BEATMATCH_STRETCH,
    MAX_DISCARDED_MUSIC_SECONDS, MIN_CLEARANCE_SECONDS, MIN_FADE_BEATS, UPSTREAM_MAX_FADE_BEATS,
    UPSTREAM_MAX_OVERLAP_SECONDS,
};
use crate::mixer::{TransitionPlan, TransitionStyle};

/// Plans one beat-matched transition (upstream `planWsolaTransition`). `None`
/// is a routing decision — the caller falls back to the adaptive overlap.
pub(super) fn plan_wsola_transition(
    outgoing: &Analysis,
    incoming: &Analysis,
) -> Option<TransitionPlan> {
    if assess(outgoing, incoming) != Tier::Beatmatched {
        return None;
    }
    let outgoing_bpm = outgoing.bpm;
    if outgoing_bpm <= 0.0 || incoming.bpm <= 0.0 {
        return None;
    }
    let incoming_bpm = align_tempo_octave(outgoing_bpm, incoming.bpm);
    // Refuse the phrase-switch rather than warp past the transparent band —
    // the adaptive-overlap path will offer a filtered handoff instead.
    let stretch_ratio =
        transparent_playback_rate(outgoing_bpm, incoming.bpm, MAX_BEATMATCH_STRETCH)?;

    let outgoing_length = outgoing.duration;
    let incoming_length = incoming.duration;
    if outgoing_length <= 0.0 || incoming_length <= 0.0 {
        return None;
    }

    let incoming_beat_seconds = 60.0 / incoming_bpm;
    let outgoing_beat_seconds = 60.0 / outgoing_bpm;

    let long = long_blend_allowed(outgoing, incoming);
    let max_seconds = if long {
        LONG_BLEND_SECONDS
    } else {
        UPSTREAM_MAX_OVERLAP_SECONDS
    };
    let max_beats = if long {
        LONG_BLEND_BEATS
    } else {
        UPSTREAM_MAX_FADE_BEATS
    };
    // The drop, or the furthest handoff the overlap can reach. Never the cue.
    let incoming_drop_time = incoming_handoff(incoming, max_seconds);

    let content_end = if outgoing.content_end > 0.0 {
        outgoing.content_end
    } else {
        outgoing_length
    };
    let (mix_out_anchor, _) = resolve_mix_out_anchor(outgoing);
    let unshifted_overlap_end = outgoing_length.min(mix_out_anchor);
    let outgoing_arrangement_overlap = if (mix_out_anchor - content_end).abs() < 0.05 {
        (ARRANGEMENT_OVERLAP_BEATS * outgoing_beat_seconds).min(MAX_DISCARDED_MUSIC_SECONDS)
    } else {
        0.0
    };
    let overlap_end_target =
        MIN_CLEARANCE_SECONDS.max(unshifted_overlap_end - outgoing_arrangement_overlap);

    let audible_start = audible_start_of(incoming);
    let available_fade_beats =
        (incoming_drop_time - audible_start).max(0.0) / incoming_beat_seconds;
    let capped_by_overlap = ((max_seconds / incoming_beat_seconds).floor() as usize / 4) * 4;
    if capped_by_overlap < MIN_FADE_BEATS {
        return None;
    }
    let mut fade_beats = max_beats
        .min(capped_by_overlap)
        .min(((available_fade_beats / 4.0).floor() as usize) * 4);
    if fade_beats < MIN_FADE_BEATS {
        fade_beats = MIN_FADE_BEATS;
    }

    while fade_beats > MIN_FADE_BEATS
        && clash_over(
            outgoing,
            incoming,
            overlap_end_target,
            outgoing_beat_seconds,
            incoming_beat_seconds,
            incoming_drop_time,
            audible_start,
            stretch_ratio,
            fade_beats,
        )
    {
        fade_beats -= 4;
    }

    let coverable_beats =
        ((incoming_drop_time - audible_start).max(0.0) / incoming_beat_seconds).floor() as usize;
    let overlap_beats = fade_beats.min(coverable_beats);
    if overlap_beats < 1 {
        return None;
    }

    let outgoing_overlap_seconds = overlap_beats as f64 * outgoing_beat_seconds;
    let overlap_seconds = overlap_beats as f64 * incoming_beat_seconds;

    let requested_incoming_handoff =
        incoming_drop_time + ARRANGEMENT_OVERLAP_BEATS * incoming_beat_seconds;
    let max_incoming_handoff = incoming_length - MIN_CLEARANCE_SECONDS;
    if max_incoming_handoff < incoming_drop_time {
        return None;
    }
    let incoming_handoff_time = requested_incoming_handoff.min(max_incoming_handoff);
    let entry = incoming_entry(incoming);
    let incoming_cue_time = cue_for(entry, incoming_handoff_time, overlap_seconds, stretch_ratio);
    if incoming_cue_time < audible_start - 0.5 {
        return None;
    }

    let start_target = overlap_end_target - outgoing_overlap_seconds;
    let transition_start =
        nearest_at_or_before(&outgoing.downbeats, start_target).unwrap_or(start_target);
    if transition_start < MIN_CLEARANCE_SECONDS {
        return None;
    }
    let transition_end = transition_start + outgoing_overlap_seconds;
    if transition_end > outgoing_length + 0.05 {
        return None;
    }

    let incoming_resume_time = incoming_cue_time + overlap_seconds;
    if incoming_resume_time + MIN_CLEARANCE_SECONDS > incoming_length {
        return None;
    }

    let bass_swap_fraction = bass_swap_fraction_for(
        outgoing,
        incoming,
        transition_start,
        incoming_cue_time,
        outgoing_beat_seconds,
        incoming_beat_seconds,
        overlap_seconds,
        overlap_beats,
    );
    let vocal_overlap = planned_vocal_overlap(
        outgoing,
        incoming,
        transition_start,
        transition_end,
        incoming_cue_time,
        stretch_ratio,
    );

    // The whole-track vocal probability cannot answer "does this handover put
    // two singers together": it sits at 0.82–0.86 on all thirteen corpus
    // tracks, sung or not. The windows the handover actually covers do
    // discriminate — 0.28 through an instrumental outro, 0.91 through a sung
    // one — so the clash is read there.
    let (anchor, _) = resolve_mix_out_anchor(outgoing);
    let vocal_conflict = handover_vocal_clash(outgoing, incoming, anchor, entry);
    let style = TransitionStyle::DjBlend;
    let playback_rate = stretch_ratio;
    let fade_seconds = transition_end - transition_start;
    let shape = mix_shape(
        style,
        true,
        vocal_overlap,
        vocal_conflict,
        fade_seconds,
        outgoing_beat_seconds,
        bass_swap_fraction,
        playback_rate,
        outgoing_bpm,
    );
    Some(TransitionPlan {
        style,
        bass_swap: true,
        bass_swap_fraction,
        filter_sweep: 0.0,
        vocal_overlap,
        fade_seconds,
        outgoing_hold_seconds: 0.0,
        bridge_start_seconds: 0.0,
        bridge_seconds: 0.0,
        bridge_repeats: 0,
        effect_seconds: 0.0,
        effect_start_seconds: 0.0,
        transition_end_seconds: transition_end,
        cue_seconds: incoming_cue_time,
        playback_rate,
        bed_fraction: shape.bed_fraction,
        bed_gain_db: shape.bed_gain_db,
        dip_depth: shape.dip_depth,
        dip_width: shape.dip_width,
        post_glide_seconds: shape.post_glide_seconds,
        outgoing_duration_seconds: outgoing.duration,
        beat_seconds: 0.0,
        reason: String::new(),
    })
}

/// The most ambitious move: run the incoming intro under the outgoing and close
/// on its drop, gated on harmonic compatibility (upstream `phraseSwitch`).
pub(super) fn phrase_switch(outgoing: &Analysis, incoming: &Analysis) -> Option<TransitionPlan> {
    if !harmonically_compatible(&trusted_key(outgoing), &trusted_key(incoming)) {
        return None;
    }
    plan_wsola_transition(outgoing, incoming)
}
