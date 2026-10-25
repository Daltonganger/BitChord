//! When a pair earns a DJ technique, and the intro that brings the next record in.

use super::*;

#[test]
fn an_unmatched_pair_crosses_over_where_the_tail_falls() {
    // 8 % apart, the outgoing record's last 12 s fall away, and the
    // session's effect budget is spent: a clean equal-power crossfade
    // placed on the fall, at the record's own rate.
    let (mut out, inc) = flat_out_and_tempo_gap(200.0, 120.0, 130.0);
    with_falling_tail(&mut out, 205.0, 12.0);
    let plan = plan_from(&out, &inc, 8.0, false);
    assert_eq!(plan.style, TransitionStyle::DjFilter);
    assert!(plan.filter_sweep > 0.0, "a filter fade, not a bare one");
    assert_eq!(
        plan.playback_rate, 1.0,
        "an unmatched pair is not stretched"
    );
    assert!(plan.effect_seconds == 0.0);
    assert!(!plan.bass_swap, "no bass swap on a clean crossfade");
    assert!(
        plan.fade_seconds >= 8.0 * 60.0 / 120.0 - 1e-6,
        "at least a phrase: {:.2}s",
        plan.fade_seconds
    );
    assert!((plan.transition_end_seconds - out.content_end).abs() < 1e-6);
    assert!(plan.reason.contains("falls away"), "{}", plan.reason);
}

#[test]
fn an_unmatched_pair_with_effect_budget_gets_its_technique() {
    // The owner's call: the paced effect slot is taken on an unmatched
    // pair even where the tail falls away, and a falling tail is never
    // given a cut, which would hold the next record across a quiet gap.
    let (mut out, inc) = flat_out_and_tempo_gap(200.0, 120.0, 130.0);
    with_falling_tail(&mut out, 205.0, 12.0);
    let plan = plan_from(&out, &inc, 8.0, true);
    assert!(
        plan.style.is_effect(),
        "got {:?}: {}",
        plan.style,
        plan.reason
    );
    assert_ne!(plan.style, TransitionStyle::DropCut);
    assert!(plan.effect_seconds > 0.0);
    // Thrown where the record is still strong, not into the faded tail.
    assert!(
        plan.effect_start_seconds <= out.content_end - 3.0,
        "thrown at {:.1}s of {:.1}s",
        plan.effect_start_seconds,
        out.content_end
    );
    assert!(
        (plan.outgoing_hold_seconds - (plan.fade_seconds - plan.effect_seconds)).abs() < 1e-6,
        "the outgoing holds until its effect"
    );
}

#[test]
fn an_effect_brings_the_next_record_in_before_the_throw() {
    // "A cloud, then the next song": the incoming has to be under the
    // outgoing, plainly audible, for two bars before the technique takes
    // the outgoing out, and the blend still ends where the analysis put it.
    let (out, inc) = flat_out_and_tempo_gap(200.0, 120.0, 130.0);
    let plan = plan_from(&out, &inc, 8.0, true);
    assert!(
        plan.style.is_effect(),
        "got {:?}: {}",
        plan.style,
        plan.reason
    );
    let beat = plan.beat_seconds;
    let intro = (EFFECT_INTRO_BEATS * beat).max(EFFECT_INTRO_MIN_SECONDS);
    if plan.style != TransitionStyle::DropCut {
        assert!(
            plan.outgoing_hold_seconds
                >= intro.min(AUTO_TRANSITION_MAX_SECONDS - plan.effect_seconds) - 1e-6,
            "intro {:.2}s, wanted {intro:.2}s: {}",
            plan.outgoing_hold_seconds,
            plan.reason
        );
        assert!(plan.bed_gain_db >= EFFECT_INTRO_KEY_CLASH_DB && plan.bed_gain_db < 0.0);
        // A 130/120 pair is a grid clash: the incoming's top is held back.
        assert_eq!(plan.filter_sweep, 1.0);
    }
    assert!(plan.transition_end_seconds <= out.content_end + 1e-6);
}

#[test]
fn the_effect_memory_skips_its_own_pair_and_keeps_two_others() {
    note_technique(1, TransitionStyle::ReverbWash);
    note_technique(2, TransitionStyle::EchoOut);
    // A re-plan of pair 2 sees only pair 1 behind it.
    assert_eq!(recent_techniques(2), vec![TransitionStyle::ReverbWash]);
    // A new pair sees both, newest first.
    assert_eq!(
        recent_techniques(3),
        vec![TransitionStyle::EchoOut, TransitionStyle::ReverbWash]
    );
    // A re-plan replaces its own entry rather than adding one.
    note_technique(2, TransitionStyle::LoopRoll);
    assert_eq!(
        recent_techniques(3),
        vec![TransitionStyle::LoopRoll, TransitionStyle::ReverbWash]
    );
}

#[test]
fn a_spent_effect_budget_keeps_the_blend() {
    // The same pair that earns a technique with the budget available gets
    // the analysed blend when the session's last four transitions already
    // used one, and says why.
    let (out, inc) = flat_out_and_tempo_gap(200.0, 120.0, 130.0);
    assert!(plan_from(&out, &inc, 8.0, true).style.is_effect());
    let paced = plan_from(&out, &inc, 8.0, false);
    assert!(!paced.style.is_effect(), "got {:?}", paced.style);
    assert!(paced.reason.contains("pacing"), "{}", paced.reason);
}

#[test]
fn a_busy_handover_earns_the_technique() {
    // The same 8 % gap, but both records are still at full energy across
    // the join: there is nowhere to cross over, so the technique stands.
    let (out, inc) = flat_out_and_tempo_gap(200.0, 120.0, 130.0);
    let plan = plan_from(&out, &inc, 8.0, true);
    assert!(
        plan.style.is_effect(),
        "got {:?}: {}",
        plan.style,
        plan.reason
    );
    assert!(!plan.reason.is_empty());
}

#[test]
fn a_key_clash_earns_an_effect_even_with_a_falling_tail() {
    // Pair 10 of the corpus: C minor against A♭ major. They are a semitone
    // distance no blend wants and neither is a Camelot neighbour, so the
    // effect stands even though the outgoing tail falls away.
    let (mut out, mut inc) = flat_out_and_tempo_gap(200.0, 144.0, 121.0);
    out.key = "C minor".into();
    out.key_confidence = 0.51;
    inc.key = "A♭ major".into();
    inc.key_confidence = 0.35;
    with_falling_tail(&mut out, 205.0, 12.0);
    assert!(keys_clash(
        &out.key,
        out.key_confidence,
        &inc.key,
        inc.key_confidence
    ));
    let plan = plan_from(&out, &inc, 8.0, true);
    assert!(
        plan.style.is_effect(),
        "got {:?}: {}",
        plan.style,
        plan.reason
    );
}

/// The pairs an autoplay run is made of — different tempos, a confident
/// grid — used to fall to a fade. They now get a named technique whose
/// landing sits on a downbeat inside the outgoing record.
#[test]
fn mismatched_tempos_get_a_dj_technique_not_a_fade() {
    let grid = |bpm: f64| {
        (0..200)
            .map(|i| i as f64 * 4.0 * 60.0 / bpm)
            .collect::<Vec<_>>()
    };
    let mut out = analysis(0.0, 210.0, grid(124.0));
    out.bpm = 124.0;
    out.beat_interval = 60.0 / 124.0;
    out.beat_confidence = 0.8;
    out.content_end = 205.0;
    let mut inc = analysis(0.0, 190.0, grid(92.0));
    inc.bpm = 92.0;
    inc.beat_interval = 60.0 / 92.0;
    inc.beat_confidence = 0.8;
    let plan = plan_from(&out, &inc, 8.0, true);
    assert!(
        plan.style.is_effect(),
        "got {:?}: {}",
        plan.style,
        plan.reason
    );
    assert!(!plan.reason.is_empty());
    assert!(plan.transition_end_seconds <= 205.0 + 1e-6);
    assert!((plan.beat_seconds - 60.0 / 124.0).abs() < 1e-6);
    assert_eq!(plan.playback_rate, 1.0, "a technique never stretches");
    // A′2: the effect rides the analysed crossfade, not its own short window.
    assert!(
        plan.fade_seconds >= AUTO_MIN_SECONDS - 1e-6,
        "effect blend {:.2}s under the 4s floor",
        plan.fade_seconds
    );
    assert!(plan.effect_seconds > 0.0 && plan.effect_seconds <= plan.fade_seconds + 1e-6);
    assert!(
        (plan.effect_start_seconds + plan.effect_seconds - plan.transition_end_seconds).abs()
            < 1e-6,
        "effect window {}..{} does not end at the blend end {}",
        plan.effect_start_seconds,
        plan.effect_start_seconds + plan.effect_seconds,
        plan.transition_end_seconds
    );
}

#[test]
fn effect_earned_only_for_a_real_clash() {
    assert!(!effect_earned(false, 0.0));
    // The 4–7 % DjFilter band is still a blend.
    assert!(!effect_earned(false, 0.049));
    assert!(!effect_earned(false, 0.069));
    // A key the stretch cannot reconcile, or a gap past the filter band,
    // earns an effect; a vocal overlap never does on its own — the blend
    // ducks it instead.
    assert!(effect_earned(true, 0.0));
    assert!(effect_earned(false, 0.071));
}
