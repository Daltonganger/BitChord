//! Vocal activity and clashes across a handover.

use super::analysis::Analysis;
use super::cue::beat_seconds_of;
use super::{VOCAL_ACTIVE_THRESHOLD, VOCAL_CLASH_TOLERANCE};

/// Mean vocal activity over `start`..`end` on a track's own timeline (upstream
/// `vocalActivityBetween`). Null when the mask is absent or the window is empty.
pub(super) fn vocal_activity_between(analysis: &Analysis, start: f64, end: f64) -> Option<f64> {
    let mask = &analysis.vocal_activity_mask;
    let curve = &analysis.energy_curve;
    if mask.is_empty() || mask.len() != curve.len() || end <= start {
        return None;
    }
    let mut sum = 0.0;
    let mut count = 0usize;
    for index in 0..mask.len() {
        let time = curve[index].time;
        if !time.is_finite() || time < start || time > end {
            continue;
        }
        let value = mask[index];
        if !value.is_finite() {
            continue;
        }
        sum += value;
        count += 1;
    }
    if count > 0 {
        Some(sum / count as f64)
    } else {
        None
    }
}

/// Both windows measurably singing at once (upstream `isVocalClash`).
pub(super) fn is_vocal_clash(
    outgoing_activity: Option<f64>,
    incoming_activity: Option<f64>,
) -> bool {
    matches!((outgoing_activity, incoming_activity),
        (Some(out), Some(inc)) if out >= VOCAL_ACTIVE_THRESHOLD && inc >= VOCAL_ACTIVE_THRESHOLD)
}

/// Do both records sing through the handover itself?
///
/// The two windows are the outgoing's last two bars before its mix-out anchor
/// and the incoming's first two bars from its cue — the audio that will
/// actually be heard together. A record that ends on an instrumental outro
/// reads 0.28 here while its whole-track probability said 0.85, which is why
/// the whole-track number could not be the trigger.
pub(super) fn handover_vocal_clash(
    outgoing: &Analysis,
    incoming: &Analysis,
    mix_out_anchor: f64,
    incoming_cue: f64,
) -> bool {
    let out_beats = beat_seconds_of(outgoing).max(0.25);
    let in_beats = beat_seconds_of(incoming).max(0.25);
    let out_end = if mix_out_anchor > 0.0 {
        mix_out_anchor
    } else if outgoing.content_end > 0.0 {
        outgoing.content_end
    } else {
        outgoing.duration
    };
    let in_start = incoming_cue.max(0.0);
    let out_tail = vocal_activity_between(outgoing, out_end - 8.0 * out_beats, out_end);
    let in_head = vocal_activity_between(incoming, in_start, in_start + 8.0 * in_beats);
    is_vocal_clash(out_tail, in_head)
}

/// The fraction of a planned overlap where **both** tracks sing at the same
/// instant (upstream `simultaneousVocalFraction`), mapping the outgoing
/// energy-curve samples onto the incoming timeline through `rate`.
pub(super) fn simultaneous_vocal_fraction(
    outgoing: &Analysis,
    incoming: &Analysis,
    out_start: f64,
    out_end: f64,
    in_start: f64,
    rate: f64,
) -> Option<f64> {
    let out_mask = &outgoing.vocal_activity_mask;
    let out_curve = &outgoing.energy_curve;
    let in_mask = &incoming.vocal_activity_mask;
    let in_curve = &incoming.energy_curve;
    if out_mask.is_empty() || out_mask.len() != out_curve.len() {
        return None;
    }
    if in_mask.is_empty() || in_mask.len() != in_curve.len() {
        return None;
    }
    if out_end <= out_start {
        return None;
    }
    let step = if rate.is_finite() && rate > 0.0 {
        rate
    } else {
        1.0
    };
    let mut in_index = 0usize;
    let mut both = 0usize;
    let mut total = 0usize;
    for index in 0..out_mask.len() {
        let time = out_curve[index].time;
        if !time.is_finite() || time < out_start {
            continue;
        }
        if time > out_end {
            break;
        }
        total += 1;
        if out_mask[index] < VOCAL_ACTIVE_THRESHOLD {
            continue;
        }
        let target = in_start + (time - out_start) * step;
        while in_index + 1 < in_curve.len() && in_curve[in_index + 1].time <= target {
            in_index += 1;
        }
        if in_index < in_mask.len() && in_mask[in_index] >= VOCAL_ACTIVE_THRESHOLD {
            both += 1;
        }
    }
    if total > 0 {
        Some(both as f64 / total as f64)
    } else {
        None
    }
}

/// The render-time vocal separation amount, measured instant-by-instant over the
/// windows the plan actually blends (upstream `plannedVocalOverlap`).
pub(super) fn planned_vocal_overlap(
    outgoing: &Analysis,
    incoming: &Analysis,
    transition_start: f64,
    transition_end: f64,
    incoming_cue_time: f64,
    incoming_playback_rate: f64,
) -> f64 {
    let outgoing_span = transition_end - transition_start;
    if outgoing_span <= 0.0 || !outgoing_span.is_finite() {
        return 0.0;
    }
    let rate = if incoming_playback_rate.is_finite() && incoming_playback_rate > 0.0 {
        incoming_playback_rate
    } else {
        1.0
    };
    simultaneous_vocal_fraction(
        outgoing,
        incoming,
        transition_start,
        transition_end,
        incoming_cue_time,
        rate,
    )
    .unwrap_or(0.0)
}

// ---- WSOLA beat-matched phrase-switch (upstream `planWsolaTransition`) ------

pub(super) fn nearest_at_or_before(values: &[f64], target: f64) -> Option<f64> {
    values
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v >= 0.0 && *v <= target)
        .max_by(|a, b| a.total_cmp(b))
}

pub(super) fn nearest_value(values: &[f64], target: f64, tolerance: f64) -> Option<f64> {
    values
        .iter()
        .copied()
        .filter(|v| v.is_finite() && (*v - target).abs() <= tolerance)
        .min_by(|a, b| (a - target).abs().total_cmp(&(b - target).abs()))
}

/// The earliest point the analysis claims the track makes sound (upstream
/// `audibleStartOf`).
pub(super) fn audible_start_of(analysis: &Analysis) -> f64 {
    let mut best = f64::INFINITY;
    for value in [analysis.audible_start_time, analysis.first_beat] {
        if value.is_finite() && value >= 0.0 {
            best = best.min(value);
        }
    }
    if best.is_finite() {
        best
    } else {
        0.0
    }
}

/// One pass of the vocal-clash gate (upstream `clashOver`).
#[allow(clippy::too_many_arguments)]
pub(super) fn clash_over(
    outgoing: &Analysis,
    incoming: &Analysis,
    overlap_end_target: f64,
    outgoing_beat_seconds: f64,
    incoming_beat_seconds: f64,
    incoming_drop_time: f64,
    audible_start: f64,
    stretch_ratio: f64,
    beats: usize,
) -> bool {
    let out_start = overlap_end_target - beats as f64 * outgoing_beat_seconds;
    let in_start = audible_start.max(incoming_drop_time - beats as f64 * incoming_beat_seconds);
    let out_vocal = vocal_activity_between(outgoing, out_start, overlap_end_target);
    let in_vocal = vocal_activity_between(incoming, in_start, incoming_drop_time);
    let simultaneous = simultaneous_vocal_fraction(
        outgoing,
        incoming,
        out_start,
        overlap_end_target,
        in_start,
        stretch_ratio,
    );
    if simultaneous.is_some_and(|s| s > VOCAL_CLASH_TOLERANCE) {
        return true;
    }
    if is_vocal_clash(out_vocal, in_vocal) {
        return true;
    }
    if beats > 8 && out_vocal.is_some_and(|v| v >= VOCAL_ACTIVE_THRESHOLD) {
        let deep_vocal = vocal_activity_between(
            outgoing,
            out_start,
            overlap_end_target - 8.0 * outgoing_beat_seconds,
        );
        if deep_vocal.is_some_and(|v| v >= VOCAL_ACTIVE_THRESHOLD) {
            return true;
        }
    }
    false
}
