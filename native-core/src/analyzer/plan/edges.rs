//! Music edges: tempo agreement, playback rate, content end, final chords and
//! first vocal onsets, and keeping a blend inside them.

use super::analysis::{Analysis, Tier};
use super::choose::energy_between;
use super::cue::{beat_seconds_of, incoming_entry};
use super::technique::loud_reference;
use super::{
    AUTO_TRANSITION_MAX_SECONDS, MAX_BPM, MAX_STRETCH_DEVIATION, MIN_BEATMATCH_CONFIDENCE, MIN_BPM,
    MIN_DJ_CONFIDENCE, NEGLIGIBLE_STRETCH, VOCAL_ACTIVE_THRESHOLD,
};
use crate::mixer::{TransitionPlan, TransitionStyle};

pub(super) fn assess(outgoing: &Analysis, incoming: &Analysis) -> Tier {
    if !(MIN_BPM..=MAX_BPM).contains(&outgoing.bpm) || !(MIN_BPM..=MAX_BPM).contains(&incoming.bpm)
    {
        return Tier::Plain;
    }
    if outgoing.beat_confidence < MIN_DJ_CONFIDENCE && incoming.beat_confidence < MIN_DJ_CONFIDENCE
    {
        return Tier::Plain;
    }
    let stretch = outgoing.bpm / align_tempo_octave(outgoing.bpm, incoming.bpm).max(1e-6);
    let far = (stretch - 1.0).abs() > MAX_STRETCH_DEVIATION;
    let weak = outgoing.beat_confidence < MIN_BEATMATCH_CONFIDENCE
        || incoming.beat_confidence < MIN_BEATMATCH_CONFIDENCE;
    if far || weak {
        Tier::DjAssisted
    } else {
        Tier::Beatmatched
    }
}

pub(super) fn align_tempo_octave(outgoing_bpm: f64, incoming_bpm: f64) -> f64 {
    if outgoing_bpm <= 0.0 || incoming_bpm <= 0.0 {
        return incoming_bpm;
    }
    let mut aligned = incoming_bpm;
    while aligned / outgoing_bpm > 1.5 {
        aligned /= 2.0;
    }
    while aligned / outgoing_bpm < 0.67 {
        aligned *= 2.0;
    }
    aligned
}

/// Rate that brings the incoming tempo onto the outgoing one, or `None` when
/// the stretch would be audible. Octave-aligned first.
pub(super) fn transparent_playback_rate(
    outgoing_bpm: f64,
    incoming_bpm: f64,
    max_deviation: f64,
) -> Option<f64> {
    if outgoing_bpm <= 0.0 || incoming_bpm <= 0.0 || max_deviation < 0.0 {
        return None;
    }
    let aligned = align_tempo_octave(outgoing_bpm, incoming_bpm);
    let rate = outgoing_bpm / aligned;
    if (rate - 1.0).abs() < NEGLIGIBLE_STRETCH {
        Some(1.0)
    } else if (rate - 1.0).abs() <= max_deviation {
        Some(((rate * 10_000.0).round()) / 10_000.0)
    } else {
        None
    }
}

/// Mixxx-style fade cap: min(outro length, intro length) when both sections
/// look real. `None` means "no useful section lengths — leave the beat math".
pub(super) fn section_fade_cap(outgoing: &Analysis, incoming: &Analysis) -> Option<f64> {
    let content_end = if outgoing.content_end > 0.0 {
        outgoing.content_end
    } else {
        outgoing.duration
    };
    let outro_len = if outgoing.outro_start > 0.0 && content_end > outgoing.outro_start + 1.0 {
        content_end - outgoing.outro_start
    } else {
        return None;
    };
    let audible = if incoming.audible_start_time.is_finite() && incoming.audible_start_time >= 0.0 {
        incoming.audible_start_time
    } else {
        0.0
    };
    let intro_end = if incoming.mix_in_time > audible + 1.0 {
        incoming.mix_in_time
    } else {
        return None;
    };
    let intro_len = intro_end - audible;
    if outro_len < 2.0 || intro_len < 2.0 {
        return None;
    }
    Some(outro_len.min(intro_len))
}

pub(super) fn normalized_tempo_ratio(current: f64, next: f64) -> f64 {
    if current <= 0.0 || next <= 0.0 {
        return 1.0;
    }
    let mut ratio = next / current;
    while ratio > 1.5 {
        ratio /= 2.0;
    }
    while ratio < 0.67 {
        ratio *= 2.0;
    }
    ratio
}

/// The tempo relationship a plan was decided from, in words a log can carry.
///
/// A beat tracker reads an octave out often enough that the planner already
/// compares tempos folded — 198 against 99 is the same beat at double time,
/// not a clash — but the reason string printed the raw numbers, so the log read
/// like one (`tempo 198→99 BPM, clash`). The fold is printed with the gap it
/// produced, which is the number [`effect_earned`] actually decided on.
pub(super) fn tempo_summary(out_bpm: f64, in_bpm: f64) -> String {
    if out_bpm <= 0.0 || in_bpm <= 0.0 {
        return "tempo unknown".into();
    }
    let ratio = normalized_tempo_ratio(out_bpm, in_bpm);
    // `normalized_tempo_ratio` folds `in/out` by a power of two, so this is the
    // octave multiplier it applied to the incoming tempo.
    let fold = ratio * out_bpm / in_bpm;
    let mut summary = format!("tempo {out_bpm:.0}→{in_bpm:.0} BPM");
    if (fold - 1.0).abs() > 0.01 {
        summary.push_str(&format!(" ({fold}× to align)"));
    }
    summary.push_str(&format!(", gap {:.0}%", (1.0 - ratio).abs() * 100.0));
    summary
}

/// Where a record's audible content ends (trailing silence removed).
pub(super) fn content_end_of(analysis: &Analysis) -> f64 {
    if analysis.content_end > 0.0 {
        analysis.content_end
    } else {
        analysis.duration
    }
}

/// The last point the record is still at its loud level: the final chord's
/// attack, or the last beat before the fade begins, depending on the ending.
pub(super) fn last_full_energy(analysis: &Analysis) -> Option<f64> {
    let reference = loud_reference(analysis)?;
    let end = content_end_of(analysis);
    analysis
        .energy_curve
        .iter()
        .filter(|point| point.time.is_finite() && point.time <= end + 1e-3)
        .filter(|point| point.energy >= reference * 0.707)
        .map(|point| point.time)
        .next_back()
}

/// The attack of a final chord, if the record ends on one rather than fading.
///
/// Two shapes qualify: an abrupt end (the last loud point is within two
/// seconds of the content end), and a final chord whose attack is a clear rise
/// in the last twelve seconds. A fade-out is monotone and has no such rise, so
/// it is left to the normal blend. The heuristic is deliberately conservative:
/// it is better to miss a chord (the blend is merely longer) than to move every
/// ending.
pub(super) fn final_chord_attack(analysis: &Analysis) -> Option<f64> {
    let reference = loud_reference(analysis)?;
    let end = content_end_of(analysis);
    if end <= 0.0 {
        return None;
    }
    let last_loud = last_full_energy(analysis)?;
    if end - last_loud <= 2.0 {
        return Some(last_loud);
    }
    let earliest = (end - 12.0).max(0.0);
    let mut time = end - 0.5;
    while time > earliest {
        if let (Some(after), Some(before)) = (
            energy_between(analysis, time, time + 0.25),
            energy_between(analysis, time - 0.5, time - 0.05),
        ) {
            if after >= reference * 0.4 && after > before * 1.6 {
                return Some(time);
            }
        }
        time -= 0.1;
    }
    None
}

/// The first sustained vocal entry at or after `from`, in the mask's own
/// resolution. Two active frames are required so one model blip cannot drag a
/// cue back.
pub(super) fn first_vocal_onset(analysis: &Analysis, from: f64) -> Option<f64> {
    let mask = &analysis.vocal_activity_mask;
    if mask.is_empty() || mask.len() != analysis.energy_curve.len() {
        return None;
    }
    let mut run = 0usize;
    let mut run_start = 0.0;
    for (index, value) in mask.iter().enumerate() {
        let time = analysis.energy_curve[index].time;
        if !time.is_finite() || time < from {
            run = 0;
            continue;
        }
        if value.is_finite() && *value >= VOCAL_ACTIVE_THRESHOLD {
            if run == 0 {
                run_start = time;
            }
            run += 1;
            if run >= 2 {
                return Some(run_start);
            }
        } else {
            run = 0;
        }
    }
    None
}

/// E: respect the music at both ends of the handover.
///
/// On the outgoing side a final chord keeps its attack at unity: the mix does
/// not begin by fading it. The incoming enters a phrase before the chord and
/// rises underneath it; the outgoing stays at unity through the attack and
/// crosses over across the decay. The blend is the phrase plus the decay, so
/// the overlap is never shorter than the phrase floor even when the chord is
/// only a beat before the record's end.
///
/// On the incoming side a vocal entry or hook is never skipped: a cue past the
/// first sustained vocal is pulled back to it. A gapless album join is
/// untouched, and DropCut keeps its deliberate short swap.
pub(super) fn respect_music_edges(
    outgoing: &Analysis,
    incoming: &Analysis,
    plan: &mut TransitionPlan,
) {
    if plan.style == TransitionStyle::Gapless {
        return;
    }
    let entry = incoming_entry(incoming);
    if let Some(onset) = first_vocal_onset(incoming, entry) {
        if plan.cue_seconds > onset + 0.05 {
            plan.cue_seconds = onset.max(entry);
        }
    }
    let end = content_end_of(outgoing);
    // Only the record's actual ending is protected. A plan that mixes out at
    // an interior cliff is deliberately leaving before the ending, so moving
    // it to the file's last chord would undo the analysis the plan is based on.
    if (plan.transition_end_seconds - end).abs() > 1.5 {
        return;
    }
    let Some(chord) = final_chord_attack(outgoing) else {
        return;
    };
    if plan.transition_end_seconds < end {
        plan.transition_end_seconds = end;
    }
    let decay = (end - chord).max(0.0);
    if plan.style == TransitionStyle::DropCut {
        // A drop swap is a deliberate short cut. The chord still may not be
        // faded through, so its effect window stays inside the decay; the
        // phrase floor does not apply to a style whose identity is the cut.
        if plan.transition_end_seconds - plan.fade_seconds + 0.15 < chord {
            plan.fade_seconds = decay.max(0.1);
            plan.effect_seconds = plan.effect_seconds.min(plan.fade_seconds);
            plan.effect_start_seconds =
                (plan.transition_end_seconds - plan.effect_seconds).max(0.0);
        }
        return;
    }

    // The phrase is the planner's own length for the pair: eight beats of the
    // outgoing record, or the plan's analysed fall when there is no grid. The
    // incoming's entry moves one phrase before the chord, which is what makes
    // the overlap phrase + decay rather than the bare decay E used to leave.
    let beat = beat_seconds_of(outgoing);
    let phrase = if beat > 0.0 {
        (8.0 * beat).clamp(2.0, AUTO_TRANSITION_MAX_SECONDS)
    } else {
        plan.fade_seconds.clamp(2.0, AUTO_TRANSITION_MAX_SECONDS)
    };
    let fade = (phrase + decay).clamp(2.0, AUTO_TRANSITION_MAX_SECONDS);
    plan.fade_seconds = fade;
    plan.outgoing_hold_seconds = (fade - decay).clamp(0.0, (fade - 0.05).max(0.0));
    // The nerd stats have to describe the blend that actually plays. The
    // planner's own reason predates the hold and quotes the pre-E-2 length, so
    // a cold ending replaces it (or leads with the new shape for a style whose
    // reason carries the technique's rationale).
    let note = format!(
        "cold ending: the final chord at {chord:.1}s is struck at unity, and the incoming \
         (entered {phrase:.1}s earlier) takes over across its {decay:.1}s decay \
         ({fade:.1}s blend, {:.1}s hold)",
        plan.outgoing_hold_seconds
    );
    plan.reason =
        if plan.reason.starts_with("crossfade:") || plan.reason.starts_with("filter fade:") {
            note
        } else {
            format!("{note}; {}", plan.reason)
        };
    // The technique dresses the ending, never the held phrase: its window
    // stays inside the decay, so the chord is still at unity when it starts.
    if plan.effect_seconds > 0.0 {
        plan.effect_seconds = plan.effect_seconds.min(decay.max(0.1)).min(fade);
        plan.effect_start_seconds = (plan.transition_end_seconds - plan.effect_seconds).max(0.0);
    }
}
