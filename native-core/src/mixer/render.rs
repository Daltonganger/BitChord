//! Rendering the decks into the output ring, mixed or bit-perfect.

use super::curves::fall_gain;
use super::deck::normalization_gain;
use super::state::{MixerState, Phase};
use super::voice::CHUNK_FRAMES;
use super::BAIL_MS;
use crate::sound::SoundMode;
use rtrb::Producer;
use std::sync::atomic::Ordering;

/// Renders while the ring has room for a chunk.
pub(super) fn render_available(state: &mut MixerState, ring: &mut Producer<f32>) {
    if state.underrun_counter.is_some()
        && (state.flush_ring.load(Ordering::Acquire) || state.bail_flush.load(Ordering::Acquire))
    {
        return;
    }
    if state.bit_perfect {
        render_bitperfect_available(state, ring);
        return;
    }
    for voice in state.current.iter_mut().chain(state.incoming.iter_mut()) {
        voice.normalization_mode = state.loudness_mode;
    }
    let chunk_samples = CHUNK_FRAMES * 2;
    loop {
        // Maintenance, evaluated per chunk rather than once per top-up.
        //
        // These decisions are about where the *audible* playhead is — arm the
        // incoming, start the fade — and this loop renders until the ring is
        // full, which is however much the device callback has drained since the
        // previous pass. While the callback keeps up, that is a handful of
        // chunks and checking once was equivalent. When it does not — a cold
        // ring, a slow decoder, a harness draining as fast as it can — a single
        // pass can render the whole fade window, or the whole track, before the
        // check runs again, and the blend lands wherever the ring ran out
        // instead of where the plan said.
        //
        // Upstream evaluates this on a 30 ms timer (`CrossfadeController.tick`),
        // which is the shape matched here: a cadence tied to the playhead, not
        // to buffer occupancy.
        state.consider_arm();
        state.consider_start_fade();
        if state.buffered_frames.load(Ordering::Relaxed)
            >= state.device_rate as u64 * state.queue_target_ms / 1000
        {
            break;
        }
        let free = ring.slots();
        if free < chunk_samples {
            break;
        }
        let mut scratch = vec![0.0f32; chunk_samples];
        let mut audible = false;
        // Samples actually rendered this pass — zero padding beyond this is
        // NOT audio and must not enter the ring (it would pin `buffered` and
        // stall every tail computation after the current track ends).
        let mut produced = 0usize;

        // Fade drive.
        let transition_done = state.drive_fade();
        if transition_done {
            state.finish_transition();
        }

        // Gapless fallback: current ended with an incoming already armed, or
        // a pending next that never got a fade window (unknown duration).
        let current_finished = state
            .current
            .as_ref()
            .is_some_and(|c| c.finished && c.pending_dev_cursor >= c.pending_dev.len());
        if current_finished && state.transition.is_none() {
            if state.incoming.is_some() {
                state.promote_incoming();
            } else if state.pending_next.is_some() {
                state.promote_pending();
            }
        }

        // Retiring voices (bail/skip ramps) keep decoding while they fade.
        let mut keep_retiring = Vec::new();
        while let Some(mut ret) = state.retiring.pop() {
            let elapsed = ret.started_at.elapsed().as_millis() as f64 / BAIL_MS as f64;
            if elapsed < 1.0 {
                let frames = ret.voice.pull(CHUNK_FRAMES, state.device_rate);
                if !frames.is_empty() {
                    audible = true;
                    produced = produced.max(frames.len());
                    ret.voice.mix_into(
                        &frames,
                        &mut scratch,
                        ret.from_gain * fall_gain(elapsed),
                        state.loudness_enabled && state.sound_mode == SoundMode::Enhanced,
                    );
                }
                keep_retiring.push(ret);
            }
        }
        state.retiring = keep_retiring;

        // A swap fades a recording into another copy of itself, so the one
        // number that decides whether the curve is right is how correlated the
        // two copies are, and only a real upgrade can supply it. The samples
        // are already decoded and aligned here, so the probe is nearly free.
        let swap_fade = state.transition.as_ref().is_some_and(|t| t.swap);
        if let Some(voice) = &mut state.current {
            if state.playing {
                let rate = state.device_rate;
                let speed = state.playback_speed;
                voice.drive_tempo_glide(rate, speed);
                let frames = voice.pull(CHUNK_FRAMES, rate);
                voice.note_glide_frames((frames.len() / 2) as u64);
                let effect_active = state.transition.as_ref().is_some_and(|t| {
                    t.phase == Phase::Fading
                        && t.fx.is_some()
                        && state
                            .incoming
                            .as_ref()
                            .is_some_and(|voice| voice.emitted_dev_frames >= t.effect_start_frame)
                });
                let effect = if effect_active {
                    state
                        .transition
                        .as_mut()
                        .filter(|t| t.phase == Phase::Fading)
                        .and_then(|t| {
                            let gain = *t.effect_input_gain.get_or_insert(voice.gain);
                            t.fx.as_mut().map(|fx| (fx, gain))
                        })
                } else {
                    None
                };
                if let Some((effect, input_gain)) = effect {
                    // The outgoing deck runs through its effect, and keeps
                    // running after its file ends: the tail is the point.
                    let mut dry = vec![0.0f32; frames.len()];
                    voice.mix_into(
                        &frames,
                        &mut dry,
                        input_gain,
                        state.loudness_enabled && state.sound_mode == SoundMode::Enhanced,
                    );
                    effect.render(&dry, &mut scratch, CHUNK_FRAMES);
                    audible = true;
                    produced = produced.max(chunk_samples);
                } else if !frames.is_empty() {
                    audible = true;
                    produced = produced.max(frames.len());
                    if swap_fade {
                        state.swap_probe.observe_current(&frames);
                    }
                    voice.mix_into(
                        &frames,
                        &mut scratch,
                        voice.gain,
                        state.loudness_enabled && state.sound_mode == SoundMode::Enhanced,
                    );
                }
            }
        }
        for voice in state.current.iter_mut().chain(state.incoming.iter_mut()) {
            if let Some(error) = voice.decode_error.take() {
                state.events.error(error);
            }
        }
        if state.playing {
            state.publish_audible_position();
        }

        // The incoming rises across the whole blend (the effect only dresses
        // the outgoing), so it is pulled from the first frame.
        let incoming_renders = state
            .transition
            .as_ref()
            .is_some_and(|t| t.phase == Phase::Fading);
        if let Some(voice) = &mut state.incoming {
            if state.playing && incoming_renders {
                let frames = voice.pull(CHUNK_FRAMES, state.device_rate);
                if !frames.is_empty() {
                    audible = true;
                    produced = produced.max(frames.len());
                    if swap_fade {
                        state.swap_probe.observe_incoming(&frames);
                    }
                    voice.mix_into(
                        &frames,
                        &mut scratch,
                        voice.gain,
                        state.loudness_enabled && state.sound_mode == SoundMode::Enhanced,
                    );
                }
            }
        }

        let draining_pipeline = produced == 0 && state.pipeline_tail_frames > 0;
        if produced > 0 && state.sound_mode == SoundMode::Enhanced {
            state.pipeline_tail_frames = state.protector.delay_frames()
                + state.clarity.delay_frames() as usize
                + state.device_rate as usize * 3;
        } else if produced == 0 && state.pipeline_tail_frames > 0 {
            let frames = state.pipeline_tail_frames.min(CHUNK_FRAMES);
            produced = frames * 2;
            state.pipeline_tail_frames -= frames;
            audible = true;
        }
        if !audible {
            // No voice produced audio this chunk. The queue is truly spent
            // only when nothing is loaded, mid-transition, or winding down —
            // a finished current voice alone still has the armed incoming
            // promotion (above) or must end the queue.
            let any_pending = state.current.as_ref().is_some_and(|c| !c.finished)
                || state.incoming.is_some()
                || state.pending_next.is_some()
                || state.transition.is_some();
            let ring_drained = state.buffered_frames.load(Ordering::Relaxed) == 0;
            if !any_pending && ring_drained && state.retiring.is_empty() {
                if state.playing {
                    log::info!("queue drained; natural end");
                    state.playing = false;
                    state.set_state(crate::PlaybackState::Stopped);
                    state.events.track_ended(
                        crate::TrackEndReason::Natural,
                        state
                            .current
                            .as_ref()
                            .map(|v| v.info.source.clone())
                            .unwrap_or_default(),
                    );
                }
                break;
            }
        }

        // Push only rendered audio. Protection carries detector and gain state
        // across render blocks, after the float-headroom processing stages.
        let mut pushed = 0;
        if produced > 0 {
            if state.sound_mode == SoundMode::Enhanced {
                state.eq.process(&mut scratch[..produced]);
            }
            state.clarity.process(&mut scratch[..produced]);
            state.protector.process(
                &mut scratch[..produced],
                state.sound_mode == SoundMode::Enhanced,
            );
            if let Ok(mut nerd) = state.nerd.lock() {
                let mut stages = Vec::new();
                let primary = if state.transition.as_ref().is_some_and(|t| t.handed_off) {
                    state.incoming.as_ref().or(state.current.as_ref())
                } else {
                    state.current.as_ref()
                };
                if let Some(v) = primary {
                    if !v.resampler.passthrough() {
                        stages.push("Rate conversion (libsoxr HQ)".into());
                    }
                    nerd.converter_delay_frames = v.resampler.delay_frames();
                    nerd.decoder_implementation = v.decoder.implementation().into();
                    nerd.decoded_layout = v.decoder.layout();
                    nerd.loudness_origin = if v.replay_gain.gain_db(v.normalization_mode).is_some()
                    {
                        "ReplayGain metadata"
                    } else if v.measured_loudness.is_some() {
                        "EBU R128 / BS.1770 LUFS"
                    } else if v.loudness_db.is_some() {
                        "YouTube relative loudness (not LUFS)"
                    } else {
                        "No measurement"
                    }
                    .into();
                    if v.info.channels != 2 {
                        stages.push("Channel mapping".into());
                    }
                    if v.user_stretch.is_some() {
                        stages.push("Pitch-preserving speed".into());
                    }
                    if v.stretch.is_some() {
                        stages.push("Automix tempo matching".into());
                    }
                    if v.spatial.enabled() {
                        stages.push("Spatial / head tracking".into());
                    }
                    if v.skip_silence {
                        stages.push("Silence trimming".into());
                    }
                }
                if state.transition.is_some() {
                    stages.push("Transition".into());
                }
                if state.volume != 1.0 || state.sleep_gain != 1.0 {
                    stages.push("Application volume".into());
                }
                if state.clarity.active() {
                    stages.push("Enhanced clarity (4× exciter)".into());
                    if state.eq.active() {
                        stages.push("Manual EQ / tone".into());
                    }
                    stages.push("True-peak protection".into());
                }
                for v in state.current.iter().chain(state.incoming.iter()) {
                    for (active, name) in [
                        (v.spatial.enabled(), "Spatial / head tracking"),
                        (v.stretch.is_some(), "Automix tempo matching"),
                        (v.user_stretch.is_some(), "Pitch-preserving speed"),
                        (v.filter.active(), "Transition filters"),
                    ] {
                        if active && !stages.iter().any(|s| s == name) {
                            stages.push(name.into());
                        }
                    }
                }
                if let Some(v) = primary {
                    nerd.loudness_gain_db = normalization_gain(
                        v,
                        state.loudness_enabled && state.sound_mode == SoundMode::Enhanced,
                    )
                    .1;
                }
                if nerd.loudness_gain_db.is_some() && state.sound_mode == SoundMode::Enhanced {
                    stages.push("Normalization".into());
                } else if state
                    .current
                    .as_ref()
                    .is_some_and(|v| v.smoothed_normalization != 1.0)
                {
                    stages.push("Normalization settling".into());
                }
                nerd.active_stages = stages;
                nerd.protection_reduction_db = state.protector.reduction_db;
                nerd.protection_interventions = state.protector.interventions
                    + state
                        .current
                        .as_ref()
                        .map(|v| v.decoder.sanitized_samples())
                        .unwrap_or(0);
                nerd.queued_target_ms = state.queue_target_ms;
                nerd.build_revision = env!("BITCHORD_CORE_REVISION").into();
            }

            if draining_pipeline
                && state.pipeline_tail_frames < state.device_rate as usize * 3
                && scratch[..produced].iter().all(|s| s.abs() < 1e-7)
            {
                state.pipeline_tail_frames = 0;
            }
            crate::diagnostics::capture(2, &scratch[..produced]);
            state
                .buffered_frames
                .fetch_add(produced as u64 / 2, Ordering::Release);
            for sample in scratch.into_iter().take(produced) {
                if ring.push(sample).is_ok() {
                    pushed += 1;
                } else {
                    break;
                }
            }
        }
        if pushed < produced {
            let missing = ((produced - pushed) / 2) as u64;
            let _ = state
                .buffered_frames
                .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub(missing))
                });
        }

        break;
    }
}

/// Direct one-voice render for the qualified integer output path. It avoids
/// gain multiplication, EQ, fades, clamping, and crossfade scheduling so the
/// decoded PCM reaches the integer output packer unchanged.
pub(super) fn render_bitperfect_available(state: &mut MixerState, ring: &mut Producer<f32>) {
    if state.underrun_counter.is_some()
        && (state.flush_ring.load(Ordering::Acquire) || state.bail_flush.load(Ordering::Acquire))
    {
        return;
    }
    let chunk_samples = CHUNK_FRAMES * 2;
    while ring.slots() >= chunk_samples {
        if state.buffered_frames.load(Ordering::Relaxed)
            >= state.device_rate as u64 * state.queue_target_ms / 1000
        {
            break;
        }
        if !state.playing {
            break;
        }
        let frames = match &mut state.current {
            Some(voice) if voice.bit_perfect && voice.info.sample_rate == state.device_rate => {
                voice.pull(CHUNK_FRAMES, state.device_rate)
            }
            Some(_) => {
                // A route changed underneath an active direct stream. Do not
                // lie about the clock match; drop back to the resampling path.
                state.bit_perfect = false;
                return render_available(state, ring);
            }
            None => Vec::new(),
        };
        if frames.is_empty() {
            let exhausted = state.current.as_ref().is_none_or(|voice| voice.finished);
            if exhausted && state.buffered_frames.load(Ordering::Relaxed) == 0 {
                state.playing = false;
                state.set_state(crate::PlaybackState::Stopped);
                state.events.track_ended(
                    crate::TrackEndReason::Natural,
                    state
                        .current
                        .as_ref()
                        .map(|v| v.info.source.clone())
                        .unwrap_or_default(),
                );
            }
            break;
        }
        state.publish_audible_position();
        if let Ok(mut nerd) = state.nerd.lock() {
            nerd.active_stages.clear();
            nerd.converter_delay_frames = 0.0;
            nerd.protection_reduction_db = 0.0;
            nerd.protection_interventions = 0;
            nerd.loudness_gain_db = None;
            nerd.loudness_origin = "Bypassed for qualified integer output".into();
            nerd.queued_target_ms = state.queue_target_ms;
            nerd.build_revision = env!("BITCHORD_CORE_REVISION").into();
            if let Some(v) = state.current.as_ref() {
                nerd.decoder_implementation = v.decoder.implementation().into();
                nerd.decoded_layout = v.decoder.layout();
            }
        }
        let frames_len = frames.len() as u64;
        state
            .buffered_frames
            .fetch_add(frames_len / 2, Ordering::Release);
        let mut pushed = 0u64;
        for sample in frames {
            if ring.push(sample).is_ok() {
                pushed += 1;
            } else {
                break;
            }
        }
        if pushed < frames_len {
            let _ = state
                .buffered_frames
                .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub((frames_len - pushed) / 2))
                });
        }
        break;
    }
}
