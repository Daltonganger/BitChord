//! Tempo octaves, ratios, stretch limits and section caps.

use super::*;

#[test]
fn a_tempo_is_counted_in_the_octave_that_plans() {
    // The corpus's four out-of-band readings fold to the octave the DSP
    // estimator and the Music Understanding overlay agree on.
    assert!((fold_tempo(44.0) - 88.0).abs() < 1e-9);
    assert!((fold_tempo(196.0) - 98.0).abs() < 1e-9);
    assert!((fold_tempo(198.2) - 99.1).abs() < 1e-9);
    assert!((fold_tempo(207.7) - 103.85).abs() < 1e-9);
    // Readings already in the band are left exactly alone.
    for bpm in [70.0, 88.0, 93.4, 121.3, 127.0, 143.0, 174.9] {
        assert!((fold_tempo(bpm) - bpm).abs() < 1e-9, "{bpm}");
    }
    // A nonsense tempo is passed through rather than spun to nothing.
    assert_eq!(fold_tempo(0.0), 0.0);
    assert!(fold_tempo(f64::NAN).is_nan());
}

#[test]
fn two_to_one_tempos_are_the_same_beat() {
    // A 198 BPM grid against a 99 BPM one is double time, not a clash: the
    // ratio folds by an octave and the gap that decides an effect is ~0.
    let ratio = normalized_tempo_ratio(198.2, 99.0);
    assert!((ratio - 1.0).abs() < 0.01, "ratio {ratio}");
    assert!(!effect_earned(false, (1.0 - ratio).abs()));
    assert!(!effect_earned(
        false,
        (1.0 - normalized_tempo_ratio(99.0, 198.2)).abs()
    ));
    // …and the log says so instead of printing a raw 2:1 beside "clash".
    let summary = tempo_summary(198.2, 99.0);
    assert!(summary.contains("2×"), "{summary}");
    assert!(summary.contains("gap 0%"), "{summary}");
}

#[test]
fn three_to_two_is_not_a_beatmatch() {
    // 3:2 is a polyrhythm, not the same beat: it stays a clash in both
    // directions, which is why the fold is a power of two and not a
    // harmonic ratio.
    for (out, inc) in [(120.0, 180.0), (180.0, 120.0)] {
        let gap = (1.0 - normalized_tempo_ratio(out, inc)).abs();
        assert!(gap > 0.07, "{out}→{inc} gap {gap}");
        assert!(effect_earned(false, gap));
    }
}

#[test]
fn a_near_ratio_is_reported_without_an_octave_fold() {
    let summary = tempo_summary(127.0, 134.0);
    assert!(!summary.contains("×"), "{summary}");
    assert!(summary.contains("gap 6%"), "{summary}");
}

#[test]
fn a_five_percent_gap_keeps_the_analysed_blend() {
    let grid = |bpm: f64| {
        (0..200)
            .map(|i| i as f64 * 4.0 * 60.0 / bpm)
            .collect::<Vec<_>>()
    };
    let mut out = analysis(0.0, 210.0, grid(127.0));
    out.bpm = 127.0;
    out.beat_interval = 60.0 / 127.0;
    out.beat_confidence = 0.9;
    out.content_end = 205.0;
    out.key = "C major".into();
    out.key_confidence = 0.9;
    let mut inc = analysis(0.0, 190.0, grid(121.0));
    inc.bpm = 121.0;
    inc.beat_interval = 60.0 / 121.0;
    inc.beat_confidence = 0.9;
    inc.key = "C major".into();
    inc.key_confidence = 0.9;
    let plan = plan_from(&out, &inc, 8.0, true);
    assert!(
        !plan.style.is_effect(),
        "a 5 % gap got {:?}: {}",
        plan.style,
        plan.reason
    );
    assert!(plan.fade_seconds >= AUTO_MIN_SECONDS - 1e-6);
}

/// A real beatmatch is still a blend: the techniques are for pairs that
/// cannot share a grid, not a replacement for one that can.
#[test]
fn a_true_beatmatch_still_blends() {
    let grid = (0..200).map(|i| i as f64 * 2.0).collect::<Vec<_>>();
    let mut out = analysis(0.0, 210.0, grid.clone());
    out.beat_confidence = 0.9;
    let mut inc = analysis(0.0, 210.0, grid);
    inc.beat_confidence = 0.9;
    let plan = plan_from(&out, &inc, 8.0, true);
    assert!(!plan.style.is_effect(), "got {:?}", plan.style);
}

#[test]
fn transparent_playback_rate_refuses_apple_style_warps() {
    // 120 → 90 is a 25% stretch after octave alignment fails to help.
    assert!(transparent_playback_rate(120.0, 90.0, MAX_BEATMATCH_STRETCH).is_none());
    // 120 → 122 is ~1.67% — transparent.
    let rate = transparent_playback_rate(120.0, 122.0, MAX_BEATMATCH_STRETCH).unwrap();
    assert!((rate - 120.0 / 122.0).abs() < 1e-4, "rate {rate}");
    // Octave cousins: 60 against 120 aligns to unity.
    let octave = transparent_playback_rate(120.0, 60.0, MAX_BEATMATCH_STRETCH).unwrap();
    assert!((octave - 1.0).abs() < 1e-4, "octave {octave}");
}

#[test]
fn dj_assisted_near_tempo_does_not_stretch() {
    // Same tempo neighbourhood, weak grids → DjAssisted. Must keep unity
    // rate and ride filters rather than warp (Apple's failure mode).
    let mut out = matched(0.5, 200.0);
    out.beat_confidence = 0.4; // below MIN_BEATMATCH_CONFIDENCE (0.55)
    let mut inc = matched(0.5, 200.0);
    inc.bpm = 126.0; // 5% fast — inside the old ±10% assisted band
    inc.beat_interval = 60.0 / 126.0;
    inc.beat_confidence = 0.4;
    assert_eq!(assess(&out, &inc), Tier::DjAssisted);
    let plan = plan_from(&out, &inc, 8.0, true);
    assert!(
        (plan.playback_rate - 1.0).abs() < 1e-9,
        "DjAssisted stretched to {}",
        plan.playback_rate
    );
    assert_eq!(plan.style, TransitionStyle::DjFilter);
    let smooth = crate::analyzer::smoothness::score_plan(&plan);
    assert!(!smooth.forced_stretch);
}

#[test]
fn section_fade_cap_takes_the_shorter_section() {
    let mut out = matched(0.5, 200.0);
    out.outro_start = 170.0;
    out.content_end = 180.0; // 10 s outro
    let mut inc = matched(0.5, 200.0);
    inc.audible_start_time = 0.5;
    inc.mix_in_time = 6.5; // 6 s intro
    let cap = section_fade_cap(&out, &inc).expect("both sections known");
    assert!((cap - 6.0).abs() < 1e-9, "cap {cap}");
}
