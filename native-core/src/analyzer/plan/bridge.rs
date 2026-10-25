//! Bar-aligned bridges that cover a gap with a repeated outgoing unit.

use super::analysis::{fold_tempo, Analysis};
use super::cue::beat_seconds_of;
use super::edges::final_chord_attack;
use super::vocal_overlap::vocal_activity_between;
use super::MAX_BRIDGED_SECONDS;
use crate::mixer::{TransitionPlan, TransitionStyle};

/// F's bridge decision for one clean crossfade.
#[derive(Clone, Copy, Debug)]
pub(super) struct BridgeUnit {
    pub(super) start: f64,
    pub(super) seconds: f64,
    pub(super) bars: usize,
}

/// The peak of the vocal mask in `[start, end]`, beside the mean
/// [`vocal_activity_between`] returns. The UMX mask has a low instrumental
/// floor (about 0.28 on the corpus), so "no voice" is a low peak as well as a
/// low mean.
pub(super) fn vocal_peak_between(analysis: &Analysis, start: f64, end: f64) -> Option<f64> {
    let mask = &analysis.vocal_activity_mask;
    let curve = &analysis.energy_curve;
    if mask.is_empty() || mask.len() != curve.len() || end <= start {
        return None;
    }
    let mut peak = 0.0f64;
    let mut seen = false;
    for (index, value) in mask.iter().enumerate() {
        let time = curve[index].time;
        if !time.is_finite() || time < start || time > end || !value.is_finite() {
            continue;
        }
        peak = peak.max(*value);
        seen = true;
    }
    seen.then_some(peak)
}

/// The "no voice" bar for an outro window: mean and peak. The model's
/// instrumental floor is about 0.28, so this admits an instrumental outro and
/// rejects a sung one. F's bridge and L1's phrase extension read the same bar.
pub(super) const VOCAL_FREE_MEAN_MAX: f64 = 0.35;
pub(super) const VOCAL_FREE_PEAK_MAX: f64 = 0.55;

/// F-3: the chosen bar grid only has to be *usable*: enough bars, a median
/// spacing in a musical range. The 15 ms test the instruction names is per
/// **unit** — the loop's own steps are measured against the grid in
/// [`bridge_unit`] — because the PLL's bars drift by 50-90 ms over a whole
/// track while staying locally regular, which is exactly what a two-bar loop
/// can use.
pub(super) fn bar_grid_usable(analysis: &Analysis) -> bool {
    let bars = &analysis.downbeats;
    if bars.len() < 5 {
        return false;
    }
    let mut gaps: Vec<f64> = bars
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|g| g.is_finite() && *g > 0.0)
        .collect();
    if gaps.len() < 4 {
        return false;
    }
    gaps.sort_by(|a, b| a.total_cmp(b));
    (0.4..=8.0).contains(&gaps[gaps.len() / 2])
}

/// F: the unit to loop, or why there is none.
///
/// Only a clean crossfade is eligible. The unit is a whole number of bars
/// (two, or four on a record at 140 BPM or faster when the cap allows), starts
/// on a phrase boundary, ends on a downbeat at or before the record's last
/// chord (or its content end), is measured from the downbeats themselves, and
/// is vocal-free. Nothing here is tuned to the corpus: the thresholds are the
/// model's own floor and the 15 ms grid tolerance.
pub(super) fn bridge_unit(
    outgoing: &Analysis,
    plan: &TransitionPlan,
) -> Result<BridgeUnit, &'static str> {
    if !(plan.reason.starts_with("crossfade:")
        || plan.reason.starts_with("filter fade:")
        || plan.reason.starts_with("cold ending:"))
    {
        return Err("not a clean crossfade");
    }
    if beat_seconds_of(outgoing) <= 0.0 || !bar_grid_usable(outgoing) {
        return Err("no regular bar grid");
    }
    let start = plan.transition_end_seconds - plan.fade_seconds;
    let chord = final_chord_attack(outgoing);
    // E-2's protected chord is the unit's ceiling. Without a hold a chord may
    // still sit after the handover (an interior mix-out discards it); it only
    // rules a unit out when it falls *inside* that unit, checked below.
    let point = if plan.outgoing_hold_seconds > 0.0 {
        chord
            .map(|c| c.min(plan.transition_end_seconds))
            .unwrap_or(plan.transition_end_seconds)
    } else {
        plan.transition_end_seconds
    };
    if point <= start + 0.5 || outgoing.downbeats.len() < 5 {
        return Err("the blend is shorter than one unit");
    }
    let beat = beat_seconds_of(outgoing);
    let fast = fold_tempo(outgoing.bpm) > 140.0;
    let orders: &[usize] = if fast { &[4, 2] } else { &[2] };
    let mut failure: &'static str = "no phrase-aligned unit after the blend start";
    for &bars in orders {
        let ideal = 4.0 * bars as f64 * beat;
        if plan.fade_seconds + ideal > MAX_BRIDGED_SECONDS {
            failure = "the unit would push the fade past 16s";
            continue;
        }
        // The last eligible unit, closest to the record's ending.
        for j in (0..outgoing.downbeats.len()).rev() {
            let end = outgoing.downbeats[j];
            if !end.is_finite() || end > point + 1e-6 || j < bars {
                continue;
            }
            let i = j - bars;
            // A phrase boundary: bar 0, 2, 4… from the first downbeat.
            if i % 2 != 0 {
                continue;
            }
            let begin = outgoing.downbeats[i];
            // The unit has to lie inside the blend. A 10 ms numerical
            // allowance, inside the 15 ms grid tolerance, covers the
            // difference between the planner's phrase and the measured span.
            if !begin.is_finite() || begin < start - 0.010 {
                continue;
            }
            let mut grid_ok = true;
            for k in 0..=bars {
                let expected = begin + k as f64 * 4.0 * beat;
                if (outgoing.downbeats[i + k] - expected).abs() > 0.015 {
                    grid_ok = false;
                    break;
                }
            }
            if !grid_ok {
                failure = "the unit's downbeats drift more than 15 ms";
                continue;
            }
            if chord.is_some_and(|c| c > begin + 0.05 && c < end - 0.05) {
                failure = "a final chord sits in the unit";
                continue;
            }
            let measured = end - begin;
            if measured <= 0.5 {
                continue;
            }
            if plan.fade_seconds + measured > MAX_BRIDGED_SECONDS {
                failure = "the unit would push the fade past 16s";
                continue;
            }
            let Some(mean) = vocal_activity_between(outgoing, begin, end) else {
                continue;
            };
            let peak = vocal_peak_between(outgoing, begin, end).unwrap_or(1.0);
            if mean > VOCAL_FREE_MEAN_MAX || peak > VOCAL_FREE_PEAK_MAX {
                failure = "the unit has a voice";
                continue;
            }
            return Ok(BridgeUnit {
                start: begin,
                seconds: measured,
                bars,
            });
        }
    }
    Err(failure)
}

/// Extends a clean crossfade with one replay of a measured, vocal-free unit of
/// the outgoing's outro. The record's end (and a cold ending's hold) move by
/// the unit; the blend start does not move, so the incoming enters where it did
/// and the outgoing has one more unit of music to cross under it.
pub(super) fn apply_bridge(outgoing: &Analysis, plan: &mut TransitionPlan) {
    // The clean crossfade is a filter fade now; the bridge extends either.
    if !matches!(
        plan.style,
        TransitionStyle::EqualPower | TransitionStyle::DjFilter
    ) || plan.effect_seconds > 0.0
        || plan.bass_swap
    {
        return;
    }
    match bridge_unit(outgoing, plan) {
        Ok(unit) => {
            plan.transition_end_seconds += unit.seconds;
            plan.fade_seconds += unit.seconds;
            // A cold ending's hold covers the unit; an ordinary crossfade has
            // no hold and its fall starts where it did.
            if plan.outgoing_hold_seconds > 0.0 {
                plan.outgoing_hold_seconds += unit.seconds;
            }
            plan.bridge_start_seconds = unit.start;
            plan.bridge_seconds = unit.seconds;
            plan.bridge_repeats = 1;
            plan.reason = format!(
                "{}; bridge: {} bars ({:.1}s) from {:.1}s, replayed once",
                plan.reason, unit.bars, unit.seconds, unit.start
            );
        }
        Err(why) => {
            plan.reason = format!("{}; bridge: none ({why})", plan.reason);
        }
    }
}
