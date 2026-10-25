//! Reconciling the model's beat grid with the PLL bars.

use super::*;

#[test]
fn a_complete_model_grid_keeps_its_bars() {
    let bars: Vec<f64> = (0..=20).map(|i| i as f64 * 2.0).collect();
    let mut out = analysis(0.0, 40.0, bars.clone());
    out.bpm = 120.0;
    out.beat_interval = 0.5;
    let grid = test_grid(
        120.0,
        bars.clone(),
        (0..=80).map(|i| i as f64 * 0.5).collect(),
    );
    let (chosen, phrase_beat, source) = reconcile_grid(&out, grid);
    assert!(source.starts_with("model"), "{source}");
    assert_eq!(phrase_beat, 0.0);
    assert_eq!(chosen.downbeats, bars);
}

#[test]
fn a_sparse_model_grid_takes_the_pll_bars() {
    let mut out = analysis(0.0, 40.0, (0..=20).map(|i| i as f64 * 2.0).collect());
    out.bpm = 120.0;
    out.beat_interval = 0.5;
    let model = test_grid(
        120.0,
        vec![0.0, 10.0, 20.0],
        (0..=80).map(|i| i as f64 * 0.5).collect(),
    );
    let (chosen, phrase_beat, source) = reconcile_grid(&out, model);
    assert!(source.starts_with("PLL"), "{source}");
    assert_eq!(chosen.downbeats.len(), 21);
    assert!((phrase_beat - 0.5).abs() < 1e-6);
    // The model's tempo and confidence survive for the tier.
    assert_eq!(chosen.bpm, 120.0);
    assert_eq!(chosen.beat_confidence, 0.8);
}

#[test]
fn an_off_phase_model_shifts_the_pll_bars_by_a_beat() {
    let mut out = analysis(0.0, 40.0, (0..=20).map(|i| i as f64 * 2.0).collect());
    out.bpm = 120.0;
    out.beat_interval = 0.5;
    let model = test_grid(
        120.0,
        vec![0.5, 10.5, 20.5],
        (0..=80).map(|i| i as f64 * 0.5 + 0.5).collect(),
    );
    let (chosen, _, source) = reconcile_grid(&out, model);
    assert!(source.contains("shifted"), "{source}");
    assert!(
        (chosen.downbeats[0] - 0.5).abs() < 1e-6,
        "{:?}",
        chosen.downbeats
    );
}

#[test]
fn a_double_time_model_keeps_its_tempo_but_not_its_half_bars() {
    let mut out = analysis(0.0, 100.0, (0..=40).map(|i| i as f64 * 2.45).collect());
    out.bpm = 98.0;
    out.beat_interval = 0.612;
    let model = test_grid(
        196.0,
        (0..=60).map(|i| i as f64 * 1.225).collect(),
        (0..=200).map(|i| i as f64 * 0.306).collect(),
    );
    let (chosen, phrase_beat, source) = reconcile_grid(&out, model);
    assert!(source.starts_with("PLL"), "{source}");
    assert_eq!(chosen.bpm, 196.0, "the tier tempo must not change");
    assert!((phrase_beat - 2.45 / 4.0).abs() < 1e-6, "{phrase_beat}");
    // The phrase is now a musical 8 beats, not half of one.
    assert!((8.0 * phrase_beat - 4.9).abs() < 0.1);
}
