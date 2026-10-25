//! The rendered shape of each transition style: no holes, no bumps, flat cold ends.

use super::*;

/// F: the bridge captures one measured unit and replays it without
/// advancing the decoder, so the record continues from exactly where it
/// stopped. Both voices are the same file, so the assertion is sample
/// equality, not a resemblance.
#[test]
fn a_bridge_repeats_the_unit_and_continues_where_it_stopped() {
    let dir = std::env::temp_dir().join(format!("bitchord-bridge-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bridge.wav");
    level_wav(&path, 12.0, 220.0, 0.4);
    let mut plain = open_voice(&path);
    let mut bridged = open_voice(&path);
    let rate = 48_000u32;
    let delay = 2 * rate as usize;
    let unit = rate as usize; // one second
    bridged.set_bridge(delay as u64, unit as u64, 1);
    let plain_frames = delay + 3 * unit;
    let mut a: Vec<f32> = Vec::new();
    let mut b: Vec<f32> = Vec::new();
    while a.len() / 2 < plain_frames {
        a.extend(plain.pull(512, rate));
    }
    let total_b = plain_frames + unit;
    while b.len() / 2 < total_b {
        let want = (total_b - b.len() / 2).min(512);
        b.extend(bridged.pull(want, rate));
    }
    let frame =
        |samples: &[f32], at: usize| -> (f32, f32) { (samples[at * 2], samples[at * 2 + 1]) };
    let same = |x: &[f32], x_at: usize, y: &[f32], y_at: usize, frames: usize| -> f64 {
        let mut worst = 0.0f64;
        for i in 0..frames {
            let (a_l, a_r) = frame(x, x_at + i);
            let (b_l, b_r) = frame(y, y_at + i);
            worst = worst.max((a_l - b_l).abs() as f64);
            worst = worst.max((a_r - b_r).abs() as f64);
        }
        worst
    };
    // Before the unit, the two voices are the same performance.
    assert!(
        same(&a, 0, &b, 0, delay) < 1e-6,
        "the delay changed the audio"
    );
    // The first pass is unchanged and the unit is captured from it.
    assert!(
        same(&a, delay, &b, delay, unit) < 1e-6,
        "the first pass changed"
    );
    // The replay is the same unit, after the 10 ms equal-gain seam.
    let seam = (rate as usize / 100).min(unit / 4);
    assert!(
        same(&a, delay + seam, &b, delay + unit + seam, unit - seam) < 1e-6,
        "the replay is not the captured unit"
    );
    // And the record continues at the point it was paused, one unit later.
    assert!(
        same(&a, delay + unit, &b, delay + 2 * unit, unit) < 1e-6,
        "the record did not continue where it stopped"
    );
    // The seam itself is a crossfade, so it is near the unit's own level,
    // not a hole.
    let level = |samples: &[f32], at: usize, frames: usize| -> f64 {
        let mut sum = 0.0f64;
        for i in 0..frames {
            let (l, r) = frame(samples, at + i);
            sum += l as f64 * l as f64 + r as f64 * r as f64;
        }
        (sum / (frames * 2) as f64).sqrt()
    };
    let seam_level = level(&b, delay + unit, seam);
    let unit_level = level(&a, delay, seam);
    assert!(
        seam_level > unit_level * 0.7 && seam_level < unit_level * 1.3,
        "the seam is a hole or a bump ({seam_level} vs {unit_level})"
    );
    std::fs::remove_file(&path).ok();
}

/// G: the transition match and the incoming's normalization are targets
/// **before** the first audible frame, not ramps through the first 0.4 s.
/// The owner hears the old shape as "the next song blasts, then corrects".
#[test]
fn a_transition_match_is_settled_before_the_first_audible_frame() {
    let dir = std::env::temp_dir().join(format!("bitchord-match-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let loud = dir.join("loud.wav");
    let quiet = dir.join("quiet.wav");
    noise_wav(&loud, 0.0, 6.0, 0.5, 0x51DC_1A2B);
    noise_wav(&quiet, 0.0, 6.0, 0.5, 0x77EE_3C4D);
    let mut outgoing_source = transition_source(&loud);
    outgoing_source.loudness_db = Some(-2.0);
    let mut incoming_source = transition_source(&quiet);
    incoming_source.loudness_db = Some(-8.0);
    let outgoing =
        Voice::open(&outgoing_source, false, 0.0, 48_000, 1.0, false, false).unwrap();
    let mut incoming =
        Voice::open(&incoming_source, false, 0.0, 48_000, 1.0, false, false).unwrap();
    incoming.arm_transition_match(Some(&outgoing), false);
    assert!(
        (incoming.match_db - 6.0).abs() < 1e-6,
        "expected a +6 dB match, got {}",
        incoming.match_db
    );
    let target = db_to_gain(6.0) as f32;
    assert!(
        (incoming.smoothed_match - target).abs() < 1e-6,
        "the match is set at arm, not ramped at the fade: {} vs {target}",
        incoming.smoothed_match
    );
    let rate = 48_000usize;
    let mut levels = Vec::new();
    for window in 0..3 {
        let frames = incoming.pull(rate, rate as u32);
        let mut mixed = vec![0.0f32; frames.len()];
        incoming.mix_into(&frames, &mut mixed, 1.0, false);
        if window == 0 {
            assert!(
                (mixed[0] / frames[0] - target).abs() < 1e-4,
                "the first audible sample is not at the match: {} vs {target}",
                mixed[0] / frames[0]
            );
        }
        levels.push(rms_window(&mixed, 0, rate));
    }
    for pair in levels.windows(2) {
        let jump = 20.0 * (pair[1] / pair[0]).log10();
        assert!(
            jump.abs() < 1.0,
            "the incoming's first seconds jump by {jump:.2} dB: {levels:?}"
        );
    }
    std::fs::remove_file(&loud).ok();
    std::fs::remove_file(&quiet).ok();
}

/// G: a beat-matched DjBlend of two records with the same kick pattern is
/// the correlated case B's independent-noise harness cannot see. The bass
/// swap has to keep the aligned kicks from summing to +3 dB.
#[test]
fn a_correlated_dj_blend_does_not_bump_at_the_midpoint() {
    const RATE: usize = 48_000;
    const BEAT: f64 = 0.5;
    const FADE: f64 = 8.0;
    const CHUNK: usize = 512;
    let dir = std::env::temp_dir().join(format!("bitchord-djcorr-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let outgoing_path = dir.join("kick-a.wav");
    let incoming_path = dir.join("kick-b.wav");
    kick_wav(&outgoing_path, 12.0, 0x1234_5678, 0.45, 0.15);
    kick_wav(&incoming_path, 12.0, 0x9E37_79B9, 0.45, 0.15);
    let mut outgoing = open_voice(&outgoing_path);
    let mut incoming = open_voice(&incoming_path);
    incoming.gain = 0.0;
    incoming.applied_gain = 0.0;
    prime_voice(&mut incoming, RATE as u32);
    let plan = TransitionPlan {
        style: TransitionStyle::DjBlend,
        bass_swap: true,
        bass_swap_fraction: 0.62,
        bed_fraction: 0.4,
        bed_gain_db: -14.0,
        dip_depth: 0.2,
        dip_width: 0.08,
        fade_seconds: FADE,
        beat_seconds: BEAT,
        ..Default::default()
    };
    let fade_frames = (FADE * RATE as f64) as usize;
    let mut mixed = Vec::with_capacity(fade_frames * 2);
    let mut out_raw = Vec::with_capacity(fade_frames * 2);
    let mut in_raw = Vec::with_capacity(fade_frames * 2);
    let mut done = 0usize;
    while done < fade_frames {
        let p = done as f64 / fade_frames as f64;
        ride_filters(p, &plan, &mut outgoing.filter, &mut incoming.filter);
        let (rise, fall) = automix_gains(p, &plan, 0.0);
        let a = outgoing.pull(CHUNK, RATE as u32);
        let b = incoming.pull(CHUNK, RATE as u32);
        let mut out = vec![0.0f32; CHUNK * 2];
        outgoing.mix_into(&a, &mut out, fall, false);
        incoming.mix_into(&b, &mut out, rise, false);
        let mut a_pad = a;
        a_pad.resize(CHUNK * 2, 0.0);
        let mut b_pad = b;
        b_pad.resize(CHUNK * 2, 0.0);
        out_raw.extend_from_slice(&a_pad);
        in_raw.extend_from_slice(&b_pad);
        mixed.extend_from_slice(&out);
        done += CHUNK;
    }
    let window = 3 * RATE;
    let span = fade_frames.saturating_sub(window);
    let mut dip = 0.0f64;
    let mut rise = 0.0f64;
    let mut at = 0;
    while at <= span {
        let m = 20.0 * rms_window(&mixed, at, window).max(1e-9).log10();
        let o = 20.0 * rms_window(&out_raw, at, window).max(1e-9).log10();
        let i = 20.0 * rms_window(&in_raw, at, window).max(1e-9).log10();
        dip = dip.min(m - o.min(i));
        rise = rise.max(m - o.max(i));
        at += 4_800;
    }
    println!("DjBlend correlated: 3s dip {dip:.2} dB, rise {rise:.2} dB");
    assert!(dip > -3.0, "the correlated blend dipped {dip:.2} dB");
    assert!(
        rise < 2.0,
        "the aligned kicks lift the blend by {rise:.2} dB; the bass swap did not separate them"
    );
    std::fs::remove_file(&outgoing_path).ok();
    std::fs::remove_file(&incoming_path).ok();
}

#[test]
fn priming_a_voice_does_not_advance_its_audible_position() {
    let dir = std::env::temp_dir().join(format!("bitchord-mixer-prime-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("prime.wav");
    level_wav(&path, 2.0, 440.0, 0.2);
    let mut voice = open_voice(&path);
    // A beatmatched handoff runs the incoming slightly off unity; the
    // pre-roll must be decoded at that rate, not at the rate of a fresh
    // decoder.
    voice.base_plan_rate = 1.03;
    let before = voice.emitted_position_seconds();
    prime_voice(&mut voice, 48_000);
    assert_eq!(
        voice.emitted_dev_frames, 0,
        "pre-roll must not be audible yet"
    );
    assert!(
        (voice.emitted_position_seconds() - before).abs() < 0.03,
        "pre-roll moved the audible position from {before:.3}s to {:.3}s",
        voice.emitted_position_seconds()
    );
    let first = voice.pull(512, 48_000);
    assert!(!first.is_empty(), "the primed voice must serve its cue");
}

#[test]
fn a_planned_cue_inside_a_quiet_lead_in_starts_at_the_audible_edge() {
    let dir = std::env::temp_dir().join(format!("bitchord-mixer-cue-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cue-lead.wav");
    noise_wav(&path, 1.5, 4.0, 0.2, 0xDEAD_BEEF);
    crate::playback_regions::put(
        path.display().to_string(),
        crate::PlaybackRegions {
            audible_start_seconds: 1.5,
            audible_end_seconds: None,
            excluded: Vec::new(),
        },
    );
    let mut request = transition_source(&path);
    // The planner's cue lands inside the quiet lead-in; the audible edge
    // has to win, not the raw cue.
    request.plan.cue_seconds = 0.5;
    request.plan.playback_rate = 1.0;
    let mut voice = Voice::open(&request, false, 0.0, 48_000, 1.0, false, true).unwrap();
    let pcm = voice.pull(512, 48_000);
    let rms = (pcm.iter().map(|s| (*s as f64).powi(2)).sum::<f64>()
        / (pcm.len() / 2).max(1) as f64)
        .sqrt();
    assert!(
        rms > 0.01,
        "the cue started inside the quiet lead-in ({rms})"
    );
    assert!(
        voice.emitted_position_seconds() > 1.45,
        "the audible position is {:.2}s, not the trimmed edge",
        voice.emitted_position_seconds()
    );
    drop(voice);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn mix_into_ramps_between_chunk_targets() {
    let dir = std::env::temp_dir().join(format!("bitchord-mixer-ramp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ramp.wav");
    level_wav(&path, 1.0, 440.0, 0.2);
    let mut voice = open_voice(&path);
    // A constant input makes the output the gain curve itself.
    let source = vec![1.0f32; 512 * 2];
    let mut out = vec![0.0f32; 512 * 2];
    voice.mix_into(&source, &mut out, 0.0, false);
    let left: Vec<f32> = out.chunks_exact(2).map(|frame| frame[0]).collect();
    let step = left
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(step < 0.01, "max inter-sample gain step {step}");
    assert!(left[0] > 0.99, "first sample {}", left[0]);
    assert!(
        *left.last().unwrap() < 0.01,
        "last sample {}",
        left.last().unwrap()
    );

    // The next chunk starts where the previous one ended: no step at a
    // 512-frame boundary even though the target jumped to 1.0.
    let mut next = vec![0.0f32; 512 * 2];
    voice.mix_into(&source, &mut next, 1.0, false);
    let left: Vec<f32> = next.chunks_exact(2).map(|frame| frame[0]).collect();
    assert!(left[0] < 0.01, "chunk boundary step {}", left[0]);
    assert!(
        *left.last().unwrap() > 0.99,
        "last sample {}",
        left.last().unwrap()
    );
    let step = left
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(step < 0.01, "max inter-sample gain step {step}");
}

#[test]
fn every_style_has_no_hole_at_the_handover() {
    let dir = std::env::temp_dir().join(format!("bitchord-mixer-hole-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let outgoing = dir.join("out.wav");
    let incoming = dir.join("in.wav");
    noise_wav(&outgoing, 0.0, 12.0, 0.5, 0x1234_5678);
    // 1.5 s of quiet lead-in, long enough that the cue must trim it and
    // short enough to sit inside the arming window.
    noise_wav(&incoming, 1.5, 30.0, 0.5, 0x9E37_79B9);
    crate::playback_regions::put(
        incoming.display().to_string(),
        crate::PlaybackRegions {
            audible_start_seconds: 1.5,
            audible_end_seconds: None,
            excluded: Vec::new(),
        },
    );
    for style in [
        TransitionStyle::EchoOut,
        TransitionStyle::LoopRoll,
        TransitionStyle::ReverbWash,
        TransitionStyle::DropCut,
        TransitionStyle::Texture,
        TransitionStyle::Brake,
        TransitionStyle::EqualPower,
        TransitionStyle::DjBlend,
        TransitionStyle::DjFilter,
    ] {
        let rendered = render_transition_style(style, &outgoing, &incoming, 0.0);
        let frames = rendered.fade_frames;
        let first_in = rms_window(&rendered.incoming, 0, 480);
        assert!(
            first_in > 0.05,
            "{style:?}: the primed incoming is silent at the blend start ({first_in}) \
             — the lead-in was not trimmed or the buffer was not primed"
        );

        // B's floor: across the landing ±200 ms the summed output must
        // never sit more than 6 dB below the quieter record.
        let start = rendered.land_frame.saturating_sub(9_600);
        let end = (rendered.land_frame + 9_600).min(frames);
        let mut worst = f64::INFINITY;
        let mut worst_at = start;
        let mut hole = None;
        let mut at = start;
        while at + 480 <= end {
            let m = rms_window(&rendered.mixed, at, 480);
            let o = rms_window(&rendered.outgoing, at, 480);
            let i = rms_window(&rendered.incoming, at, 480);
            let reference = o.min(i);
            if reference > 1e-6 && m > 0.0 {
                let ratio = 20.0 * (m / reference).log10();
                if ratio < worst {
                    worst = ratio;
                    worst_at = at;
                }
            }
            // A 20 ms run below −60 dBFS is a hole, not a quiet moment.
            if hole.is_none() && at + 960 <= end && rms_window(&rendered.mixed, at, 960) < 0.001 {
                hole = Some(at);
            }
            at += 480;
        }
        let worst = if worst.is_finite() { worst } else { 0.0 };
        assert!(
            hole.is_none(),
            "{style:?}: a 20 ms hole at frame {:?} (land {})",
            hole,
            rendered.land_frame
        );
        assert!(
            worst > -6.0,
            "{style:?}: dipped to {worst:.2} dB at frame {worst_at} (land {})",
            rendered.land_frame
        );

        // Continuity of level, too: 3 s short-term RMS stays within 3 dB
        // below the quieter neighbour and 2 dB above the louder one.
        let window = 3 * 48_000usize;
        let span = frames.saturating_sub(window);
        let mut dip = 0.0f64;
        let mut rise = 0.0f64;
        let mut rise_at = 0usize;
        let mut rise_values = (0.0f64, 0.0f64, 0.0f64);
        let mut at = 0;
        while at <= span {
            let m = 20.0 * rms_window(&rendered.mixed, at, window).max(1e-9).log10();
            let o = 20.0 * rms_window(&rendered.outgoing, at, window).max(1e-9).log10();
            let i = 20.0 * rms_window(&rendered.incoming, at, window).max(1e-9).log10();
            dip = dip.min(m - o.min(i));
            if m - o.max(i) > rise {
                rise = m - o.max(i);
                rise_at = at;
                rise_values = (m, o, i);
            }
            at += 4_800;
        }
        println!(
            "{style:?}: land {}f, floor {worst:.2} dB, 3s dip {dip:.2} dB, rise {rise:.2} dB \
             at {rise_at} (m/o/i {rise_values:?})",
            rendered.land_frame
        );
        assert!(
            dip > -3.0,
            "{style:?}: the 3 s level dipped {dip:.2} dB below the quieter record"
        );
        assert!(
            rise < 2.0,
            "{style:?}: the 3 s level rose {rise:.2} dB above the louder record"
        );
    }
}

/// E-2 in numbers: a cold ending holds the outgoing through its final
/// chord and enters the next record underneath it, so the summed level does
/// not lift at the chord and then has a decay to cross over. The gate is the
/// same B gate — no dip below the quieter record, no rise above the louder
/// one — on the new hold shape.
#[test]
fn a_cold_ending_renders_flat_through_its_chord() {
    const RATE: usize = 48_000;
    let dir = std::env::temp_dir().join(format!("bitchord-mixer-cold-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let outgoing = dir.join("out.wav");
    let incoming = dir.join("in.wav");
    noise_wav(&outgoing, 0.0, 12.0, 0.5, 0x2468_ACE0);
    noise_wav(&incoming, 1.5, 30.0, 0.5, 0x1357_9BDF);
    crate::playback_regions::put(
        incoming.display().to_string(),
        crate::PlaybackRegions {
            audible_start_seconds: 1.5,
            audible_end_seconds: None,
            excluded: Vec::new(),
        },
    );
    // Pair 5's phrase inside the 8 s harness: the chord is struck at 3.36 s
    // and the remaining 4.64 s is the crossing.
    let hold = 3.36;
    let rendered = render_transition_style(TransitionStyle::EqualPower, &outgoing, &incoming, hold);
    let frames = rendered.fade_frames;
    let chord = (hold * RATE as f64) as usize;
    let at_chord = rms_window(&rendered.mixed, chord, 480);
    let out_chord = rms_window(&rendered.outgoing, chord, 480).max(1e-9);
    let chord_db = 20.0 * (at_chord / out_chord).log10();
    assert!(
        (-1.0..1.6).contains(&chord_db),
        "the chord frame is {chord_db:+.2} dB against the outgoing record — the hold is not flat"
    );

    let window = 3 * RATE;
    let span = frames.saturating_sub(window);
    let mut dip = 0.0f64;
    let mut rise = 0.0f64;
    let mut at = 0;
    while at <= span {
        let m = 20.0 * rms_window(&rendered.mixed, at, window).max(1e-9).log10();
        let o = 20.0 * rms_window(&rendered.outgoing, at, window).max(1e-9).log10();
        let i = 20.0 * rms_window(&rendered.incoming, at, window).max(1e-9).log10();
        dip = dip.min(m - o.min(i));
        rise = rise.max(m - o.max(i));
        at += 4_800;
    }
    println!("ColdEnd: chord {chord}f {chord_db:+.2} dB, 3s dip {dip:.2} dB, rise {rise:.2} dB");
    assert!(dip > -3.0, "the cold ending dipped {dip:.2} dB");
    assert!(rise < 2.0, "the cold ending rose {rise:.2} dB");
}

/// The two crossfade curves, and why they have to differ.
///
/// Regression: a source swap used the equal-power pair, so the same
/// recording on both sides — correlated, and aligned by construction —
/// summed to √2 through the middle of the fade, and every quality upgrade
/// was heard as the volume lifting and settling back.
#[test]
fn swap_fade_is_level_flat_while_a_track_fade_is_power_flat() {
    let mut worst_swap_db = 0.0f64;
    let mut worst_track_db = 0.0f64;
    for step in 0..=1000 {
        let p = step as f64 / 1000.0;

        // Correlated sides: amplitudes add, so the sum must stay at 1.
        let (rise, fall) = fade_gains(p, true);
        let sum = rise as f64 + fall as f64;
        worst_swap_db = worst_swap_db.max(20.0 * sum.log10().abs());

        // Uncorrelated sides: powers add, so the power sum must stay at 1.
        let (rise, fall) = fade_gains(p, false);
        let power = (rise as f64).powi(2) + (fall as f64).powi(2);
        worst_track_db = worst_track_db.max(10.0 * power.log10().abs());
    }
    assert!(
        worst_swap_db < 1e-4,
        "swap fade swells by {worst_swap_db} dB"
    );
    assert!(
        worst_track_db < 1e-4,
        "track fade dips by {worst_track_db} dB"
    );

    // What a plain equal-power pair costs when both sides are the same
    // signal — recorded so the branch above cannot quietly be undone, and
    // so the reason the incoming no longer uses one is written down where
    // the arithmetic is.
    let quarter = core::f64::consts::FRAC_PI_4;
    let bump_db = 20.0 * (quarter.sin() + quarter.cos()).log10();
    assert!(
        (bump_db - 3.01).abs() < 0.02,
        "expected the √2 bump at the midpoint, got {bump_db} dB"
    );
}

/// A plain crossfade is equal-power: silent at the open, unity at the
/// close, and level-flat the whole way. A DJ blend is a different curve.
#[test]
fn a_plain_crossfade_is_equal_power_and_starts_from_silence() {
    let (first_rise, first_fall) = fade_gains(0.0, false);
    let (last_rise, last_fall) = fade_gains(1.0, false);
    assert!(
        first_rise.abs() < 1e-6,
        "a crossfade opens on silence, got {first_rise}"
    );
    assert!((first_fall - 1.0).abs() < 1e-6);
    assert!((last_rise - 1.0).abs() < 1e-6);
    assert!(last_fall.abs() < 1e-6);
    let mut worst_db = 0.0f64;
    for step in 0..=1000 {
        let (rise, fall) = fade_gains(step as f64 / 1000.0, false);
        let power = (rise as f64).powi(2) + (fall as f64).powi(2);
        worst_db = worst_db.max(10.0 * power.log10().abs());
    }
    assert!(
        worst_db < 1e-4,
        "equal-power moved the level by {worst_db} dB"
    );
}

/// E-2: the outgoing holds unity through the final chord's attack, the
/// incoming waits underneath, and the two cross over the decay without the
/// summed level lifting or dipping by even a decibel.
#[test]
fn an_effect_intro_has_both_records_playing_before_the_throw() {
    let hold = 0.6;
    // Across the intro the outgoing is at unity and the incoming climbs
    // to its intro level, reaching it exactly at the throw.
    let (mid_rise, mid_fall) = effect_intro_gains(0.3, hold, -4.0);
    assert_eq!(mid_fall, 1.0);
    assert!(
        mid_rise > 0.2,
        "the next record is audible mid-intro: {mid_rise}"
    );
    let (at_throw, fall) = effect_intro_gains(hold, hold, -4.0);
    assert!((at_throw as f64 - db_to_gain(-4.0)).abs() < 1e-4);
    assert_eq!(fall, 1.0);
    // No step anywhere, and unity at the end.
    let mut last = 0.0f32;
    for step in 0..=1000 {
        let (rise, _) = effect_intro_gains(step as f64 / 1000.0, hold, -4.0);
        assert!(rise >= last - 1e-6 && rise - last < 0.01, "step at {step}");
        last = rise;
    }
    assert!((last - 1.0).abs() < 1e-6);
    // The bass waits for the landing; the top opens across the throw.
    let mut filter = TransitionFilter::new(2, 48_000);
    let landing = effect_landing(
        &TransitionPlan {
            style: TransitionStyle::EchoOut,
            beat_seconds: 0.5,
            ..Default::default()
        },
        hold,
    );
    assert!(landing > hold && landing < 1.0);
    ride_effect_intro(0.3, hold, landing, 1.0, &mut filter);
    assert!(filter.active(), "filtered under the outgoing");
}

#[test]
fn a_cold_ending_holds_the_chord_at_unity() {
    // Pair 5's shape: a 3.36 s phrase inside a 4.86 s blend, so the chord's
    // decay is the last 1.5 s.
    let hold = 3.36 / 4.86;
    let (rise, fall) = cold_end_gains(hold, hold);
    assert!(
        (fall - 1.0).abs() < 1e-6,
        "the final chord is faded through: {fall}"
    );
    assert!(
        (rise as f64 - db_to_gain(COLD_END_UNDERLAY_DB)).abs() < 1e-6,
        "the incoming is not underneath at the chord: {rise}"
    );
    // Relative to the incoming's own matched level, never an absolute one.
    let underlay_db = 20.0 * (rise as f64).log10();
    assert!(
        (underlay_db + 12.0).abs() < 0.01,
        "the underlay is {underlay_db:.2} dB, not −12 dB under the matched level"
    );
    // It has started to leave immediately after the chord.
    let (_, after) = cold_end_gains(hold + 0.02, hold);
    assert!(
        after < 0.999,
        "the outgoing did not begin to fall after the chord: {after}"
    );
    // No +3 dB bump while the outgoing is held: the underlay is what keeps
    // the sum near unity, and this is the assertion that stops it drifting.
    let mut worst_db = 0.0f64;
    for step in 0..=1000 {
        let (rise, fall) = cold_end_gains(step as f64 / 1000.0, hold);
        let power = (rise as f64).powi(2) + (fall as f64).powi(2);
        worst_db = worst_db.max(10.0 * power.log10().abs());
    }
    assert!(
        worst_db < 1.0,
        "the cold-ending hold moved the summed level by {worst_db} dB"
    );
    let (end_rise, end_fall) = cold_end_gains(1.0, hold);
    assert!((end_rise - 1.0).abs() < 1e-6 && end_fall.abs() < 1e-6);
    // A plan without a hold keeps the plain equal-power pair.
    let (plain_rise, plain_fall) = cold_end_gains(0.5, 0.0);
    let (fade_rise, fade_fall) = fade_gains(0.5, false);
    assert!((plain_rise - fade_rise).abs() < 1e-6);
    assert!((plain_fall - fade_fall).abs() < 1e-6);
}

/// Bed, then a rise, then the outgoing lets go. The incoming is already
/// audible while the outgoing is still at full level, and a one-beat dip
/// sits just before the swap.
#[test]
fn a_dj_blend_beds_the_incoming_track_then_swaps() {
    let plan = TransitionPlan {
        style: TransitionStyle::DjBlend,
        bass_swap: true,
        bass_swap_fraction: 0.62,
        bed_fraction: 0.4,
        bed_gain_db: -14.0,
        dip_depth: 0.5,
        dip_width: 0.08,
        fade_seconds: 12.0,
        effect_seconds: 0.0,
        effect_start_seconds: 0.0,
        ..TransitionPlan::default()
    };
    let bed = 10.0_f64.powf(-14.0 / 20.0) as f32;
    let (open_rise, open_fall) = automix_gains(0.0, &plan, 0.0);
    assert!(
        open_rise.abs() < 1e-4,
        "the bed eases in from silence, got {open_rise}"
    );
    assert!(
        (open_fall - 1.0).abs() < 1e-4,
        "the outgoing stays up, got {open_fall}"
    );

    let (bed_rise, bed_fall) = automix_gains(0.2, &plan, 0.0);
    assert!(
        bed_rise > bed * 0.5 && bed_rise < 0.45,
        "early in the blend the incoming is underneath, got {bed_rise}"
    );
    assert!(
        (bed_fall - 1.0).abs() < 1e-4,
        "the outgoing is still the record"
    );

    let (late_rise, late_fall) = automix_gains(0.95, &plan, 0.0);
    assert!(
        late_rise > 0.95,
        "after the swap the incoming is the record, got {late_rise}"
    );
    assert!(
        late_fall < 0.35,
        "after the swap the outgoing is leaving, got {late_fall}"
    );

    // The dip is on the incoming, just before the swap, and the outgoing
    // gain at that moment is still full. Sample a half-width either side
    // of the notch so the comparison is the dip, not the climb into it.
    let before = automix_gains(0.54, &plan, 0.0).0;
    let notch = automix_gains(0.58, &plan, 0.0).0;
    let after = automix_gains(0.70, &plan, 0.0).0;
    assert!(
        notch < before * 0.85 && after > notch,
        "expected a dip then a recovery, got {before} -> {notch} -> {after}"
    );
    let outgoing_at_dip = automix_gains(0.58, &plan, 0.0).1;
    assert!(
        outgoing_at_dip > 0.75,
        "the dip is on the incoming; the outgoing is still leaving slowly, got {outgoing_at_dip}"
    );
}
