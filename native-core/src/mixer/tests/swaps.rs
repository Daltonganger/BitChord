//! Source swaps and skips: alignment, stale preparations, refusals, bail flushes.

use super::*;

#[test]
fn seeking_during_a_pending_swap_rejects_it_and_keeps_the_original() {
    let (harness, low, high) = SwapHarness::new("seek-pending-swap");
    let (reply, loaded) = crossbeam_channel::bounded(1);
    harness
        .tx
        .send(Command::Load {
            request: SwapHarness::source(&low, "Original", 96),
            reply,
        })
        .unwrap();
    loaded
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    harness.tx.send(Command::Pause).unwrap();
    let (reply, swapped) = crossbeam_channel::bounded(1);
    harness
        .tx
        .send(Command::SwapSource {
            request: SwapHarness::source(&high, "Candidate", 320),
            crossfade_seconds: 0.55,
            reply,
        })
        .unwrap();
    swapped
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert_eq!(harness.nerd.lock().unwrap().swap_accepted, None);
    harness.tx.send(Command::Seek { seconds: 2.0 }).unwrap();
    // A source-gated command proves the seek retained the original voice.
    harness
        .tx
        .send(Command::ForSource {
            source: low.display().to_string(),
            command: Box::new(Command::QueueNext {
                request: SwapHarness::source(&high, "Next", 320),
            }),
        })
        .unwrap();
    let (reply, pending) = crossbeam_channel::bounded(1);
    harness.tx.send(Command::PendingTrack { reply }).unwrap();
    assert_eq!(
        pending
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .title,
        "Next"
    );
    let snapshot = harness.nerd.lock().unwrap().clone();
    assert_eq!(snapshot.swap_target, high.display().to_string());
    assert_eq!(snapshot.swap_accepted, Some(false));
    harness.finish();
}

#[test]
fn stop_or_next_during_a_pending_swap_reports_rejection() {
    for stop in [true, false] {
        let (harness, low, high) = SwapHarness::new(if stop {
            "stop-pending-swap"
        } else {
            "next-pending-swap"
        });
        let (reply, loaded) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::Load {
                request: SwapHarness::source(&low, "Original", 96),
                reply,
            })
            .unwrap();
        loaded
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        harness.tx.send(Command::Pause).unwrap();
        let (reply, swapped) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::SwapSource {
                request: SwapHarness::source(&high, "Candidate", 320),
                crossfade_seconds: 0.55,
                reply,
            })
            .unwrap();
        swapped
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        if stop {
            harness.tx.send(Command::Stop).unwrap();
        } else {
            let (reply, skipped) = crossbeam_channel::bounded(1);
            harness
                .tx
                .send(Command::SkipToPending {
                    skip_mix_enabled: false,
                    reply,
                })
                .unwrap();
            assert!(skipped
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .is_err());
        }
        let (reply, barrier) = crossbeam_channel::bounded(1);
        harness.tx.send(Command::PendingTrack { reply }).unwrap();
        assert!(barrier
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_none());
        assert_eq!(harness.nerd.lock().unwrap().swap_accepted, Some(false));
        harness.finish();
    }
}

/// How well a swap actually lines up.
///
/// Swapping to a byte-identical copy makes alignment directly measurable:
/// if the two sides are sample-aligned and the fade is linear, the blend
/// *is* the original signal at unity and the level cannot move. Whatever
/// deviation shows up here is comb cancellation — what a listener hears as
/// a hollow, phasey moment instead of a quality change.
#[test]
fn swap_to_an_identical_copy_reports_its_alignment() {
    let (mut harness, low, _) = SwapHarness::new("swap-level");
    let copy = low.with_file_name("low-copy.wav");
    std::fs::copy(&low, &copy).unwrap();

    harness
        .tx
        .send(Command::Load {
            request: SwapHarness::source(&low, "Low", 96),
            reply: crossbeam_channel::bounded(1).0,
        })
        .unwrap();
    drain_frames(&mut harness.consumer, &harness.buffered, 44_100);
    let before = rms_frames(&mut harness.consumer, &harness.buffered, 44_100 / 4);
    assert!(before > 0.0, "the track must be producing audio");

    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    harness
        .tx
        .send(Command::SwapSource {
            request: SwapHarness::source(&copy, "Copy", 320),
            crossfade_seconds: SWAP_CROSSFADE_MS / 1000.0,
            reply: reply_tx,
        })
        .unwrap();
    reply_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the mixer must answer a swap")
        .expect("the swap must be accepted");

    // Short windows, so a 550 ms fade cannot be averaged away by the
    // single-voice audio either side of it. Drain until the probe reports,
    // which is the moment the fade completed.
    let mut worst_db = 0.0f64;
    let mut correlation: Option<f64> = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        let window = rms_frames(&mut harness.consumer, &harness.buffered, 44_100 / 20);
        if window > 0.0 {
            let db = 20.0 * (window / before).log10();
            if db.abs() > worst_db.abs() {
                worst_db = db;
            }
        }
        correlation = harness.nerd.lock().unwrap().swap_correlation;
        if correlation.is_some() {
            break;
        }
    }
    eprintln!(
        "[swap-alignment] worst level deviation through the fade: {worst_db:.2} dB, rho={correlation:?}"
    );
    // Measured at 0.00 dB: the two sides are sample-aligned and the linear
    // fade makes the blend the original signal. 2 dB leaves room for a
    // future rate change between the two sources while still catching a
    // genuine misalignment, which reads as a deep comb notch.
    assert!(
        worst_db.abs() < 2.0,
        "the swap is misaligned by {worst_db} dB of comb"
    );
    // The probe has to be able to say "these are the same signal" — that is
    // the measurement everything else here rests on.
    let rho = correlation.expect("a completed swap must report a correlation");
    assert!(
        rho > 0.99,
        "two byte-identical copies must correlate at 1.0, got {rho}"
    );
    let snapshot = harness.nerd.lock().unwrap().clone();
    assert_eq!(snapshot.swap_target, copy.display().to_string());
    assert_eq!(snapshot.swap_accepted, Some(true));
    harness.finish();
}

#[test]
fn stale_preparation_cannot_modify_a_new_selection() {
    let (mut harness, low, high) = SwapHarness::new("stale-preparation");
    let (reply, loaded) = crossbeam_channel::bounded(1);
    harness
        .tx
        .send(Command::Load {
            request: SwapHarness::source(&high, "Selected", 320),
            reply,
        })
        .unwrap();
    loaded
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    let (reply, swapped) = crossbeam_channel::bounded(1);
    harness
        .tx
        .send(Command::ForSource {
            source: low.display().to_string(),
            command: Box::new(Command::SwapSource {
                request: SwapHarness::source(&low, "Obsolete", 96),
                crossfade_seconds: 0.1,
                reply,
            }),
        })
        .unwrap();
    assert!(swapped
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .is_err());
    harness
        .tx
        .send(Command::ForSource {
            source: low.display().to_string(),
            command: Box::new(Command::QueueNext {
                request: SwapHarness::source(&low, "Obsolete", 96),
            }),
        })
        .unwrap();
    drain_frames(&mut harness.consumer, &harness.buffered, 44_100 * 10);
    assert!(
        harness.events.handoffs.lock().unwrap().is_empty(),
        "stale queued audio must never become audible"
    );
    harness.finish();
}

/// A manual Next must not re-resolve: the upcoming track is already armed,
/// so the skip promotes it now — peek names it, the skip opens it and
/// reports the handoff, all without a catalogue round-trip.
#[test]
fn skip_promotes_queued_track_immediately() {
    let (harness, low, high) = SwapHarness::new("skip-queued");
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
    // The peek names the armed successor without touching it.
    let (reply, peeked) = crossbeam_channel::bounded(1);
    harness.tx.send(Command::PendingTrack { reply }).unwrap();
    let queued = peeked
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .expect("a queued track must be visible to the skip peek");
    assert_eq!(queued.title, "Second");
    // The skip opens it now and reports the handoff, like a load would.
    let (reply, skipped) = crossbeam_channel::bounded(1);
    harness
        .tx
        .send(Command::SkipToPending {
            skip_mix_enabled: true,
            reply,
        })
        .unwrap();
    let info = skipped
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .expect("an armed track must be skippable");
    assert_eq!(info.title, "Second");
    assert!(
        harness
            .events
            .handoffs
            .lock()
            .unwrap()
            .contains(&"Second".to_string()),
        "the skip must report the handoff the app flips its queue on"
    );
    // Nothing left armed: a second skip is refused, not invented.
    let (reply, again) = crossbeam_channel::bounded(1);
    harness
        .tx
        .send(Command::SkipToPending {
            skip_mix_enabled: false,
            reply,
        })
        .unwrap();
    assert!(again.recv_timeout(Duration::from_secs(5)).unwrap().is_err());
    harness.finish();
}

#[test]
fn skip_with_nothing_armed_is_refused() {
    let (harness, _, _) = SwapHarness::new("skip-empty");
    let (reply, peeked) = crossbeam_channel::bounded(1);
    harness.tx.send(Command::PendingTrack { reply }).unwrap();
    assert!(peeked
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .is_none());
    let (reply, skipped) = crossbeam_channel::bounded(1);
    harness
        .tx
        .send(Command::SkipToPending {
            skip_mix_enabled: false,
            reply,
        })
        .unwrap();
    assert!(
        skipped
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err(),
        "with no armed successor the caller must fall back to a full load"
    );
    harness.finish();
}

#[test]
fn source_swap_without_a_current_track_is_refused() {
    let (harness, low, _) = SwapHarness::new("swap-refuse");
    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    harness
        .tx
        .send(Command::SwapSource {
            request: SwapHarness::source(&low, "Low", 96),
            crossfade_seconds: SWAP_CROSSFADE_MS / 1000.0,
            reply: reply_tx,
        })
        .unwrap();
    let reply = reply_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the mixer must answer a swap");
    assert!(
        reply.is_err(),
        "nothing is playing, so there is nothing to swap"
    );
    harness.finish();
}

/// The regression this guards: upgrading the source used to be a `loadTrack`
/// — stop, open, seek — which put an audible hole in the song and reset the
/// playhead. A swap must instead crossfade in place, keep the timeline, and
/// never be reported as the queue moving on.
#[test]
fn source_swap_promotes_the_replacement_without_a_handoff() {
    let (mut harness, low, _) = SwapHarness::new("swap-crossfade");
    let high = low.with_file_name("high.wav");
    std::fs::copy(&low, &high).unwrap();
    harness
        .tx
        .send(Command::Load {
            request: SwapHarness::source(&low, "Low", 96),
            reply: crossbeam_channel::bounded(1).0,
        })
        .unwrap();

    drain_frames(&mut harness.consumer, &harness.buffered, 44_100);
    let _ = rms_frames(&mut harness.consumer, &harness.buffered, 44_100 / 4);
    let before = harness.position.load(Ordering::Relaxed);
    assert!(before > 0, "the track must be advancing before the swap");

    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    harness
        .tx
        .send(Command::SwapSource {
            request: SwapHarness::source(&high, "High", 320),
            crossfade_seconds: SWAP_CROSSFADE_MS / 1000.0,
            reply: reply_tx,
        })
        .unwrap();
    let reply = reply_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the mixer must answer a swap");
    assert!(reply.is_ok(), "swap must be accepted, got {reply:?}");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        let _ = rms_frames(&mut harness.consumer, &harness.buffered, 44_100 / 20);
        if harness.nerd.lock().unwrap().kbps == 320 {
            break;
        }
    }

    let kbps = harness.nerd.lock().unwrap().kbps;
    assert_eq!(
        kbps, 320,
        "the promoted source must be the one the readout names"
    );
    assert!(
        harness.events.handoffs.lock().unwrap().is_empty(),
        "a source swap is not a track change and must not fire a handoff"
    );
    let after = harness.position.load(Ordering::Relaxed);
    assert!(
        after >= before,
        "the playhead moved backwards across a swap: {before} ms -> {after} ms"
    );
    harness.finish();
}

/// When a candidate replacement is a mismatched recording or corrupt transcode
/// (Pearson correlation rho < 0.85), the source swap must be rejected:
/// the replacement voice is dropped, the original track remains playing at unity gain,
/// and the nerd readout does not promote to the new source.
#[test]
fn source_swap_with_mismatched_recording_is_rejected() {
    let (mut harness, low, _) = SwapHarness::new("swap-rejected");
    let dir = low.parent().unwrap();
    let mismatched = dir.join("mismatched.wav");
    test_wav(&mismatched, 8.0, 660.0); // 660 Hz vs 440 Hz -> rho near 0

    harness
        .tx
        .send(Command::Load {
            request: SwapHarness::source(&low, "Low", 96),
            reply: crossbeam_channel::bounded(1).0,
        })
        .unwrap();

    drain_frames(&mut harness.consumer, &harness.buffered, 44_100 * 2);

    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    harness
        .tx
        .send(Command::SwapSource {
            request: SwapHarness::source(&mismatched, "Mismatched", 320),
            crossfade_seconds: SWAP_CROSSFADE_MS / 1000.0,
            reply: reply_tx,
        })
        .unwrap();
    let reply = reply_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the mixer must answer a swap");
    assert!(reply.unwrap_err().contains("no confident PCM alignment"));
    // Preflight refuses the candidate before any of its samples enter the mix.
    drain_frames(&mut harness.consumer, &harness.buffered, 44_100 / 4);
    assert_eq!(harness.nerd.lock().unwrap().kbps, 96);
    let snapshot = harness.nerd.lock().unwrap().clone();
    assert_eq!(snapshot.swap_target, mismatched.display().to_string());
    assert_eq!(snapshot.swap_accepted, Some(false));
    harness.finish();
}

/// A route change replaces the ring. Everything between the playhead and
/// the decoder's read head is in the abandoned ring and nothing has played
/// it, so the voice has to be re-pointed at the playhead — otherwise the
/// listener loses the whole ring depth, seconds of music, the moment their
/// headphones disconnect.
#[test]
fn output_format_change_resumes_at_the_playhead() {
    let (mut harness, low, _) = SwapHarness::new("route-change");
    harness
        .tx
        .send(Command::Load {
            request: SwapHarness::source(&low, "Low", 96),
            reply: crossbeam_channel::bounded(1).0,
        })
        .unwrap();

    // Long enough that the playhead is well inside the track and the ring
    // is deep enough for a skip to be unmistakable.
    drain_frames(&mut harness.consumer, &harness.buffered, 44_100 * 2);
    let before = harness.position.load(Ordering::Relaxed);
    assert!(before > 0, "the track must be advancing");

    // Replace the output exactly as a route-change rebuild does.
    let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(48_000 * 2 * 2);
    harness
        .tx
        .send(Command::SetOutputFormat {
            rate: 48_000,
            producer,
            generation: 1,
            acknowledged: None,
        })
        .unwrap();

    // The old consumer is dead now; drain the replacement.
    drain_frames(&mut consumer, &harness.buffered, 48_000 / 2);
    let after = harness.position.load(Ordering::Relaxed);

    assert!(
        after + 500 >= before,
        "the playhead restarted instead of continuing: {before} ms -> {after} ms"
    );
    assert!(
        after < before + 1_500,
        "the playhead skipped the abandoned ring: {before} ms -> {after} ms"
    );
    harness.finish();
}

/// Skipping a track (Command::Load) must trigger a deferred bail flush
/// rather than an immediate hard cut of the ring, so the audio ramps out
/// over 120 ms without a click.
#[test]
fn skip_load_triggers_bail_flush_not_instant_flush() {
    let (mut harness, low, high) = SwapHarness::new("skip-bail");
    harness
        .tx
        .send(Command::Load {
            request: SwapHarness::source(&low, "Low", 96),
            reply: crossbeam_channel::bounded(1).0,
        })
        .unwrap();

    // Drain some frames so playback is active.
    drain_frames(&mut harness.consumer, &harness.buffered, 44_100);

    // Reset the bail_flush and flush_ring flags in harness
    harness.bail_flush.store(false, Ordering::Relaxed);
    harness.flush_ring.store(false, Ordering::Relaxed);

    // Issue a skip by loading a new track.
    harness
        .tx
        .send(Command::Load {
            request: SwapHarness::source(&high, "High", 320),
            reply: crossbeam_channel::bounded(1).0,
        })
        .unwrap();

    // Wait for mixer to process Command::Load.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !harness.bail_flush.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert!(
        harness.bail_flush.load(Ordering::Acquire),
        "Command::Load must set bail_flush for the 120 ms ramp"
    );
    assert!(
        !harness.flush_ring.load(Ordering::Acquire),
        "Command::Load must not perform an immediate hard flush_ring"
    );

    harness.finish();
}

#[test]
fn preview_preserves_the_next_samples_and_audible_timeline() {
    let (harness, low, _) = SwapHarness::new("preview-preserves-audio");
    let mut reference = open_voice(&low);
    let mut previewed = open_voice(&low);
    assert_eq!(reference.pull(512, 48_000), previewed.pull(512, 48_000));
    let position = previewed.emitted_position_seconds();
    let emitted = previewed.emitted_dev_frames;
    previewed.buffer_preview(12_000, 48_000);
    assert!((previewed.emitted_position_seconds() - position).abs() < 1e-6);
    assert_eq!(previewed.emitted_dev_frames, emitted);
    assert_eq!(
        reference.pull(13_000, 48_000),
        previewed.pull(13_000, 48_000)
    );
    harness.finish();
}

#[test]
fn swap_aligns_codec_delay_before_accepting_the_replacement() {
    for delay_frames in [-137isize, 221] {
        let (mut harness, low, high) = SwapHarness::new(&format!("codec-delay-{delay_frames}"));
        let mut original = std::fs::read(&low).unwrap();
        let mut x = 71u32;
        for frame in original[44..].chunks_exact_mut(4) {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let value = ((x as i32 / 131_072) as i16).to_le_bytes();
            frame[..2].copy_from_slice(&value);
            frame[2..].copy_from_slice(&value);
        }
        let mut delayed = original.clone();
        for (index, frame) in delayed[44..].chunks_exact_mut(4).enumerate() {
            let from = index as isize - delay_frames;
            if from >= 0 && (from as usize + 1) * 4 <= original.len() - 44 {
                frame.copy_from_slice(&original[44 + from as usize * 4..48 + from as usize * 4]);
            } else {
                frame.fill(0);
            }
        }
        std::fs::write(&low, original).unwrap();
        std::fs::write(&high, delayed).unwrap();
        let (reply, loaded) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::Load {
                request: SwapHarness::source(&low, "Original", 96),
                reply,
            })
            .unwrap();
        loaded
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        drain_frames(&mut harness.consumer, &harness.buffered, 44_100);
        let (reply, submitted) = crossbeam_channel::bounded(1);
        harness
            .tx
            .send(Command::SwapSource {
                request: SwapHarness::source(&high, "Delayed copy", 320),
                crossfade_seconds: 0.55,
                reply,
            })
            .unwrap();
        submitted
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        for _ in 0..20 {
            drain_frames(&mut harness.consumer, &harness.buffered, 44_100 / 4);
            if harness.nerd.lock().unwrap().swap_accepted.is_some() {
                break;
            }
        }
        let snapshot = harness.nerd.lock().unwrap().clone();
        assert_eq!(snapshot.swap_accepted, Some(true), "delay={delay_frames}");
        assert!(
            snapshot.swap_correlation.unwrap() > 0.99,
            "delay={delay_frames}"
        );
        assert_eq!(snapshot.kbps, 320);
        harness.finish();
    }
}
