//! Playback regions a voice skips, and the hold after a song ends.

use super::*;

#[test]
fn a_voice_skips_regions_once_and_releases_its_file_reader() {
    let path =
        std::env::temp_dir().join(format!("bitchord-voice-regions-{}.wav", std::process::id()));
    test_wav(&path, 3.0, 440.0);
    let source = path.to_string_lossy().to_string();
    crate::playback_regions::put(
        source.clone(),
        crate::PlaybackRegions {
            excluded: vec![crate::PlaybackInterval {
                start_seconds: 1.0,
                end_seconds: 2.0,
            }],
            ..Default::default()
        },
    );
    let request = TrackSource {
        source: source.clone(),
        title: "Fixture".into(),
        artist: "".into(),
        start_seconds: 0.0,
        plan: TransitionPlan::default(),
        headers: std::collections::HashMap::new(),
        claimed_kbps: 0,
        loudness_db: None,
        duration_seconds: 3.0,
    };
    let mut voice = Voice::open(&request, false, 0.0, 44100, 1.0, false, false).unwrap();
    assert!(crate::playback_regions::source_has_playback_readers(
        source.clone()
    ));
    let mut frames = 0;
    while !voice.finished || voice.pending_dev_cursor < voice.pending_dev.len() {
        frames += voice.pull(1024, 44100).len() / 2;
    }
    assert!((frames as f64 / 44100.0 - 2.0).abs() < 0.03);
    assert!(voice.skipped_regions[0]);
    voice.decoder.seek_seconds(1.2).unwrap();
    voice.reset_processing();
    voice.skipped_regions.fill(true);
    let replay = voice.pull(1024, 44100);
    assert!(!replay.is_empty());
    assert!(voice.decoder.position_seconds() < 2.0);
    drop(voice);
    assert!(!crate::playback_regions::source_has_playback_readers(
        source
    ));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn aac_region_cuts_advance_without_reseeking_preroll() {
    let (done, result) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let fixture = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/seek-tone.m4a"
        ));
        for late in [false, true] {
            let path = std::env::temp_dir().join(format!(
                "bitchord-aac-regions-{}-{late}.m4a",
                std::process::id()
            ));
            std::fs::write(&path, fixture).unwrap();
            let source = path.to_string_lossy().to_string();
            let regions = crate::PlaybackRegions {
                // Deliberately between source samples.
                audible_start_seconds: 0.190001,
                excluded: vec![crate::PlaybackInterval {
                    start_seconds: 1.0,
                    end_seconds: 1.5,
                }],
                ..Default::default()
            };
            if !late {
                crate::playback_regions::put(source.clone(), regions.clone());
            }
            let request = TrackSource {
                source: source.clone(),
                title: "AAC fixture".into(),
                artist: "".into(),
                start_seconds: 0.0,
                plan: TransitionPlan::default(),
                headers: Default::default(),
                claimed_kbps: 0,
                loudness_db: None,
                duration_seconds: 3.0,
            };
            let mut voice = Voice::open(&request, false, 0.0, 48000, 1.0, false, true).unwrap();
            if late {
                voice.regions = regions;
                voice.skipped_regions = vec![false];
            }
            let mut frames = 0;
            let mut peak = 0.0_f32;
            for _ in 0..200 {
                let pcm = voice.pull(1024, 48000);
                frames += pcm.len() / 2;
                peak = pcm.iter().fold(peak, |a, b| a.max(b.abs()));
                if voice.finished && voice.pending_dev_cursor == voice.pending_dev.len() {
                    break;
                }
            }
            assert!(voice.finished, "AAC voice must complete");
            assert!(peak > 0.1, "AAC voice must produce audible PCM");
            assert!(
                (frames as f64 / 48000.0 - 2.309999).abs() < 0.03,
                "trim and skip must retain exact source timing: {frames} frames"
            );
            assert!(voice.skipped_regions[0]);
            drop(voice);
            std::fs::remove_file(path).unwrap();
        }
        done.send(()).unwrap();
    });
    result
        .recv_timeout(Duration::from_secs(3))
        .expect("AAC boundary seek stalled the mixer");
}

#[test]
fn after_song_hold_rejects_late_successors() {
    let (mut harness, low, high) = SwapHarness::new("sleep-hold");
    let (reply, loaded) = crossbeam_channel::bounded(1);
    harness
        .tx
        .send(Command::Load {
            request: SwapHarness::source(&low, "First", 96),
            reply,
        })
        .unwrap();
    loaded
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    harness
        .tx
        .send(Command::QueueNext {
            request: SwapHarness::source(&high, "Second", 320),
        })
        .unwrap();
    harness.tx.send(Command::HoldTrackEnd(true)).unwrap();
    harness
        .tx
        .send(Command::QueueNext {
            request: SwapHarness::source(&high, "Late", 320),
        })
        .unwrap();
    let (reply, peeked) = crossbeam_channel::bounded(1);
    harness.tx.send(Command::PendingTrack { reply }).unwrap();
    assert!(peeked
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .is_none());
    drain_frames(&mut harness.consumer, &harness.buffered, 44100 * 8);
    std::thread::sleep(Duration::from_millis(20));
    assert!(harness.events.handoffs.lock().unwrap().is_empty());
    assert_eq!(harness.events.ended.lock().unwrap().len(), 1);
    harness.finish();
}
