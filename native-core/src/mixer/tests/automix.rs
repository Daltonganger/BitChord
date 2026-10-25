//! Which plan wins while arming, and mixing on a manual skip.

use super::*;

#[test]
fn automix_winner_classifies_safety_and_analyzed_plans() {
    assert_eq!(
        automix_plan_winner(&TransitionPlan {
            fade_seconds: 4.0,
            reason: "safety:".into(),
            ..TransitionPlan::default()
        }),
        Some("safety")
    );
    assert_eq!(
        automix_plan_winner(&TransitionPlan {
            fade_seconds: 2.0,
            reason: "tempo fit".into(),
            ..TransitionPlan::default()
        }),
        Some("analyzed")
    );
    assert_eq!(automix_plan_winner(&TransitionPlan::default()), None);
    assert_eq!(
        automix_plan_winner(&TransitionPlan {
            style: TransitionStyle::EchoOut,
            fade_seconds: 1.0,
            reason: "skip-mix:".into(),
            ..TransitionPlan::default()
        }),
        None
    );
}

#[test]
fn analyzed_plan_replaces_safety_plan_only_while_arming() {
    let dir = std::env::temp_dir().join(format!("bitchord-am4-upgrade-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let outgoing = dir.join("out.wav");
    let incoming = dir.join("in.wav");
    test_wav(&outgoing, 12.0, 440.0);
    test_wav(&incoming, 12.0, 660.0);

    let mut arming = armed_test_state(&outgoing, &incoming, Phase::Arming);
    arming.upgrade_armed_plan(analyzed_upgrade(&incoming));
    let retained = &arming.transition.as_ref().unwrap().plan;
    assert_eq!(automix_plan_winner(retained), Some("analyzed"));
    assert_eq!(retained.style, TransitionStyle::DjBlend);
    assert_eq!(retained.cue_seconds, 1.0);

    let mut fading = armed_test_state(&outgoing, &incoming, Phase::Fading);
    fading.upgrade_armed_plan(analyzed_upgrade(&incoming));
    let retained = &fading.transition.as_ref().unwrap().plan;
    assert_eq!(automix_plan_winner(retained), Some("safety"));
    assert_eq!(retained.style, TransitionStyle::EqualPower);
}

#[test]
fn skip_mix_starts_a_two_beat_echo_when_an_armed_grid_exists() {
    let dir = std::env::temp_dir().join(format!("bitchord-am5-skip-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let outgoing = dir.join("out.wav");
    let incoming = dir.join("in.wav");
    test_wav(&outgoing, 12.0, 440.0);
    test_wav(&incoming, 12.0, 660.0);

    let mut state = armed_test_state(&outgoing, &incoming, Phase::Arming);
    state.transition.as_mut().unwrap().plan.beat_seconds = 0.5;
    assert!(state.begin_skip_mix().is_some());
    state.consider_start_fade();

    let transition = state.transition.as_ref().unwrap();
    assert!(transition.phase == Phase::Fading);
    assert_eq!(transition.plan.style, TransitionStyle::EchoOut);
    assert_eq!(transition.fade_frames, 44_100);
    assert_eq!(
        transition.fx.as_ref().unwrap().window_frames(),
        transition.fade_frames
    );
    assert_eq!(transition.plan.reason, "skip-mix:");
}

#[test]
fn skip_mix_without_a_grid_keeps_the_existing_cut_fallback() {
    let dir = std::env::temp_dir().join(format!("bitchord-am5-no-grid-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let outgoing = dir.join("out.wav");
    let incoming = dir.join("in.wav");
    test_wav(&outgoing, 12.0, 440.0);
    test_wav(&incoming, 12.0, 660.0);

    let mut state = armed_test_state(&outgoing, &incoming, Phase::Arming);
    assert!(state.begin_skip_mix().is_none());
    state.promote_incoming();
    assert_eq!(state.current.as_ref().unwrap().info.title, "in");
    assert!(state.transition.is_none());
}

#[test]
fn throttled_intro_fixture_keeps_analyzed_wins_at_or_above_ninety_percent() {
    const DOWNLOAD_RATE_KIB_PER_SECOND: u64 = 64;
    const ANALYSIS_TIME_MS: u64 = 350;
    const FADE_START_MS: u64 = 10_000;
    const INTRO_KIB: [u64; 10] = [16, 64, 128, 256, 320, 384, 448, 512, 576, 1_024];

    // Fixed download rate and analysis time form a deterministic throttled
    // arrival fixture. Nine plans reach the mixer during Arming; the tenth
    // finishes analysis after the safety fade has started.
    let analyzed_wins = INTRO_KIB
        .into_iter()
        .filter(|intro_kib| {
            let download_ms = (*intro_kib * 1_000 + DOWNLOAD_RATE_KIB_PER_SECOND - 1)
                / DOWNLOAD_RATE_KIB_PER_SECOND;
            let analysis_ready_ms = download_ms + ANALYSIS_TIME_MS;
            let phase = if analysis_ready_ms < FADE_START_MS {
                Phase::Arming
            } else {
                Phase::Fading
            };
            accepts_analyzed_plan(phase, false)
        })
        .count();
    assert_eq!(analyzed_wins, 9);
    assert!(
        analyzed_wins * 100 >= INTRO_KIB.len() * 90,
        "only {analyzed_wins}/{} throttled intros kept the analyzed plan at {DOWNLOAD_RATE_KIB_PER_SECOND} KiB/s with {ANALYSIS_TIME_MS} ms analysis",
        INTRO_KIB.len()
    );
}
