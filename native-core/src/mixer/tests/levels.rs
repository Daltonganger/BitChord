//! Loudness matching across a transition, and the silence classifier.

use super::*;

#[test]
fn a_transition_matches_like_with_like_and_clamps() {
    let loud = deck(Some(-14.0), None, None);
    let quiet = deck(Some(-20.0), None, None);
    // Six LU apart: a quieter incoming deck is boosted, but only by the
    // boost ceiling — turning a streamed master up is turning it into the
    // limiter.
    let db = DeckLoudness::match_gain_db(quiet, loud).unwrap();
    assert!((db - MAX_TRANSITION_BOOST_DB).abs() < 1e-9, "{db}");
    // And the other way round it attenuates by the full difference.
    let db = DeckLoudness::match_gain_db(loud, quiet).unwrap();
    assert!((db + 6.0).abs() < 1e-9, "{db}");
    // A twelve-LU cut is clamped: a transition corrects a level, it does not
    // re-master a track.
    let very_loud = deck(Some(-8.0), None, None);
    let db = DeckLoudness::match_gain_db(very_loud, quiet).unwrap();
    assert!((db + MAX_TRANSITION_MATCH_DB).abs() < 1e-9, "{db}");
    // A measured true peak bounds the boost further: a deck already peaking at
    // -0.5 dBTP has no room under the -1 dBTP ceiling.
    let hot = DeckLoudness { true_peak_dbtp: Some(-0.5), ..quiet };
    assert_eq!(DeckLoudness::match_gain_db(hot, loud), Some(0.0));
    let roomy = DeckLoudness { true_peak_dbtp: Some(-2.5), ..quiet };
    let db = DeckLoudness::match_gain_db(roomy, loud).unwrap();
    assert!((db - 1.5).abs() < 1e-9, "{db}");

    // Relative figures use the player's own convention: positive is louder,
    // because `loudness_gain` attenuates by `-db`. A deck marked 6 dB louder
    // than the other is attenuated by 6, never boosted.
    let louder = deck(None, Some(6.0), None);
    let reference = deck(None, Some(0.0), None);
    let db = DeckLoudness::match_gain_db(louder, reference).unwrap();
    assert!((db + 6.0).abs() < 1e-9, "louder incoming got {db:+}");
    let db = DeckLoudness::match_gain_db(reference, louder).unwrap();
    assert!((db - MAX_TRANSITION_BOOST_DB).abs() < 1e-9, "quieter incoming got {db:+}");

    // An absolute figure and a relative one are not comparable.
    let mixed = deck(None, Some(3.0), None);
    assert_eq!(DeckLoudness::match_gain_db(mixed, loud), None);
    // Nothing to go on at all.
    assert_eq!(
        DeckLoudness::match_gain_db(deck(None, None, None), loud),
        None
    );
}

#[test]
fn normalization_that_already_matched_the_decks_is_not_applied_twice() {
    // Two measured decks, both normalized to the target: their applied
    // gains are the exact inverse of their loudness, so after normalization
    // they are at the same level and the transition has nothing to correct.
    let loud = deck(Some(-20.0), None, Some(6.0)); // -20 LUFS + 6 dB → -14
    let quiet = deck(Some(-26.0), None, Some(12.0)); // -26 LUFS + 12 dB → -14
    assert_eq!(DeckLoudness::match_gain_db(quiet, loud), Some(0.0));

    // With normalization off the same two decks are six apart, and that is
    // the difference the transition has to make up.
    let loud_off = deck(Some(-20.0), None, None);
    let quiet_off = deck(Some(-26.0), None, None);
    let db = DeckLoudness::match_gain_db(loud_off, quiet_off).unwrap();
    assert!((db + 6.0).abs() < 1e-9, "{db}");

    // A relative deck under normalization keeps whatever the attenuation
    // could not reach: boosting is refused, so the residual is the match —
    // up to the transition's own boost ceiling.
    let quiet_relative = deck(None, Some(-6.0), None);
    let reference = deck(None, Some(0.0), Some(0.0));
    let db = DeckLoudness::match_gain_db(quiet_relative, reference).unwrap();
    assert!((db - MAX_TRANSITION_BOOST_DB).abs() < 1e-9, "{db}");
}

/// The production path, end to end: the playing deck gets its measurement
/// from `SetLoudnessMeasurement`, the queued deck is opened by the arming
/// path with only the player's relative figure — and the two still match.
///
/// No voice is configured by hand here: the tracks go in through `Load` and
/// `QueueNext`, exactly as the engine receives them, and the correction is
/// read back from the published nerd snapshot.
#[test]
fn the_queue_path_matches_a_measured_deck_to_a_relative_one() {
    // Short files: the blend is scheduled at the end of the first one, and
    // the mixer runs in real time.
    let dir = std::env::temp_dir().join(format!("bitchord-mixer-queue-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let loud = dir.join("a.wav");
    let quiet = dir.join("b.wav");
    level_wav(&loud, 3.0, 1_000.0, 0.5);
    level_wav(&quiet, 3.0, 1_500.0, 0.25);

    let events = std::sync::Arc::new(RecordedEvents::default());
    let (tx, rx) = crossbeam_channel::unbounded::<Command>();
    let (consumer_side, mut consumer) = rtrb::RingBuffer::<f32>::new(44_100 * 2 * 4);
    let buffered = std::sync::Arc::new(AtomicU64::new(0));
    let position = std::sync::Arc::new(AtomicU64::new(0));
    let duration = std::sync::Arc::new(AtomicU64::new(0));
    let shutdown = std::sync::Arc::new(AtomicBool::new(false));
    let nerd = std::sync::Arc::new(Mutex::new(NerdSnapshot::default()));

    let handle = std::thread::spawn({
        let events = events.clone();
        let buffered = buffered.clone();
        let position = position.clone();
        let duration = duration.clone();
        let shutdown = shutdown.clone();
        let nerd = nerd.clone();
        move || {
            run_mixer(
                rx,
                consumer_side,
                buffered,
                position,
                duration,
                44_100,
                events,
                shutdown,
                std::sync::Arc::new(AtomicBool::new(false)),
                std::sync::Arc::new(AtomicBool::new(false)),
                nerd,
            )
        }
    });

    let source = |path: &std::path::Path, loudness: f64| TrackSource {
        source: path.display().to_string(),
        title: path.display().to_string(),
        artist: String::new(),
        start_seconds: 0.0,
        plan: TransitionPlan::default(),
        headers: std::collections::HashMap::new(),
        claimed_kbps: 0,
        loudness_db: Some(loudness),
        duration_seconds: 3.0,
    };
    tx.send(Command::Load {
        request: source(&loud, 0.0),
        reply: crossbeam_channel::bounded(1).0,
    })
    .unwrap();
    // The current deck is the only one production measures.
    tx.send(Command::SetLoudnessMeasurement(measured(-14.0)))
        .unwrap();
    // The queued deck is opened by the arming path and has only the player's
    // relative figure.
    tx.send(Command::QueueNext {
        request: source(&quiet, 6.0),
    })
    .unwrap();

    // The mixer blocks on a full ring, so the drain never stops. Arming is
    // what this test is about: it happens as the outgoing deck approaches
    // its end, well before the fade itself. Waiting for the handoff as well
    // would make the test about the crossfade's schedule.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    // The device callback's side of the ring: every popped sample has to
    // leave the shared counter, or the mixer believes the ring is full and
    // never looks at its command channel again.
    fn drain(consumer: &mut rtrb::Consumer<f32>, buffered: &AtomicU64) {
        while consumer.pop().is_ok() {
            let current = buffered.load(Ordering::Relaxed);
            if current > 0 {
                let _ = buffered.compare_exchange(
                    current,
                    current - 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
            }
        }
    }

    let mut armed = None;
    loop {
        drain(&mut consumer, &buffered);
        armed = nerd.lock().unwrap().transition_match_db;
        if armed.is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the transition was never armed"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    // `Command::Shutdown` only stops the voices; the thread watches its own
    // flag, and the drain has to continue until it has seen it — the mixer
    // is blocked on a full ring otherwise.
    shutdown.store(true, Ordering::Release);
    let stop = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !handle.is_finished() {
        drain(&mut consumer, &buffered);
        assert!(std::time::Instant::now() < stop, "the mixer did not stop");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    handle.join().unwrap();

    assert_eq!(
        armed,
        Some(-6.0),
        "a measured outgoing deck and a relative incoming one declined to match"
    );
}

#[test]
fn the_voice_path_matches_a_six_lu_blend_and_releases() {
    // Quiet into loud: the louder incoming is cut by six.
    let (loud, quiet) = levels();
    let mut outgoing = open_voice(&quiet);
    let mut incoming = open_voice(&loud);
    outgoing.measured_loudness = Some(measured(-20.0));
    incoming.measured_loudness = Some(measured(-14.0));

    incoming.arm_transition_match(Some(&outgoing), false);
    assert!(
        (incoming.match_db + 6.0).abs() < 1e-6,
        "armed {}",
        incoming.match_db
    );
    // G: the correction is in place at arm, before the incoming is audible.
    // A ramp here is the owner's "blasts, then corrects" complaint.
    assert!(
        (incoming.smoothed_match - db_to_gain(-6.0) as f32).abs() < 1e-6,
        "the match was not in place at arm: {}",
        incoming.smoothed_match
    );

    let mixed = render_blend(&mut outgoing, &mut incoming, false);
    let (spread, peak) = level_spread(&mixed);
    assert!(spread < 1.0, "level moved {spread:.2} dB across the blend");
    assert!(peak <= 1.0, "peak {peak} clipped");
    assert!(
        incoming.smoothed_match < 0.55,
        "the cut did not hold: {}",
        incoming.smoothed_match
    );

    // Release walks the correction home instead of stepping it away.
    incoming.release_transition_match();
    assert_eq!(incoming.match_db, 0.0);
    let mut previous = incoming.smoothed_match;
    for _ in 0..120 {
        let frames = incoming.pull(1024, 48_000);
        let mut out = vec![0.0f32; frames.len()];
        incoming.mix_into(&frames, &mut out, 1.0, false);
        assert!(
            incoming.smoothed_match >= previous - 1e-6,
            "release went back down: {} < {previous}",
            incoming.smoothed_match
        );
        previous = incoming.smoothed_match;
    }
    assert!(previous > 0.7, "release stalled at {}", previous);
}

#[test]
fn the_same_blend_without_the_match_dips() {
    let (loud, quiet) = levels();
    let mut outgoing = open_voice(&loud);
    let mut incoming = open_voice(&quiet);
    outgoing.measured_loudness = Some(measured(-14.0));
    incoming.measured_loudness = Some(measured(-20.0));

    let mixed = render_blend(&mut outgoing, &mut incoming, false);
    let (spread, _) = level_spread(&mixed);
    assert!(
        spread > 1.0,
        "the unmatched blend should dip, moved {spread:.2} dB"
    );
}

#[test]
fn a_normalized_blend_is_not_corrected_twice() {
    let (loud, quiet) = levels();
    let mut outgoing = open_voice(&loud);
    let mut incoming = open_voice(&quiet);
    outgoing.normalization_mode = LoudnessMode::Track;
    incoming.normalization_mode = LoudnessMode::Track;
    outgoing.measured_loudness = Some(measured(-14.0));
    incoming.measured_loudness = Some(measured(-20.0));

    // Normalization boosts the quieter deck by exactly the difference, so
    // both decks reach the blend at the target level and the transition has
    // nothing left to correct.
    incoming.arm_transition_match(Some(&outgoing), true);
    assert_eq!(
        incoming.match_db, 0.0,
        "the same difference was corrected twice"
    );

    let mixed = render_blend(&mut outgoing, &mut incoming, true);
    let (spread, peak) = level_spread(&mixed);
    assert!(spread < 1.0, "normalized blend moved {spread:.2} dB");
    assert!(peak <= 1.0, "peak {peak} clipped");
}

#[test]
fn silence_floor_uses_device_rate_not_source_rate() {
    // The bug: 44100 device frames at 96 kHz is 0.46 s, well under the
    // 1 s floor. Dividing by a 44.1 kHz source rate treated it as 1 s
    // and ate musical pauses.
    assert!(!silence_exceeds_floor(44_100, 96_000));
    assert!(!silence_exceeds_floor(48_000, 48_000));
    assert!(silence_exceeds_floor(48_001, 48_000));
    assert!(silence_exceeds_floor(96_001, 96_000));
}

#[test]
fn silence_classifier_matches_media3_int16_threshold() {
    let just_under = 1023.0 / 32768.0;
    let just_over = 1025.0 / 32768.0;
    assert!(is_silent(&vec![just_under; 64]));
    assert!(!is_silent(&vec![just_over; 64]));
    let mut mixed = vec![0.0f32; 64];
    mixed[10] = just_over;
    assert!(!is_silent(&mixed));
    assert!(!is_silent(&[]));
}
