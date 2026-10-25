//! Bar-aligned bridges: when a unit is used, skipped or refused.

use super::*;

/// F: a vocal-free, phrase-aligned unit after the blend start is replayed
/// once, and the record's end moves by exactly that unit.
#[test]
fn a_vocal_free_phrase_gets_one_bridge_unit() {
    let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
    out.beat_confidence = 0.8;
    out.content_end = 100.0;
    with_energy(&mut out, 100.0, |time| {
        if time < 80.0 {
            0.8
        } else {
            0.8 * (1.0 - (time - 80.0) / 20.0).max(0.02)
        }
    });
    for value in out.vocal_activity_mask.iter_mut() {
        *value = 0.1;
    }
    let mut plan = edge_plan(100.0, 8.0, 0.0);
    plan.reason = "crossfade: test".into();
    apply_bridge(&out, &mut plan);
    assert!(
        (plan.bridge_start_seconds - 96.0).abs() < 1e-6,
        "{:?}",
        plan
    );
    assert!((plan.bridge_seconds - 4.0).abs() < 1e-6, "{:?}", plan);
    assert_eq!(plan.bridge_repeats, 1);
    assert!((plan.transition_end_seconds - 104.0).abs() < 1e-6);
    assert!((plan.fade_seconds - 12.0).abs() < 1e-6);
    assert_eq!(
        plan.outgoing_hold_seconds, 0.0,
        "an ordinary blend has no hold"
    );
    assert!(plan.reason.contains("bridge: 2 bars"), "{}", plan.reason);
}

/// A bridged fade may run past the ordinary 12 s ceiling, up to 16 s.
#[test]
fn a_bridge_may_take_the_fade_to_sixteen_seconds() {
    let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
    out.beat_confidence = 0.8;
    out.content_end = 100.0;
    with_energy(&mut out, 100.0, |time| {
        if time < 80.0 {
            0.8
        } else {
            0.8 * (1.0 - (time - 80.0) / 20.0).max(0.02)
        }
    });
    for value in out.vocal_activity_mask.iter_mut() {
        *value = 0.1;
    }
    // 10.5 s base + a 4 s unit = 14.5 s, which the old 12 s cap refused.
    let mut plan = edge_plan(100.0, 10.5, 0.0);
    plan.reason = "crossfade: test".into();
    apply_bridge(&out, &mut plan);
    assert!((plan.bridge_seconds - 4.0).abs() < 1e-6, "{}", plan.reason);
    assert!((plan.fade_seconds - 14.5).abs() < 1e-6, "{}", plan.reason);
    assert!(plan.fade_seconds <= 16.0);
}

/// A bridge on a cold ending sits before the chord and grows the hold by
/// the unit, so the delayed chord is still struck at unity.
#[test]
fn a_cold_ending_bridges_before_its_chord() {
    let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
    out.beat_confidence = 0.8;
    out.content_end = 98.0;
    with_energy(&mut out, 98.0, |time| if time <= 96.0 { 0.8 } else { 0.1 });
    for value in out.vocal_activity_mask.iter_mut() {
        *value = 0.1;
    }
    let mut inc = analysis(0.0, 80.0, vec![0.0, 2.0, 4.0]);
    with_energy(&mut inc, 80.0, |_| 0.8);
    let mut plan = edge_plan(98.0, 8.0, 0.0);
    plan.reason = "crossfade: test".into();
    respect_music_edges(&out, &inc, &mut plan);
    apply_bridge(&out, &mut plan);
    let start = plan.transition_end_seconds - plan.fade_seconds;
    let chord = final_chord_attack(&out).expect("the fixture ends on a chord");
    assert!(plan.bridge_seconds > 0.0, "{}", plan.reason);
    assert!(plan.bridge_start_seconds + plan.bridge_seconds <= chord + 1e-6);
    // The chord is delayed by the unit and the hold grew with it.
    assert!(
        (start + plan.outgoing_hold_seconds - (chord + plan.bridge_seconds)).abs() < 1e-6,
        "the delayed chord is not the start of the fall: {start} + {} vs {}",
        plan.outgoing_hold_seconds,
        chord + plan.bridge_seconds
    );
}

#[test]
fn a_sung_unit_gets_no_bridge() {
    let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
    out.beat_confidence = 0.8;
    out.content_end = 100.0;
    with_energy(&mut out, 100.0, |time| {
        if time < 80.0 {
            0.8
        } else {
            0.8 * (1.0 - (time - 80.0) / 20.0).max(0.02)
        }
    });
    for value in out.vocal_activity_mask.iter_mut() {
        *value = 0.8;
    }
    let mut plan = edge_plan(100.0, 8.0, 0.0);
    plan.reason = "crossfade: test".into();
    apply_bridge(&out, &mut plan);
    assert_eq!(plan.bridge_repeats, 0);
    assert!(
        plan.reason.contains("bridge: none (the unit has a voice)"),
        "{}",
        plan.reason
    );
}

#[test]
fn an_irregular_bar_grid_gets_no_bridge() {
    let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
    out.beat_confidence = 0.8;
    out.content_end = 100.0;
    // The model's confidence is no longer the gate: its bars are.
    for (index, bar) in out.downbeats.iter_mut().enumerate() {
        *bar += (index % 3) as f64 * 0.2;
    }
    with_energy(&mut out, 100.0, |time| {
        if time < 80.0 {
            0.8
        } else {
            0.8 * (1.0 - (time - 80.0) / 20.0).max(0.02)
        }
    });
    for value in out.vocal_activity_mask.iter_mut() {
        *value = 0.1;
    }
    let mut plan = edge_plan(100.0, 8.0, 0.0);
    plan.reason = "crossfade: test".into();
    apply_bridge(&out, &mut plan);
    assert_eq!(plan.bridge_repeats, 0);
    assert!(plan.reason.contains("bridge: none"), "{}", plan.reason);
}

#[test]
fn a_drifting_unit_is_skipped_for_an_aligned_one() {
    let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
    out.beat_confidence = 0.8;
    out.content_end = 100.0;
    // The last unit's middle downbeat is 40 ms late; an earlier unit is
    // still on the grid.
    out.downbeats[49] = 98.04;
    with_energy(&mut out, 100.0, |time| {
        if time < 80.0 {
            0.8
        } else {
            0.8 * (1.0 - (time - 80.0) / 20.0).max(0.02)
        }
    });
    for value in out.vocal_activity_mask.iter_mut() {
        *value = 0.1;
    }
    let mut plan = edge_plan(100.0, 8.0, 0.0);
    plan.reason = "crossfade: test".into();
    apply_bridge(&out, &mut plan);
    assert!(plan.bridge_seconds > 0.0, "{}", plan.reason);
    assert!(plan.bridge_start_seconds <= 92.0 + 1e-6, "{:?}", plan);
}
