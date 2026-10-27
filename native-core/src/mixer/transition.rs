//! The transition lifecycle: arm the incoming, start the fade, drive it,
//! and finish or upgrade it.

use super::curves::{
    automix_gains, cold_end_gains, effect_intro_db, effect_intro_gains, effect_landing, fade_gains,
    hold_fraction, ride_effect_intro, ride_filters,
};
use super::state::{MixerState, Phase, TransitionState};
use super::types::{TrackInfo, TrackSource, TransitionPlan, TransitionStyle};
use super::voice::Voice;
use super::{
    accepts_analyzed_plan, automix_winner_record, fx, prime_voice, ARM_LEAD_MS, ARM_TIMEOUT_MS,
};
use crate::sound::SoundMode;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

impl MixerState {
    /// A later plan for the track already armed, applied only while the fade
    /// has not started.
    ///
    /// The safety overlap is queued as soon as the next file's first bytes
    /// exist, and the analysed plan can take longer than that. Dropping the
    /// analysed plan because the safety one already armed is how a blend that
    /// was ready still played out to the file's last byte.
    pub(super) fn upgrade_armed_plan(&mut self, request: TrackSource) {
        let still_waiting = self
            .transition
            .as_ref()
            .is_some_and(|transition| accepts_analyzed_plan(transition.phase, transition.swap));
        if !still_waiting {
            log::debug!("queueNext ignored; transition already fading");
            return;
        }
        let cue = request.start_seconds.max(0.0);
        let track = self
            .current
            .as_ref()
            .and_then(|voice| voice.known_duration)
            .unwrap_or(0.0);
        let fade_s = self.plan_seconds(&request.plan, track);
        let rate = if request.plan.playback_rate > 0.05 {
            request.plan.playback_rate
        } else {
            1.0
        };
        let glide = request.plan.post_glide_seconds.max(0.0);
        if let Some(transition) = &mut self.transition {
            if fade_s > 0.0 {
                transition.fade_frames = (fade_s * self.device_rate as f64) as u64;
            }
            transition.plan = request.plan;
        }
        if let Some(voice) = &mut self.incoming {
            voice.base_plan_rate = rate;
            voice.plan_rate = rate;
            voice.post_glide_seconds = glide;
            if (voice.decoder.position_seconds() - cue).abs() > 0.05 {
                voice.base_position = 0.0;
                voice.reset_processing();
                if let Err(e) = voice.decoder.seek_seconds(cue) {
                    log::warn!("could not move the armed cue to {cue:.2}s: {e}");
                }
            }
            // A moved cue clears the primed buffer in `reset_processing`; fill
            // it again so the first fade chunk is not a cold decode.
            prime_voice(voice, self.device_rate);
        }
        log::info!("updated armed transition: fade={fade_s:.1}s cue={cue:.2}s rate={rate:.4}");
    }

    /// Convert an armed successor into an immediate two-beat EchoOut when a
    /// trusted planner grid is present. The queue owner keeps its current skip
    /// path when this returns `None`. This is the native short-window port of
    /// upstream `DesktopAudioEngine.beginSkipMix`.
    pub(super) fn begin_skip_mix(&mut self) -> Option<TrackInfo> {
        let transition = self.transition.as_mut()?;
        if !accepts_analyzed_plan(transition.phase, transition.swap) {
            return None;
        }
        let beat = transition.plan.beat_seconds;
        if !beat.is_finite() || !(0.25..=1.5).contains(&beat) {
            return None;
        }
        let current = self.current.as_ref()?;
        let incoming = self.incoming.as_ref()?;
        let device_rate = self.device_rate.max(1);
        let beat_frames = (beat * device_rate as f64).round().max(1.0) as u64;
        let fade_frames = beat_frames.saturating_mul(2);
        let fade_seconds = fade_frames as f64 / device_rate as f64;
        if current
            .audible_remaining_seconds()
            .is_some_and(|remaining| remaining < fade_seconds)
            || incoming
                .audible_remaining_seconds()
                .is_some_and(|remaining| remaining < fade_seconds)
        {
            return None;
        }
        let start = current.emitted_position_seconds();
        let info = incoming.info.clone();
        transition.fade_frames = fade_frames;
        transition.plan.style = TransitionStyle::EchoOut;
        transition.plan.fade_seconds = fade_seconds;
        transition.plan.transition_end_seconds = start + fade_seconds;
        transition.plan.effect_start_seconds = start;
        transition.plan.effect_seconds = fade_seconds;
        transition.plan.outgoing_hold_seconds = 0.0;
        transition.plan.bed_fraction = 0.0;
        transition.plan.bed_gain_db = 0.0;
        transition.plan.dip_depth = 0.0;
        transition.plan.dip_width = 0.0;
        transition.plan.playback_rate = 1.0;
        transition.plan.post_glide_seconds = 0.0;
        transition.plan.reason = "skip-mix:".into();
        if let Some(incoming) = &mut self.incoming {
            incoming.base_plan_rate = 1.0;
            incoming.post_glide_seconds = 0.0;
            incoming.release_plan_stretch(self.playback_speed, self.device_rate);
        }
        log::info!("automix: skip-mix style=EchoOut window={fade_seconds:.2}s beat={beat:.3}s");
        Some(info)
    }

    /// Opens a different file on a transition that has not started sounding.
    ///
    /// This is how a better encode of the next record joins the blend: the
    /// ramp the listener hears is the song change, not a second crossfade
    /// laid on top of it. Once the fade is audible the voice stays — swapping
    /// it then is the cut this exists to avoid.
    pub(super) fn retarget_armed_source(&mut self, request: TrackSource) {
        match Voice::open(
            &request,
            self.spatial_enabled && self.sound_mode == SoundMode::Enhanced,
            self.head_yaw,
            self.device_rate,
            self.playback_speed,
            self.skip_silence && self.sound_mode == SoundMode::Enhanced,
            true,
        ) {
            Ok(mut voice) => {
                voice.gain = 0.0;
                voice.applied_gain = 0.0;
                voice.normalization_mode = self.loudness_mode;
                voice.arm_transition_match(
                    self.current.as_ref(),
                    self.loudness_enabled && self.sound_mode == SoundMode::Enhanced,
                );
                let track = self
                    .current
                    .as_ref()
                    .and_then(|current| current.known_duration)
                    .unwrap_or(0.0);
                let fade_s = self.plan_seconds(&request.plan, track);
                let rate = if request.plan.playback_rate > 0.05 {
                    request.plan.playback_rate
                } else {
                    1.0
                };
                voice.base_plan_rate = rate;
                voice.plan_rate = rate;
                voice.post_glide_seconds = request.plan.post_glide_seconds.max(0.0);
                // Prime the replacement before it is needed: this voice is
                // pulled for the first time at the fade, and a cold decode is
                // the hole the blend cannot afford.
                prime_voice(&mut voice, self.device_rate);
                if let Some(transition) = &mut self.transition {
                    if fade_s > 0.0 {
                        transition.fade_frames = (fade_s * self.device_rate as f64) as u64;
                    }
                    transition.plan = request.plan;
                }
                log::info!(
                    "retargeted armed transition to {} fade={fade_s:.1}s",
                    voice.info.source
                );
                self.incoming = Some(voice);
            }
            Err(e) => {
                log::warn!("could not retarget the armed transition: {e}");
            }
        }
    }

    pub(super) fn consider_arm(&mut self) {
        if self.hold_end
            || self.transition.is_some()
            || self.pending_next.is_none()
            || !self.playing
        {
            return;
        }
        let Some(current) = &self.current else {
            return;
        };
        if current.finished {
            return;
        }
        // Unknown or implausibly short duration (muxed MP4 used to report
        // AAC packet counts as PCM frames): wait for real EOS instead of
        // blending immediately. Upstream uses ExoPlayer's container duration.
        if current.known_duration.unwrap_or(0.0) < 2.0 && current.position_seconds() < 2.0 {
            return;
        }
        // The plan decides both where the blend ends and how long it runs —
        // upstream arms from `plan.transitionStart` (CrossfadeController.kt:713)
        // and starts the fade from the same window (`:731` → `:967`). Arming
        // from the file end instead prepared the incoming only after the moment
        // the fade was already due, which pushed the whole blend late.
        let plan = self
            .pending_next
            .as_ref()
            .map(|request| request.plan.clone())
            .unwrap_or_default();
        let window_s = self.effective_fade_seconds(current.known_duration.unwrap_or(0.0));
        let fade_s = if plan.fade_seconds > 0.0 {
            self.plan_seconds(&plan, current.known_duration.unwrap_or(0.0))
        } else {
            window_s
        };
        let audible_tail_s = self.audible_tail_s(&plan, current);
        if !audible_tail_s.is_finite() {
            // There is no end to schedule against, so there is no blend: the
            // incoming is never armed and the queue advances by a cut at
            // end-of-stream. Worth saying out loud, because from the outside it
            // is indistinguishable from a crossfade setting of zero.
            if !self.warned_no_duration {
                self.warned_no_duration = true;
                log::warn!(
                    "no transition can be scheduled for {:?}: neither the container nor the \
                     caller knows how long it is, so there is no end to schedule against. The \
                     next track will be cut in at the end rather than mixed. Pass \
                     LoadRequest.durationSeconds, or have the planner set \
                     TransitionPlan.outgoing_duration_seconds.",
                    current.info.title,
                );
            }
            return;
        }
        let need_s = fade_s + ARM_LEAD_MS as f64 / 1000.0;
        if audible_tail_s > need_s {
            return;
        }

        let request = self.pending_next.take().unwrap();
        let fade_frames = (fade_s * self.device_rate as f64) as u64;
        match Voice::open(
            &request,
            self.spatial_enabled && self.sound_mode == SoundMode::Enhanced,
            self.head_yaw,
            self.device_rate,
            self.playback_speed,
            self.skip_silence && self.sound_mode == SoundMode::Enhanced,
            true,
        ) {
            Ok(mut voice) => {
                voice.gain = 0.0; // silent until the fade lifts it
                voice.applied_gain = 0.0;
                // The match is measured after normalization, so the voice has to
                // know what normalization it will get before the correction is
                // worked out — a freshly opened voice is in `Off`.
                voice.normalization_mode = self.loudness_mode;
                voice.arm_transition_match(
                    self.current.as_ref(),
                    self.loudness_enabled && self.sound_mode == SoundMode::Enhanced,
                );
                let match_db = voice.match_db;
                if let Ok(mut nerd) = self.nerd.lock() {
                    nerd.transition_match_db = (match_db.abs() >= 0.05).then_some(match_db);
                }
                // Decode the cue now rather than at the fade: the arm is
                // seconds ahead, and this is the only cold decode in the path.
                prime_voice(&mut voice, self.device_rate);
                log::info!("armed next track: {}", request.title);
                self.incoming = Some(voice);
                self.transition = Some(TransitionState {
                    phase: Phase::Arming,
                    fade_frames,
                    handed_off: false,
                    plan,
                    arm_deadline: Instant::now() + Duration::from_millis(ARM_TIMEOUT_MS),
                    swap: false,
                    fx: None,
                    effect_start_frame: 0,
                    effect_input_gain: None,
                });
            }
            Err(e) => {
                // Upstream gives up and lets the queue move on plainly: a
                // missed crossfade rather than a broken one.
                log::warn!("arm failed for {}: {e}", request.title);
                self.events
                    .error(format!("could not prepare next track: {e}"));
                self.transition = None;
            }
        }
    }

    pub(super) fn effective_fade_seconds(&self, total: f64) -> f64 {
        // A fade that swallows a third of a song stops being a transition —
        // upstream's `fadeFor` cap. Applied against the *current* track.
        let window = self.crossfade_window_s.max(0.0);
        if total > 0.0 {
            return window.min(total / 3.0);
        }
        window
    }

    /// A planned fade, kept off a half-second floor.
    ///
    /// Analysis of a partial file, or a grid snap onto the mix-out, can hand
    /// the mixer a fade of a few hundred milliseconds. On a track with room
    /// for four seconds, that figure is a cut and the floor replaces it. A
    /// plan that already asked for a real blend is left alone, including one
    /// that spends most of a short record — the planner chose that length.
    /// The window a plan needs: a technique's own length, in beats, or the
    /// floored blend length for a crossfade.
    pub(super) fn plan_seconds(&self, plan: &TransitionPlan, track_seconds: f64) -> f64 {
        if plan.style.is_effect() {
            // The effect rides a real crossfade: its length is the planner's
            // blend, and the technique only fills the last sub-window.
            if plan.fade_seconds > 0.0 {
                return plan.fade_seconds;
            }
            return fx::window_seconds(plan.style, plan.beat_seconds)
                .unwrap_or_else(|| self.blend_seconds(plan.fade_seconds, track_seconds));
        }
        self.blend_seconds(plan.fade_seconds, track_seconds)
    }

    pub(super) fn blend_seconds(&self, plan_fade: f64, track_seconds: f64) -> f64 {
        let planned = plan_fade.max(0.0);
        // A region-limited handoff must never be expanded across a skipped interval.
        let active = self.current.iter().chain(self.incoming.iter()).any(|v| {
            v.regions.audible_start_seconds > 0.0
                || v.regions.audible_end_seconds.is_some()
                || !v.regions.excluded.is_empty()
        });
        let pending = self
            .pending_next
            .as_ref()
            .map(|r| crate::playback_regions::get(&r.source))
            .unwrap_or_default();
        if active
            || pending.audible_start_seconds > 0.0
            || pending.audible_end_seconds.is_some()
            || !pending.excluded.is_empty()
        {
            return planned;
        }
        if planned == 0.0 || planned >= 4.0 {
            return planned;
        }
        if track_seconds > 0.0 && track_seconds < 12.0 {
            return planned;
        }
        4.0
    }

    /// Starts the fade the moment the audible tail has shrunk to it — or, for
    /// gapless (zero window), at the exact end.
    pub(super) fn consider_start_fade(&mut self) {
        let Some(t) = &self.transition else {
            return;
        };
        if t.phase != Phase::Arming {
            return;
        }
        let Some(current) = &self.current else {
            return;
        };
        let Some(_incoming) = &self.incoming else {
            return;
        };
        if Instant::now() > t.arm_deadline {
            // Incoming never became ready; queue moves on plainly.
            log::warn!("arm timeout; dropping transition");
            self.incoming = None;
            self.transition = None;
            return;
        }
        let fade_s = t.fade_frames as f64 / self.device_rate as f64;
        // The blend starts when the *rendered* playhead crosses the plan, not
        // when the audible tail estimate does. The audible figure adds the ring
        // backlog, which moves with how full the device buffer is, so the same
        // plan started on a different beat every time. The next sample this
        // voice will emit is the one that has to land on the downbeat, and it
        // is checked once per chunk, so the error is at most 512 frames.
        let start_s = {
            let end_s = self.outgoing_end_s(&t.plan, current);
            if !end_s.is_finite() {
                return;
            }
            (end_s - fade_s).max(0.0)
        };
        if current.emitted_position_seconds() + 1e-4 < start_s {
            return;
        }
        self.start_fade();
    }

    pub(super) fn start_fade(&mut self) {
        let Some(mut voice) = self.incoming.take() else {
            return;
        };
        // The blend is what the handoff stretch is for, so the stage goes live
        // here rather than at open. It holds this rate until the blend ends;
        // the glide home starts in `finish_transition`.
        if (voice.base_plan_rate - 1.0).abs() > 1e-4 {
            log::info!(
                "handoff stretch engaged: rate={:.4} ({:.0} cents, pitch preserved)",
                voice.base_plan_rate,
                1200.0 * (voice.base_plan_rate.ln() / 2f64.ln()).abs(),
            );
            voice.set_plan_rate(voice.base_plan_rate);
        }
        let info = voice.info.clone();
        log::info!("fade start + handoff: {}", info.title);
        self.duration_ms
            .store((info.duration_seconds * 1000.0) as u64, Ordering::Relaxed);
        // G's open point: a route or loudness-mode change between arm and
        // fade re-arms the match here, while the incoming is still silent, so
        // the first frame is never corrected after it is audible.
        voice.normalization_mode = self.loudness_mode;
        voice.arm_transition_match(
            self.current.as_ref(),
            self.loudness_enabled && self.sound_mode == SoundMode::Enhanced,
        );
        self.incoming = Some(voice);
        let device_rate = self.device_rate;
        let snap_error = self
            .transition
            .as_ref()
            .and_then(|transition| {
                self.current.as_ref().map(|current| {
                    let planned_start = (self.outgoing_end_s(&transition.plan, current)
                        - transition.fade_frames as f64 / device_rate.max(1) as f64)
                        .max(0.0);
                    current.emitted_position_seconds() - planned_start
                })
            })
            .filter(|error| error.is_finite());
        if let Some(t) = &mut self.transition {
            t.phase = Phase::Fading;
            t.handed_off = true;
            t.fx = if t.plan.reason.starts_with("skip-mix:") {
                fx::DeckFx::new_skip_echo(device_rate, t.plan.beat_seconds)
            } else {
                let phrase_window = fx::window_seconds(t.plan.style, t.plan.beat_seconds)
                    .filter(|adaptive| t.plan.effect_seconds > *adaptive + 1e-6)
                    .map(|_| t.plan.effect_seconds);
                fx::DeckFx::new_with_window(
                    t.plan.style,
                    device_rate,
                    t.plan.beat_seconds,
                    phrase_window,
                )
            };
            if let Some(record) = automix_winner_record(&t.plan, snap_error) {
                log::info!(
                    "automix: plan_won={} style={} reason={:?} fade={:.2}s snap_error_s={:?}",
                    record.winner,
                    record.style,
                    record.reason,
                    t.fade_frames as f64 / device_rate.max(1) as f64,
                    record.snap_error_seconds,
                );
            }
            let transition_start = (t.plan.transition_end_seconds
                - t.fade_frames as f64 / device_rate as f64)
                .max(0.0);
            t.effect_start_frame = ((t.plan.effect_start_seconds - transition_start)
                * device_rate as f64)
                .max(0.0) as u64;
            if t.fx.is_some() {
                log::info!(
                    "transition technique {:?} ({:.2}s beat): {}",
                    t.plan.style,
                    t.plan.beat_seconds,
                    t.plan.reason
                );
            }
        }
        // F: arm the loop bridge on the outgoing at the blend start. The unit
        // starts `bridge_start_seconds` on the record's timeline; the served
        // position is the blend's clock, so the delay is in served frames.
        if let Some(t) = &self.transition {
            if t.plan.bridge_seconds > 0.0 && t.plan.bridge_repeats > 0 {
                if let Some(current) = &mut self.current {
                    let delay = ((t.plan.bridge_start_seconds - current.emitted_position_seconds())
                        .max(0.0)
                        * device_rate as f64)
                        .round() as u64;
                    let unit = (t.plan.bridge_seconds * device_rate as f64).round() as u64;
                    current.set_bridge(delay, unit, t.plan.bridge_repeats);
                    log::info!(
                        "bridge: {:.2}s unit at {:.2}s, {} repeat(s), {} frames of delay",
                        t.plan.bridge_seconds,
                        t.plan.bridge_start_seconds,
                        t.plan.bridge_repeats,
                        delay
                    );
                }
            }
        }
        let db = self.incoming.as_ref().and_then(|v| v.loudness_db);
        self.publish_nerd(&info, db);
        // Playhead follows the incoming track from this moment — upstream
        // swaps the session player at handoff. Leaving it on the outgoing
        // voice published a time near that track's end under the new
        // duration, so the next song appeared to start in its outro.
        self.publish_audible_position();
        self.events.handoff(info);
    }

    /// Drives gains + filter ride for the fade in flight. Returns true when
    /// the transition finished this chunk.
    pub(super) fn drive_fade(&mut self) -> bool {
        if !self
            .transition
            .as_ref()
            .is_some_and(|t| t.phase == Phase::Fading)
        {
            return false;
        }
        if let Some(t) = self.transition.as_ref().filter(|t| t.fx.is_some()) {
            // The effect rides the blend: the incoming rises across the whole
            // crossfade while the outgoing falls across the same span (an
            // equal-power pair, so the sum never bumps as the two records add
            // up). The effect only dresses the outgoing's last sub-window.
            let fade = t.fade_frames.max(1);
            let played = self
                .incoming
                .as_ref()
                .map(|voice| voice.emitted_dev_frames)
                .unwrap_or(0);
            let p = (played as f64 / fade as f64).clamp(0.0, 1.0);
            let hold = hold_fraction(&t.plan, fade as f64 / self.device_rate as f64);
            let (rise, fall) = effect_intro_gains(p, hold, effect_intro_db(&t.plan));
            let landing = effect_landing(&t.plan, hold);
            let band = t.plan.filter_sweep;
            if let Some(current) = &mut self.current {
                current.gain = fall;
                current.filter.open();
            }
            if let Some(incoming) = &mut self.incoming {
                incoming.gain = rise;
                ride_effect_intro(p, hold, landing, band, &mut incoming.filter);
            }
            return played >= fade;
        }
        let Some(incoming) = &self.incoming else {
            return false;
        };
        let incoming_cap_frames = incoming
            .audible_remaining_seconds()
            .map(|remaining| {
                let played = incoming.emitted_dev_frames as f64;
                let total_from_cue = played + remaining * self.device_rate as f64;
                ((total_from_cue / 3.0) as u64).max(1)
            })
            .unwrap_or(u64::MAX);
        let emitted = incoming.emitted_dev_frames;
        let fade_frames = self.transition.as_ref().map(|t| t.fade_frames).unwrap_or(0);
        let span = if fade_frames == 0 {
            0
        } else {
            let capped = fade_frames.min(incoming_cap_frames).max(1);
            // The third-of-the-record cap is for a genuinely short song. A cap
            // under a second, against a plan that asked for more, is a partial
            // container duration — and using it is how a blend collapses into
            // a half-second cut.
            if fade_frames > self.device_rate as u64 && capped < self.device_rate as u64 {
                fade_frames
            } else {
                capped
            }
        };
        let p = if span == 0 {
            1.0
        } else {
            (emitted as f64 / span as f64).clamp(0.0, 1.0)
        };
        let plan = self.transition.as_ref().map(|t| (t.plan.clone(), t.swap));
        let swap = plan.as_ref().is_some_and(|(_, swap)| *swap);
        let span_seconds = span as f64 / self.device_rate as f64;
        let hold = plan
            .as_ref()
            .map(|(plan, _)| hold_fraction(plan, span_seconds))
            .unwrap_or(0.0);
        let dj = plan.as_ref().is_some_and(|(plan, _)| {
            !swap
                && plan.bed_fraction > 0.0
                && matches!(
                    plan.style,
                    TransitionStyle::DjBlend | TransitionStyle::DjFilter
                )
        });
        // A swap needs the linear pair. A plain crossfade needs equal-power.
        // A DJ blend holds the outgoing record up and brings the next one in
        // underneath. A cold ending holds the outgoing until its final chord
        // and crosses over the decay. The tempo stretch is *held* here —
        // walking it during the fade is how two matched grids drift into a
        // flam. It glides home after the blend, on the promoted voice.
        let (rise, fall) = if swap {
            fade_gains(p, true)
        } else if dj {
            automix_gains(p, &plan.as_ref().unwrap().0, hold)
        } else if hold > 0.0 {
            cold_end_gains(p, hold)
        } else {
            fade_gains(p, false)
        };
        if let (Some(current), Some(incoming), Some((plan, _))) =
            (&mut self.current, &mut self.incoming, plan)
        {
            current.gain = fall;
            incoming.gain = rise;
            if swap {
                // A source swap is the same recording against itself, so any
                // ride here would be an edit the record never asked for.
                current.filter.open();
                incoming.filter.open();
            } else {
                ride_filters(p, &plan, &mut current.filter, &mut incoming.filter);
            }
        }
        // The outgoing decoder can hit EOF before a late/stale plan reaches
        // its end. That must not finish the transition: doing so promotes the
        // incoming voice at its current gain and makes the outgoing song sound
        // like it was cut mid-fade. Let the incoming ramp reach unity; only
        // abandon it early if the incoming source itself is fully drained.
        let incoming_drained = self.incoming.as_ref().is_some_and(|voice| {
            voice.finished && voice.pending_dev_cursor >= voice.pending_dev.len()
        });
        p >= 1.0 || incoming_drained
    }

    pub(super) fn finish_transition(&mut self) {
        let swap = self.transition.as_ref().is_some_and(|t| t.swap);
        if let Some(t) = &self.transition {
            if t.handed_off {
                if swap {
                    // The one measurement that can only come from a device:
                    // how alike two encodings of one master are. Recorded
                    // rather than asserted, because it is data about the
                    // catalogue, not a property of this code.
                    let correlation = self.swap_probe.correlation();
                    match correlation {
                        Some(rho) => log::info!(
                            "source swap correlation: rho={rho:.4} \
                             (linear-fade deviation from flat: {:.2} dB)",
                            10.0 * ((1.0 + rho) / 2.0).log10()
                        ),
                        None => log::info!(
                            "source swap correlation: unmeasurable (no signal overlapped)"
                        ),
                    }
                    if let Ok(mut nerd) = self.nerd.lock() {
                        nerd.swap_correlation = correlation;
                    }
                    // Condition in-place source swap on Pearson correlation rho >= 0.85
                    // so mismatched recordings/transcodes do not cut over.
                    if let Some(rho) = correlation {
                        if rho < 0.85 {
                            if let Ok(mut nerd) = self.nerd.lock() {
                                nerd.swap_accepted = Some(false);
                            }
                            log::warn!(
                                "source swap rejected: rho={rho:.4} < 0.85 \
                                 (mismatched recording or transcode)"
                            );
                            self.incoming = None;
                            if let Some(current) = &mut self.current {
                                current.gain = 1.0;
                                current.filter.open();
                            }
                            self.transition = None;
                            return;
                        }
                    }
                }
                if let Some(mut incoming) = self.incoming.take() {
                    incoming.gain = 1.0;
                    incoming.release_transition_match();
                    incoming.filter.open();
                    // Hold the matched tempo through the blend, then walk it
                    // home. A swap has nothing to walk, and a plan with no
                    // glide releases here so the stretch cannot outlive the mix.
                    let glide = !swap
                        && incoming.post_glide_seconds > 0.05
                        && (incoming.base_plan_rate - 1.0).abs() > 1e-4;
                    if glide {
                        incoming.begin_tempo_glide(self.device_rate);
                    } else {
                        incoming.release_plan_stretch(self.playback_speed, self.device_rate);
                    }
                    let info = incoming.info.clone();
                    self.current = Some(incoming);
                    if swap {
                        // The readout should name the source that is actually
                        // playing, and that only becomes true here.
                        let db = self.current.as_ref().and_then(|v| v.loudness_db);
                        self.publish_nerd(&info, db);
                        if let Ok(mut nerd) = self.nerd.lock() {
                            nerd.swap_accepted = Some(true);
                        }
                    }
                }
                // Old current dropped; its tail is spent by definition of the
                // fade end.
            } else {
                // Never audible; drop the incoming scratch voice.
                self.incoming = None;
                if let Some(current) = &mut self.current {
                    current.gain = 1.0;
                }
            }
        }
        self.transition = None;
    }
}
