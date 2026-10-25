//! The incoming's cue: downbeats, depth bounds, grids and vocal entries.

use super::*;

#[test]
fn incoming_cue_uses_head_downbeat_not_outro() {
    let cue = incoming_cue(&analysis(
        1.2,
        200.0,
        vec![1.2, 3.0, 5.0, 180.0, 184.0, 188.0, 192.0, 196.0, 198.0],
    ));
    assert!(
        (cue - 1.2).abs() < 1e-9,
        "cue {cue} should be the first mix-in downbeat, not the outro"
    );
}

/// A mix-in is the top of the next record, not its drop.
///
/// Regression: the cue used to be the highest-ranked phase-1 candidate, and
/// that ranking put `main_drop` — 32 beats in — above `pickup`. Worse, when
/// the DSP tempo read failed, `beat_interval` was 0, which sent the drop
/// candidate to `intro_end_time`: the first moment the track got *loud*,
/// capped at 48 s. That is a mix-*out* question used as a mix-in question,
/// and it is where "the next song starts a minute in" came from.
#[test]
fn a_mix_in_never_reaches_for_the_drop() {
    // 120 BPM, pickup at 2 s, and a grid running the length of the track.
    let downbeats: Vec<f64> = (0..40).map(|i| 2.0 + i as f64 * 2.0).collect();
    let cue = incoming_cue(&analysis(2.0, 240.0, downbeats));
    assert!(
        (cue - 2.0).abs() < 1e-9,
        "a beat-aligned pickup at 2 s should cue at 2 s, got {cue}"
    );

    // And with the analysis reporting its pickup through the candidate list,
    // which is where the real value comes from.
    let with_candidate_grid: Vec<f64> = (0..40).map(|i| 2.0 + i as f64 * 2.0).collect();
    let mut with_candidate = analysis(2.0, 240.0, with_candidate_grid);
    with_candidate.audible_start_time = 2.0;
    with_candidate.mix_in_candidates = vec![audio_analysis::MixCuePoint {
        time: 2.0,
        score: 0.8,
        kind: "pickup".into(),
    }];
    assert!((incoming_cue(&with_candidate) - 2.0).abs() < 1e-9);
}

/// No usable grid means the top of the file, and never a guess.
///
/// This is the degraded path in its purest form: the analysis is empty, so
/// there is nothing to align to and nothing to say how deep a mix-in should
/// reach. A plain crossfade from 0 is right; a crossfade from 45 s is the
/// bug.
#[test]
fn a_mix_in_without_a_beat_grid_starts_at_the_top() {
    // No tempo at all.
    let mut blind = analysis(0.0, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
    blind.bpm = 0.0;
    blind.beat_interval = 0.0;
    assert_eq!(
        incoming_cue(&blind),
        0.0,
        "no tempo must not become a deep cue"
    );

    // A tempo outside the analyser's own range means the read failed, and a
    // failed read must not turn into a 32-beat cue either.
    let mut absurd = analysis(0.0, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
    absurd.bpm = 12.0;
    absurd.beat_interval = 5.0;
    assert_eq!(
        incoming_cue(&absurd),
        0.0,
        "an out-of-range tempo is not a grid"
    );

    // A tempo with no downbeats is no grid either.
    let mut no_downbeats = analysis(0.0, 200.0, vec![]);
    no_downbeats.audible_start_time = 1.0;
    assert_eq!(incoming_cue(&no_downbeats), 0.0);
}

/// The three bounds, each of which is the one that bites in its own case.
#[test]
fn a_mix_in_is_bounded_by_seconds_beats_and_the_track() {
    // Seconds: a pickup deep into the record is pulled back to eight.
    let deep: Vec<f64> = (0..40).map(|i| 30.0 + i as f64 * 0.5).collect();
    let mut slow = analysis(30.0, 240.0, deep);
    slow.audible_start_time = 30.0;
    let cue = incoming_cue(&slow);
    assert!(
        cue <= MAX_CUE_SECONDS + 1e-9,
        "cue {cue} exceeded the {MAX_CUE_SECONDS}s cap"
    );

    // Beats: 16 beats of a 40 BPM track is 24 s, so the beat cap is the one
    // that applies — and a failed read at 40 BPM must not become 32 beats.
    let slow_grid: Vec<f64> = (0..200).map(|i| 1.0 + i as f64 * 1.5).collect();
    let mut ballad = analysis(1.0, 600.0, slow_grid);
    ballad.bpm = 40.0;
    ballad.beat_interval = 1.5;
    ballad.audible_start_time = 40.0;
    let cue = incoming_cue(&ballad);
    assert!(
        cue <= MAX_CUE_BEATS * 1.5 + 1e-9,
        "cue {cue} exceeded {MAX_CUE_BEATS} beats"
    );

    // Fraction: short material gets a proportionally smaller entry, because
    // eight seconds is an eighth of a one-minute clip and nothing at all in
    // a four-minute song.
    let short: Vec<f64> = (0..200).map(|i| 0.2 + i as f64 * 0.5).collect();
    let mut brief = analysis(0.2, 60.0, short);
    brief.audible_start_time = 12.0;
    let cue = incoming_cue(&brief);
    assert!(
        cue <= 60.0 * MAX_CUE_FRACTION + 1e-9,
        "cue {cue} exceeded {MAX_CUE_FRACTION} of a one-minute track"
    );
}

/// The cue lands on a beat, because entering between beats is the one thing
/// a listener hears as "edited" rather than "mixed".
#[test]
fn a_mix_in_lands_on_the_grid() {
    let downbeats: Vec<f64> = (0..40).map(|i| 0.25 + i as f64 * 0.5).collect();
    let mut track = analysis(0.25, 200.0, downbeats.clone());
    // A pickup that falls a little after a downbeat.
    track.audible_start_time = 1.4;
    let cue = incoming_cue(&track);
    assert!(
        downbeats.iter().any(|d| (d - cue).abs() < 1e-6),
        "cue {cue} is not on the grid"
    );
    assert!(
        cue >= 1.4 && cue <= 1.4 + 0.5 + 1e-6,
        "cue {cue} should be the first downbeat at or after the 1.4 s pickup"
    );
}

#[test]
fn incoming_cue_does_not_index_into_sorted_outro_beats() {
    // The old fallback picked downbeats[8] after merging and sorting tail
    // times, which for a 3-minute track is ~duration − WINDOW.
    let cue = incoming_cue(&analysis(
        0.0,
        180.0,
        vec![
            150.0, 154.0, 158.0, 162.0, 166.0, 170.0, 174.0, 176.0, 178.0,
        ],
    ));
    assert_eq!(cue, 0.0, "outro-only grid must not become a mix-in cue");
}

#[test]
fn incoming_cue_rejects_pickup_in_the_last_ten_seconds() {
    let cue = incoming_cue(&analysis(175.0, 180.0, vec![175.0, 177.0, 179.0]));
    assert_eq!(cue, 0.0);
}

/// An eight-bar intro is a runway, so the phrase switch fires and the cue
/// stays at the top of that intro.
#[test]
fn phrase_switch_fires_on_an_eight_bar_intro_and_starts_at_the_entry() {
    let out = matched(0.5, 200.0);
    let mut inc = matched(0.5, 200.0);
    // 32 beats of intro: the drop, not the cue.
    inc.mix_in_candidates = vec![audio_analysis::MixCuePoint {
        time: 0.5 + 32.0 * 0.5,
        score: 0.95,
        kind: "main_drop".into(),
    }];
    let plan = phrase_switch(&out, &inc).expect("an eight-bar intro should phrase-switch");
    assert_eq!(plan.style, TransitionStyle::DjBlend);
    let entry = incoming_entry(&inc);
    assert!(
        plan.cue_seconds <= entry + MAX_CUE_SECONDS + 1e-6,
        "cue {} started more than {MAX_CUE_SECONDS}s past the entry {entry}",
        plan.cue_seconds
    );
    assert!(
        plan.cue_seconds < 0.5 + 32.0 * 0.5 - 1.0,
        "cue {} is the drop, not the entry",
        plan.cue_seconds
    );
    assert!(
        plan.bed_fraction > 0.0,
        "a phrase switch is a bed, not a crossfade"
    );
}

#[test]
fn a_cue_is_never_more_than_eight_seconds_past_the_entry() {
    assert!(
        (cue_for(1.0, 80.0, 8.0, 1.0) - 9.0).abs() < 1e-9,
        "80 - 8 = 72, which must clamp to entry + 8"
    );
    let out = matched(0.5, 240.0);
    let mut inc = matched(0.5, 240.0);
    inc.mix_in_candidates = vec![audio_analysis::MixCuePoint {
        time: 90.0,
        score: 0.95,
        kind: "main_drop".into(),
    }];
    let plan = plan_from(&out, &inc, 8.0, true);
    let entry = incoming_entry(&inc);
    assert!(
        plan.cue_seconds <= entry + MAX_CUE_SECONDS + 1e-6,
        "cue {} exceeded entry {entry} + {MAX_CUE_SECONDS}s",
        plan.cue_seconds
    );
}

#[test]
fn a_cue_past_a_vocal_entry_is_pulled_back() {
    let mut out = analysis(0.0, 60.0, vec![0.0, 2.0, 4.0]);
    with_energy(&mut out, 60.0, |_| 0.8);
    let mut inc = analysis(0.0, 40.0, vec![0.0, 2.0, 4.0]);
    with_energy(&mut inc, 40.0, |_| 0.8);
    for (index, point) in inc.energy_curve.iter().enumerate() {
        if point.time >= 4.0 {
            inc.vocal_activity_mask[index] = 0.8;
        }
    }
    let mut plan = edge_plan(60.0, 8.0, 6.0);
    respect_music_edges(&out, &inc, &mut plan);
    assert!(
        (plan.cue_seconds - 4.0).abs() < 0.05,
        "cue {} did not move to the entry",
        plan.cue_seconds
    );
}
