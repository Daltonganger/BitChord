//! Where the outgoing leaves: the mix-out anchor, bass-swap timing, and how
//! much audible music each candidate discards.

use super::analysis::{low_energy_change, low_energy_reference, low_energy_resolution, Analysis};
use super::{
    audio_analysis, AUDIBLE_ENERGY_FRACTION, BASS_SWAP_MAX_SECONDS, DEFAULT_BASS_SWAP_FRACTION,
    HANDOFF_FRACTION, MAX_BASS_SWAP_FRACTION, MAX_DISCARDED_MUSIC_SECONDS,
    MIN_BASS_STRUCTURE_SCORE,
};

/// Chooses one shared-grid beat for the low-end handoff from the low-energy
/// structure of both tracks (upstream `bassSwapFractionFor`).
pub(super) fn bass_swap_fraction_for(
    outgoing: &Analysis,
    incoming: &Analysis,
    transition_start: f64,
    incoming_cue_time: f64,
    outgoing_beat_seconds: f64,
    incoming_beat_seconds: f64,
    overlap_seconds: f64,
    overlap_beats: usize,
) -> f64 {
    if overlap_seconds <= 0.0 {
        return DEFAULT_BASS_SWAP_FRACTION;
    }
    let latest_fraction =
        (MAX_BASS_SWAP_FRACTION.min(BASS_SWAP_MAX_SECONDS / overlap_seconds)).clamp(0.0, 1.0);
    let prior = DEFAULT_BASS_SWAP_FRACTION.min(latest_fraction);
    if overlap_beats < 2 {
        return prior;
    }

    let earliest_fraction = HANDOFF_FRACTION.min(latest_fraction);
    let earliest_beat = ((earliest_fraction * overlap_beats as f64 - 1e-9).ceil() as usize)
        .clamp(1, overlap_beats - 1);
    let latest_beat = ((latest_fraction * overlap_beats as f64 + 1e-9).floor() as usize)
        .clamp(earliest_beat, overlap_beats - 1);
    let candidates: Vec<usize> = (earliest_beat..=latest_beat).collect();
    // `minWithOrNull`: closest to the prior, then beat-on-a-bar preferred.
    let fallback_beat = match candidates.iter().min_by(|a, b| {
        let da = (**a as f64 / overlap_beats as f64 - prior).abs();
        let db = (**b as f64 / overlap_beats as f64 - prior).abs();
        da.total_cmp(&db).then_with(|| {
            (if **a % 4 == 0 { 0 } else { 1 }).cmp(&(if **b % 4 == 0 { 0 } else { 1 }))
        })
    }) {
        Some(beat) => *beat,
        None => return prior,
    };

    let outgoing_reference = low_energy_reference(&outgoing.low_energy_curve);
    let incoming_reference = low_energy_reference(&incoming.low_energy_curve);
    let outgoing_window =
        outgoing_beat_seconds.max(low_energy_resolution(&outgoing.low_energy_curve) * 1.1);
    let incoming_window =
        incoming_beat_seconds.max(low_energy_resolution(&incoming.low_energy_curve) * 1.1);

    let mut strongest: Option<(usize, f64)> = None;
    for beat in &candidates {
        let outgoing_at = transition_start + *beat as f64 * outgoing_beat_seconds;
        let incoming_at = incoming_cue_time + *beat as f64 * incoming_beat_seconds;
        let incoming_change = low_energy_change(
            &incoming.low_energy_curve,
            incoming_reference,
            incoming_at,
            incoming_window,
        );
        let outgoing_change = low_energy_change(
            &outgoing.low_energy_curve,
            outgoing_reference,
            outgoing_at,
            outgoing_window,
        );
        // Instrument-activity proxy: prefer the beat where outgoing bass falls
        // and incoming bass rises (Music Understanding–style activity masks).
        let out_bass_before = mask_activity_between(
            &outgoing.bass_activity_mask,
            &outgoing.energy_curve,
            outgoing_at - outgoing_window,
            outgoing_at,
        );
        let out_bass_after = mask_activity_between(
            &outgoing.bass_activity_mask,
            &outgoing.energy_curve,
            outgoing_at,
            outgoing_at + outgoing_window,
        );
        let in_bass_before = mask_activity_between(
            &incoming.bass_activity_mask,
            &incoming.energy_curve,
            incoming_at - incoming_window,
            incoming_at,
        );
        let in_bass_after = mask_activity_between(
            &incoming.bass_activity_mask,
            &incoming.energy_curve,
            incoming_at,
            incoming_at + incoming_window,
        );
        let bass_handoff = match (
            out_bass_before,
            out_bass_after,
            in_bass_before,
            in_bass_after,
        ) {
            (Some(ob), Some(oa), Some(ib), Some(ia)) => (ob - oa) + (ia - ib),
            _ => 0.0,
        };
        if incoming_change.is_none() && outgoing_change.is_none() && bass_handoff.abs() < 0.05 {
            continue;
        }
        let score =
            incoming_change.unwrap_or(0.0) - outgoing_change.unwrap_or(0.0) + bass_handoff * 0.35;
        let candidate = (*beat, score);
        strongest = Some(match strongest {
            None => candidate,
            Some(current) => {
                // `maxWithOrNull`: score desc, then beat-on-bar, then closest to prior.
                let mut better = candidate.1 > current.1;
                if candidate.1 == current.1 {
                    let c_bar = if candidate.0 % 4 == 0 { 1 } else { 0 };
                    let s_bar = if current.0 % 4 == 0 { 1 } else { 0 };
                    better = c_bar > s_bar;
                    if c_bar == s_bar {
                        better = (candidate.0 as f64 / overlap_beats as f64 - prior).abs()
                            < (current.0 as f64 / overlap_beats as f64 - prior).abs();
                    }
                }
                if better {
                    candidate
                } else {
                    current
                }
            }
        });
    }

    let chosen_beat = match strongest {
        Some((beat, score)) if score >= MIN_BASS_STRUCTURE_SCORE => beat,
        _ => fallback_beat,
    };
    chosen_beat as f64 / overlap_beats as f64
}

// ---- Mix-out anchor ranking (upstream `rankMixOutCandidates`) ---------------

pub(super) fn mix_out_type_score(kind: &str) -> f64 {
    match kind {
        "energy_cliff" | "interior_mix_out" => 0.95,
        "outro_start" => 0.9,
        "section_boundary" => 0.88,
        "content_end" => 0.75,
        _ => 0.0,
    }
}

/// Audible seconds (energy ≥ 10 % of the track reference) between `start` and
/// `end`, measured on the energy curve (upstream `audibleSecondsBetween`).
pub(super) fn audible_seconds_between(
    curve: &[audio_analysis::EnergyPoint],
    start: f64,
    end: f64,
) -> Option<f64> {
    if curve.len() < 2 || end <= start {
        return None;
    }
    let mut energies: Vec<f64> = curve
        .iter()
        .map(|p| p.energy)
        .filter(|e| e.is_finite() && *e >= 0.0)
        .collect();
    if energies.is_empty() {
        return None;
    }
    energies.sort_by(|a, b| a.total_cmp(b));
    let reference = energies[((energies.len() - 1) as f64 * 0.85) as usize];
    if reference <= 0.0 {
        return Some(0.0);
    }
    let threshold = reference * AUDIBLE_ENERGY_FRACTION;
    let first = curve.first().unwrap().time;
    let last = curve.last().unwrap().time;
    if !first.is_finite() || !last.is_finite() || last <= first {
        return None;
    }
    let sample_seconds = (last - first) / (curve.len() - 1) as f64;
    let mut audible = 0.0;
    for point in curve {
        if !point.time.is_finite() || point.time < start || point.time > end {
            continue;
        }
        if point.energy >= threshold {
            audible += sample_seconds;
        }
    }
    Some(audible)
}

/// Mean of a parallel activity mask over `start`..`end` (same grid as energy).
pub(super) fn mask_activity_between(
    mask: &[f64],
    curve: &[audio_analysis::EnergyPoint],
    start: f64,
    end: f64,
) -> Option<f64> {
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

/// Bonus when pace drops across a candidate mix-out (structure + novelty cue).
pub(super) fn pace_drop_bonus(analysis: &Analysis, time: f64) -> f64 {
    if analysis.pace_curve.len() < 4 {
        return 0.0;
    }
    let before = analysis
        .pace_curve
        .iter()
        .filter(|p| p.time >= time - 4.0 && p.time < time)
        .map(|p| p.energy)
        .collect::<Vec<_>>();
    let after = analysis
        .pace_curve
        .iter()
        .filter(|p| p.time >= time && p.time <= time + 4.0)
        .map(|p| p.energy)
        .collect::<Vec<_>>();
    if before.is_empty() || after.is_empty() {
        return 0.0;
    }
    let mean = |vals: &[f64]| vals.iter().sum::<f64>() / vals.len() as f64;
    let drop = mean(&before) - mean(&after);
    if drop > 0.08 {
        0.06
    } else if drop > 0.03 {
        0.03
    } else {
        0.0
    }
}

/// Where the outgoing track's transition ends: the best-ranked mix-out
/// candidate inside the discarded-music budget, or the content end. Returns
/// `(time, discarded_music_seconds)` (upstream `resolveMixOutAnchor`).
pub(super) fn resolve_mix_out_anchor(outgoing: &Analysis) -> (f64, f64) {
    let end = if outgoing.content_end > 0.0 {
        outgoing.content_end
    } else {
        outgoing.duration
    };
    if end <= 0.0 {
        return (0.0, 0.0);
    }
    let mut candidates = outgoing.mix_out_candidates.clone();
    if !candidates.iter().any(|c| (c.time - end).abs() < 0.05) {
        candidates.push(audio_analysis::MixCuePoint {
            time: end,
            score: 0.75,
            kind: "content_end".into(),
        });
    }

    let mut best: Option<(f64, f64, f64)> = None; // (rank_score, time, discarded)
    for candidate in &candidates {
        let measured = audible_seconds_between(&outgoing.energy_curve, candidate.time, end);
        let discarded = measured.unwrap_or((end - candidate.time).max(0.0));
        if discarded > MAX_DISCARDED_MUSIC_SECONDS {
            continue;
        }
        let rank_score = candidate.score
            + mix_out_type_score(&candidate.kind)
            + pace_drop_bonus(outgoing, candidate.time);
        best = Some(match best {
            None => (rank_score, candidate.time, discarded),
            Some((score, time, discarded_seconds)) => {
                if rank_score > score || (rank_score == score && candidate.time > time) {
                    (rank_score, candidate.time, discarded)
                } else {
                    (score, time, discarded_seconds)
                }
            }
        });
    }
    match best {
        Some((_, time, discarded)) => (time, discarded),
        None => (end, 0.0),
    }
}

// ---- Harmonic key routing (upstream TransitionPlanner key helpers) ----------
