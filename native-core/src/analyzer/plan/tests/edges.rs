//! Final chords, cold endings and fade-outs.

use super::*;

#[test]
fn a_final_chord_is_struck_at_unity_after_a_held_phrase() {
    let mut out = analysis(0.0, 60.0, vec![0.0, 2.0, 4.0]);
    out.content_end = 60.0;
    // A quiet break, then the final chord at 57 s.
    with_energy(&mut out, 60.0, |time| if time < 57.0 { 0.08 } else { 0.8 });
    let mut inc = analysis(0.0, 40.0, vec![0.0, 2.0, 4.0]);
    with_energy(&mut inc, 40.0, |_| 0.8);
    let mut plan = edge_plan(60.0, 8.0, 0.0);
    respect_music_edges(&out, &inc, &mut plan);
    let chord = final_chord_attack(&out).expect("the fixture ends on a chord");
    let start = plan.transition_end_seconds - plan.fade_seconds;
    // The blend is a phrase plus the decay, and the outgoing is still at
    // unity when the chord is struck, not already falling.
    assert!(
        plan.fade_seconds >= 4.0,
        "fade {:.2}s is under the phrase",
        plan.fade_seconds
    );
    assert!((plan.outgoing_hold_seconds - 4.0).abs() < 1e-6);
    assert!((start + plan.outgoing_hold_seconds - chord).abs() < 1e-6);
    assert!((plan.transition_end_seconds - 60.0).abs() < 1e-6);
    record_transition_edges(&out, &inc, &plan);
    let edges = last_transition_edges();
    assert!(edges.cold_ending, "{edges:?}");
    assert!(!edges.clipped_final_chord, "{edges:?}");
}

/// Pair 5 of the real corpus, pinned as a shape: 143 BPM (a 3.36 s phrase)
/// whose final chord is 1.5 s before the content end. E used to leave a
/// 1.5 s blend here; E-2 keeps the phrase in the overlap and holds the
/// outgoing until the chord instead.
#[test]
fn pair_five_keeps_its_phrase_and_holds_the_final_chord() {
    let mut out = analysis(0.0, 215.0, vec![0.0, 2.0, 4.0]);
    out.bpm = 143.0;
    out.beat_interval = 0.42;
    out.content_end = 215.0;
    with_energy(
        &mut out,
        215.0,
        |time| if time <= 213.5 { 0.8 } else { 0.1 },
    );
    let mut inc = analysis(0.0, 200.0, vec![0.0, 2.0, 4.0]);
    with_energy(&mut inc, 200.0, |_| 0.8);
    let mut plan = edge_plan(215.0, 3.36, 0.0);
    respect_music_edges(&out, &inc, &mut plan);
    let start = plan.transition_end_seconds - plan.fade_seconds;
    assert!(
        plan.fade_seconds >= 3.36 - 1e-6,
        "pair 5's overlap fell to {:.2}s",
        plan.fade_seconds
    );
    assert!(
        (plan.fade_seconds - 4.86).abs() < 1e-6,
        "{:.2}s",
        plan.fade_seconds
    );
    assert!((plan.outgoing_hold_seconds - 3.36).abs() < 1e-6);
    assert!((start + plan.outgoing_hold_seconds - 213.5).abs() < 1e-6);
    record_transition_edges(&out, &inc, &plan);
    let edges = last_transition_edges();
    assert!(edges.cold_ending, "{edges:?}");
    assert!(!edges.clipped_final_chord, "{edges:?}");
}

#[test]
fn a_cold_ending_keeps_its_effect_inside_the_decay() {
    let mut out = analysis(0.0, 215.0, vec![0.0, 2.0, 4.0]);
    out.bpm = 143.0;
    out.beat_interval = 0.42;
    out.content_end = 215.0;
    with_energy(
        &mut out,
        215.0,
        |time| if time <= 213.5 { 0.8 } else { 0.1 },
    );
    let mut inc = analysis(0.0, 200.0, vec![0.0, 2.0, 4.0]);
    with_energy(&mut inc, 200.0, |_| 0.8);
    let mut plan = edge_plan(215.0, 6.0, 0.0);
    plan.style = TransitionStyle::EchoOut;
    plan.effect_seconds = 3.0;
    plan.effect_start_seconds = 209.0;
    respect_music_edges(&out, &inc, &mut plan);
    let chord = final_chord_attack(&out).expect("the fixture ends on a chord");
    // The effect dresses the ending, not the phrase that leads into it.
    assert!(
        (plan.effect_seconds - 1.5).abs() < 1e-6,
        "{:.2}s",
        plan.effect_seconds
    );
    assert!(plan.effect_start_seconds >= chord - 1e-6);
    assert!(
        (plan.effect_start_seconds + plan.effect_seconds - 215.0).abs() < 1e-3,
        "the effect no longer ends with the record"
    );
}

#[test]
fn a_fade_out_is_not_treated_as_a_final_chord() {
    let mut out = analysis(0.0, 60.0, vec![0.0, 2.0, 4.0]);
    out.content_end = 60.0;
    with_energy(&mut out, 60.0, |time| {
        if time < 50.0 {
            0.8
        } else {
            0.8 * (1.0 - (time - 50.0) / 10.0).max(0.02)
        }
    });
    assert!(final_chord_attack(&out).is_none());
}

#[test]
fn a_cold_ending_ringing_past_the_plan_end_is_kept() {
    let mut out = analysis(0.0, 60.0, vec![0.0, 2.0, 4.0]);
    out.content_end = 50.0;
    with_energy(&mut out, 50.0, |time| if time < 44.0 { 0.08 } else { 0.8 });
    let mut inc = analysis(0.0, 40.0, vec![0.0, 2.0, 4.0]);
    with_energy(&mut inc, 40.0, |_| 0.8);
    let mut plan = edge_plan(49.0, 6.0, 0.0);
    respect_music_edges(&out, &inc, &mut plan);
    assert!((plan.transition_end_seconds - 50.0).abs() < 1e-6);
    let chord = final_chord_attack(&out).expect("the fixture ends on a chord");
    let start = plan.transition_end_seconds - plan.fade_seconds;
    assert!(plan.fade_seconds >= 4.0);
    assert!((start + plan.outgoing_hold_seconds - chord).abs() < 1e-6);
}
