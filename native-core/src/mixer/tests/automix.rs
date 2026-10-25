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
fn automix_winner_record_uses_stable_style_and_final_reason() {
    let plan = TransitionPlan {
        style: TransitionStyle::ReverbWash,
        fade_seconds: 8.0,
        reason: "low-vocal outro, bar-aligned".into(),
        ..TransitionPlan::default()
    };
    let record = automix_winner_record(&plan, Some(-0.012)).unwrap();
    assert_eq!(record.winner, "analyzed");
    assert_eq!(record.style, "reverb_wash");
    assert_eq!(record.reason, "low-vocal outro, bar-aligned");
    assert_eq!(record.snap_error_seconds, Some(-0.012));

    let safety = TransitionPlan {
        fade_seconds: 4.0,
        reason: "safety: overlap while analysis is pending".into(),
        ..TransitionPlan::default()
    };
    let safety_record = automix_winner_record(&safety, Some(0.0)).unwrap();
    assert_eq!(safety_record.winner, "safety");
    assert_eq!(safety_record.reason, "overlap while analysis is pending");

    let skip = TransitionPlan {
        style: TransitionStyle::EchoOut,
        fade_seconds: 1.0,
        reason: "skip-mix:".into(),
        ..TransitionPlan::default()
    };
    assert!(automix_winner_record(&skip, Some(0.0)).is_none());
}

#[test]
fn all_ten_transition_styles_have_unique_stable_telemetry_names() {
    let styles = [
        TransitionStyle::EqualPower,
        TransitionStyle::DjFilter,
        TransitionStyle::DjBlend,
        TransitionStyle::Gapless,
        TransitionStyle::EchoOut,
        TransitionStyle::LoopRoll,
        TransitionStyle::ReverbWash,
        TransitionStyle::DropCut,
        TransitionStyle::Texture,
        TransitionStyle::Brake,
    ];
    let names: Vec<_> = styles.iter().map(|style| style.telemetry_name()).collect();
    assert_eq!(
        names,
        [
            "equal_power",
            "dj_filter",
            "dj_blend",
            "gapless",
            "echo_out",
            "loop_roll",
            "reverb_wash",
            "drop_cut",
            "texture",
            "brake",
        ]
    );
    assert_eq!(names.iter().collect::<std::collections::HashSet<_>>().len(), 10);
}

#[test]
fn texture_and_brake_round_trip_through_the_plan_record() {
    for style in [TransitionStyle::Texture, TransitionStyle::Brake] {
        let record = crate::TransitionPlanRec::from(TransitionPlan {
            style,
            ..TransitionPlan::default()
        });
        assert_eq!(
            TransitionPlan::from(record).style,
            style,
            "{style:?} was lost through the FFI plan record"
        );
    }
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
    let winner = automix_winner_record(retained, Some(0.025)).unwrap();
    assert_eq!(winner.style, "dj_blend");
    assert_eq!(winner.reason, "beat grid and vocal windows");

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

/// A seek after the handoff is a seek in the track the listener now hears.
/// It abandons the blend, and used to leave the mixer with no current voice:
/// the seek was refused as "nothing playing" and the output ran dry.
#[test]
fn a_seek_after_the_handoff_lands_on_the_incoming_track() {
    let dir = std::env::temp_dir().join(format!("bitchord-seek-blend-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let outgoing = dir.join("out.wav");
    let incoming = dir.join("in.wav");
    test_wav(&outgoing, 12.0, 440.0);
    test_wav(&incoming, 12.0, 660.0);

    let mut state = armed_test_state(&outgoing, &incoming, Phase::Arming);
    state.start_fade();
    assert!(state.transition.as_ref().unwrap().handed_off);

    let (mut producer, _consumer) = rtrb::RingBuffer::<f32>::new(4096);
    crate::mixer::commands::handle_command(&mut state, Command::Seek { seconds: 3.0 }, &mut producer);

    let current = state.current.as_ref().expect("the seek left nothing current");
    assert_eq!(current.info.title, "in");
    assert!((current.decoder.position_seconds() - 3.0).abs() < 0.05);
    assert!(state.incoming.is_none());
    assert!(state.transition.is_none());
    assert_eq!(state.retiring.len(), 1, "the outgoing voice fades out");
    std::fs::remove_dir_all(&dir).ok();
}
