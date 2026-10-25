//! Clean crossfades and long blends: phrase bars, vocal-free phrases, caps.

use super::*;

#[test]
fn a_plain_crossfade_ends_where_the_music_ends() {
    let mut out = analysis(0.5, 200.0, vec![]);
    out.bpm = 0.0;
    out.beat_interval = 0.0;
    out.content_end = 170.0;
    let inc = analysis(0.5, 200.0, vec![]);
    let plan = plan_from(&out, &inc, 8.0, true);
    // No tempo is no longer a fade: the record washes out instead, and the
    // wash still ends where the music does.
    assert_eq!(plan.style, TransitionStyle::ReverbWash, "{}", plan.reason);
    assert!(
        (plan.transition_end_seconds - 170.0).abs() < 1e-6,
        "wash ended at {}, not the content end",
        plan.transition_end_seconds
    );
    let fade = plain_fallback(8.0, &out);
    assert_eq!(fade.style, TransitionStyle::EqualPower);
    assert!((fade.transition_end_seconds - 170.0).abs() < 1e-6);
    assert_eq!(plan.bed_fraction, 0.0);
    assert_eq!(plan.dip_depth, 0.0);
}

/// A grid snap that lands on the mix-out used to leave `mix_end - start`
/// at a few dozen milliseconds, and `.max(0.5)` turned every one of those
/// into a half-second cut.
#[test]
fn a_snapped_start_cannot_shrink_the_blend_to_half_a_second() {
    let (start, fade) = realized_window(180.0, 179.95, 12.0, AUTO_MIN_SECONDS);
    assert!(
        (fade - 12.0).abs() < 1e-6,
        "a collapsed snap must restore the planned blend, got {fade}s"
    );
    assert!((start - 168.0).abs() < 1e-6, "start {start}");

    let (kept_start, kept_fade) = realized_window(180.0, 168.0, 12.0, AUTO_MIN_SECONDS);
    assert!(
        (kept_fade - 12.0).abs() < 1e-6,
        "a good snap stays, got {kept_fade}"
    );
    assert!((kept_start - 168.0).abs() < 1e-6);

    // The tail is the whole window: do not invent blend time the track
    // does not have.
    let (_, short) = realized_window(2.0, 1.9, 12.0, AUTO_MIN_SECONDS);
    assert!((short - 2.0).abs() < 1e-6, "short tail {short}");
}

#[test]
fn a_clean_beatmatched_pair_blends_for_more_than_thirty_two_beats() {
    let out = matched(0.5, 240.0);
    let inc = matched(0.5, 240.0);
    let plan = phrase_switch(&out, &inc).expect("a clean pair should phrase-switch");
    let beats = plan.fade_seconds / 0.5;
    assert!(
        beats > 32.0,
        "fade {:.2}s is only {beats:.0} beats; a clean pair should run long",
        plan.fade_seconds
    );
}

#[test]
fn a_vocal_clash_shrinks_the_long_blend() {
    let mut out = matched(0.5, 200.0);
    let mut inc = matched(0.5, 200.0);
    let curve: Vec<audio_analysis::EnergyPoint> = (0..400)
        .map(|i| audio_analysis::EnergyPoint {
            time: i as f64 * 0.5,
            energy: 1.0,
        })
        .collect();
    let mask = vec![1.0f64; curve.len()];
    out.energy_curve = curve.clone();
    inc.energy_curve = curve;
    out.vocal_activity_mask = mask.clone();
    inc.vocal_activity_mask = mask;
    out.vocal_probability = 0.9;
    inc.vocal_probability = 0.9;
    let plan = phrase_switch(&out, &inc).expect("a clash shrinks the blend, it does not refuse it");
    let beats = plan.fade_seconds / 0.5;
    assert!(
        beats <= 8.0,
        "a vocal clash left the blend at {beats:.0} beats"
    );
}

/// F-4: a clean crossfade starts on the one. The end is untouched; the
/// start moves to the nearest downbeat, and an even (phrase) bar beats a
/// nearer odd one.
#[test]
fn a_clean_crossfade_starts_on_a_phrase_bar() {
    let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
    out.content_end = 100.0;
    // Last full-energy point 95.5 -> a 4.5 s fall, start 95.5.
    with_energy(&mut out, 100.0, |time| if time <= 95.5 { 0.8 } else { 0.1 });
    out.vocal_activity_mask.fill(0.8); // Keep this fixture on the F-4 fallback path.
    let plan = clean_crossfade(&out, 100.0, 0.0).expect("a crossfade");
    let start = plan.transition_end_seconds - plan.fade_seconds;
    assert!((start - 96.0).abs() < 1e-6, "start {start}");
    assert!(
        (plan.fade_seconds - 4.0).abs() < 1e-6,
        "fade {}",
        plan.fade_seconds
    );
    assert!(plan.reason.contains("bar-aligned"), "{}", plan.reason);
}

#[test]
fn a_clean_crossfade_may_snap_to_an_odd_bar_when_that_is_all_there_is() {
    let mut out = analysis(
        0.0,
        100.0,
        vec![0.0, 20.0, 40.0, 60.0, 80.0, 85.0, 90.0, 95.0, 99.0, 103.0],
    );
    out.content_end = 100.0;
    with_energy(&mut out, 100.0, |time| if time <= 95.5 { 0.8 } else { 0.1 });
    out.vocal_activity_mask.fill(0.8); // Keep this fixture on the F-4 fallback path.
    let plan = clean_crossfade(&out, 100.0, 0.0).expect("a crossfade");
    let start = plan.transition_end_seconds - plan.fade_seconds;
    // 95 is the only downbeat inside the one-bar radius.
    assert!((start - 95.0).abs() < 1e-6, "start {start}");
    assert!((plan.fade_seconds - 5.0).abs() < 1e-6);
}

#[test]
fn a_clean_crossfade_extends_to_the_longest_vocal_free_phrase_before_the_fall() {
    let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
    out.content_end = 100.0;
    with_energy(&mut out, 100.0, |time| if time <= 95.5 { 0.8 } else { 0.1 });
    let plan = clean_crossfade(&out, 100.0, 0.0).expect("a crossfade");
    let start = plan.transition_end_seconds - plan.fade_seconds;
    assert!((start - 92.0).abs() < 1e-6, "start {start}");
    assert!(
        (plan.fade_seconds - 8.0).abs() < 1e-6,
        "fade {}",
        plan.fade_seconds
    );
    assert!(plan.reason.contains("outro phrase"), "{}", plan.reason);
}

#[test]
fn a_vocal_outro_keeps_the_existing_clean_crossfade() {
    let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
    out.content_end = 100.0;
    with_energy(&mut out, 100.0, |time| if time <= 95.5 { 0.8 } else { 0.1 });
    for index in 0..out.vocal_activity_mask.len() {
        if out.energy_curve[index].time >= 92.0 {
            out.vocal_activity_mask[index] = 0.8;
        }
    }
    let plan = clean_crossfade(&out, 100.0, 0.0).expect("fallback crossfade");
    let start = plan.transition_end_seconds - plan.fade_seconds;
    assert!((start - 96.0).abs() < 1e-6, "start {start}");
    assert!(
        (plan.fade_seconds - 4.0).abs() < 1e-6,
        "fade {}",
        plan.fade_seconds
    );
    assert!(!plan.reason.contains("outro phrase"), "{}", plan.reason);
}

#[test]
fn a_vocal_incoming_does_not_block_an_instrumental_outro_phrase() {
    let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
    out.content_end = 100.0;
    with_energy(&mut out, 100.0, |time| if time <= 95.5 { 0.8 } else { 0.1 });
    let mut incoming = analysis(0.0, 100.0, vec![]);
    with_vocal_curve(&mut incoming, 100.0, 0.9);
    let plan = clean_crossfade(&out, 100.0, incoming_entry(&incoming)).expect("outro phrase blend");
    assert!((plan.fade_seconds - 8.0).abs() < 1e-6);
    assert!(plan.reason.contains("outro phrase"));
}

#[test]
fn a_slow_clean_crossfade_is_capped_at_twelve_seconds() {
    // At 75 BPM, 16 beats would be 12.8 s. The 4.8 s grid phase makes
    // 88 s an even-bar candidate at the 12 s cap, while F-4 stays shorter.
    let mut out = analysis(0.0, 100.0, (0..=30).map(|i| 4.8 + i as f64 * 3.2).collect());
    out.bpm = 75.0;
    out.beat_interval = 0.8;
    out.content_end = 100.0;
    with_energy(&mut out, 100.0, |time| if time <= 95.5 { 0.8 } else { 0.1 });
    let plan = clean_crossfade(&out, 100.0, 0.0).expect("a crossfade");
    let start = plan.transition_end_seconds - plan.fade_seconds;
    assert!((start - 88.0).abs() < 1e-6, "start {start}");
    assert!(
        (plan.fade_seconds - 12.0).abs() < 1e-6,
        "fade {}",
        plan.fade_seconds
    );
    assert!(plan.reason.contains("outro phrase"), "{}", plan.reason);
}

#[test]
fn an_outro_extension_never_shortens_the_existing_fall() {
    let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
    out.content_end = 100.0;
    with_energy(&mut out, 100.0, |time| if time <= 85.0 { 0.8 } else { 0.1 });
    let plan = clean_crossfade(&out, 100.0, 0.0).expect("a crossfade");
    assert!(
        (plan.fade_seconds - 12.0).abs() < 1e-6,
        "fade {}",
        plan.fade_seconds
    );
    assert!(!plan.reason.contains("outro phrase"), "{}", plan.reason);
}
