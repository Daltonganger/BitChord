//! The mix-out anchor, bass swaps, and when a pair gets only a plain fade.

use super::*;

#[test]
fn bass_swap_fraction_defaults_and_stays_bounded() {
    let out = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
    let inc = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
    // Non-positive overlap returns the fixed default.
    assert!((bass_swap_fraction_for(&out, &inc, 0.0, 0.5, 0.5, 0.5, 0.0, 16) - 0.7).abs() < 1e-9);
    // A normal overlap stays bounded even with no low-energy structure.
    let fraction = bass_swap_fraction_for(&out, &inc, 0.0, 0.5, 0.5, 0.5, 6.0, 16);
    assert!((0.0..=1.0).contains(&fraction), "fraction {fraction}");
}

#[test]
fn blocked_text_matches_word_boundaries_only() {
    assert!(blocked_text("The Daily Podcast Episode 12"));
    assert!(blocked_text("Live at the Apollo (Concert)"));
    assert!(blocked_text("an audiobook performance"));
    // "alive" and "lively" are not "live"; "concerted" is not "concert".
    assert!(!blocked_text("Stay Alive"));
    assert!(!blocked_text("Lively Discussion"));
    assert!(!blocked_text("A Concerted Effort"));
    assert!(!blocked_text("Ordinary Song Title"));
}

#[test]
fn interior_energy_cliff_wins_the_mix_out_anchor() {
    let mut out = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
    out.content_end = 180.0;
    out.mix_out_candidates.push(audio_analysis::MixCuePoint {
        time: 175.0,
        score: 0.95,
        kind: "energy_cliff".into(),
    });
    // The cliff is 5 s before content end (inside the 12 s budget) and ranks
    // above the implicit content_end, so it is the anchor — and it is
    // interior, which is what gates the gapless path off.
    let (anchor, _) = resolve_mix_out_anchor(&out);
    assert!((anchor - 175.0).abs() < 1e-9, "anchor {anchor}");
    assert!(anchor < 180.0 - 1.0);
}

#[test]
fn mix_out_anchor_falls_back_to_content_end() {
    let mut out = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
    out.content_end = 180.0;
    // No mix-out candidates and no energy curve → content_end is the anchor
    // with no discarded-music charge.
    let (time, discarded) = resolve_mix_out_anchor(&out);
    assert!((time - 180.0).abs() < 1e-9, "time {time}");
    assert_eq!(discarded, 0.0);
}

#[test]
fn short_track_gets_plain_crossfade_despite_strong_beat_evidence() {
    // Upstream MIN_SMART_DURATION_SECONDS=45: a sub-45s track must never get
    // a smart transition, even when its beat grid would otherwise qualify.
    let mut short = analysis(0.5, 30.0, vec![0.5, 1.0, 1.5, 2.0]);
    short.bpm = 120.0;
    short.beat_confidence = 0.8;
    let mut long = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
    long.bpm = 120.0;
    long.beat_confidence = 0.8;

    let plan = plan_from(&long, &short, 8.0, true);
    assert_eq!(plan.style, TransitionStyle::EqualPower);
    assert!(!plan.bass_swap);
}

#[test]
fn fade_is_capped_by_incoming_duration() {
    // A 30 s incoming track must not fade longer than 40 % of itself.
    let mut short = analysis(0.5, 30.0, vec![0.5, 1.0, 1.5, 2.0]);
    short.bpm = 90.0;
    short.beat_confidence = 0.8;
    let mut long = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
    long.bpm = 90.0;
    long.beat_confidence = 0.8;
    // Without the cap, 16 beats at 90 BPM is ~10.7 s.
    let plan = plan_from(&long, &short, 8.0, true);
    assert!(
        plan.fade_seconds <= 30.0 * 0.4 + 1e-6,
        "fade {}",
        plan.fade_seconds
    );
}
