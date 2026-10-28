//! One playing deck: its decoder, resampler, stretcher and per-stream DSP.

use super::bridge::{BridgeStage, VoiceBridge};
use super::curves::db_to_gain;
use super::deck::{
    bound_mix_in_depth, clamp_start_seconds, glided_plan_rate, is_silent, known_duration,
    normalization_gain, silence_exceeds_floor, source_still_growing, user_stretcher,
    MAX_SILENCE_KEEP_SECS, MIN_SILENCE_SECS, MIN_VOLUME_TO_KEEP,
};
use super::types::{TrackInfo, TrackSource};
use super::DeckLoudness;
use crate::decode::resampler::StreamResampler;
use crate::decode::{SourceKind, SymphoniaDecoder};
use crate::sound::LoudnessMode;
use crate::spatial::SpatialRenderer;
use crate::time_stretch::TimeStretch;
use crate::transition_filter::TransitionFilter;

pub(super) const CHUNK_FRAMES: usize = 512;

pub(super) struct Voice {
    pub(super) _source_lease: crate::playback_regions::SourceLease,
    pub(super) head_trim_allowed: bool,
    pub(super) region_fade_in: usize,
    pub(super) regions: crate::PlaybackRegions,
    pub(super) skipped_regions: Vec<bool>,
    pub(super) decoder: SymphoniaDecoder,
    pub(super) resampler: StreamResampler,
    pub(super) user_stretch: Option<TimeStretch>,
    pub(super) spatial: SpatialRenderer,
    pub(super) filter: TransitionFilter,
    pub(super) gain: f32,
    /// The fade gain the last `mix_into` actually reached, so the next call can
    /// ramp from it instead of stepping once per 512-frame chunk.
    pub(super) applied_gain: f32,
    /// Cue offset at open; zeroed by a seek (decoder position then runs from
    /// file start).
    pub(super) base_position: f64,
    pub(super) info: TrackInfo,
    pub(super) finished: bool,
    pub(super) decode_error: Option<String>,
    /// Device-domain frames emitted (post-resample).
    pub(super) emitted_dev_frames: u64,
    /// Rate the post-decode DSP is currently running at; changes on a route
    /// rebuild, so it is a field rather than a constructor argument.
    pub(super) device_rate: u32,
    /// Device-domain samples decoded/resampled but not yet served, plus the
    /// read cursor — pull never returns more than the caller asked for.
    pub(super) pending_dev: Vec<f32>,
    pub(super) pending_dev_cursor: usize,
    pub(super) skip_silence: bool,
    /// Consecutive silent frames in the *device* domain. Dividing by the
    /// source rate (the earlier bug) made the 1 s floor shrink on 48/96 kHz
    /// output — the "make the music sound rushed" failure upstream raised
    /// `MIN_SILENCE_US` to avoid.
    pub(super) silent_dev_frames: u64,
    /// The listener's speed, applied with pitch-preserving stereo WSOLA.
    ///
    /// Kept strictly separate from `plan_rate`: folding the handoff stretch
    /// into this resampler is exactly what made it a pitch bend, and the two
    /// being distinguishable here is what stops that coming back.
    pub(super) effective_speed: f32,
    /// Automix handoff stretch, applied by WSOLA so it moves tempo without
    /// moving pitch. `None` whenever the rate is unity.
    pub(super) stretch: Option<TimeStretch>,
    /// The handoff stretch has come home to unity and the stage is holding
    /// unplaced samples. It is drained into the output on the next pull rather
    /// than dropped, so retiring it is a handover and not a ~12 ms splice.
    pub(super) stretch_draining: bool,
    /// Automix tempo stretch target; drives `stretch` and nothing else.
    pub(super) plan_rate: f64,
    /// The stretch the plan asked for. Held for the blend, then the value the
    /// post-blend glide walks back to unity from.
    pub(super) base_plan_rate: f64,
    /// Post-blend glide length, from the plan. 0 means release at the blend end.
    pub(super) post_glide_seconds: f64,
    /// Output frames the glide spans, and how many have been rendered. Both
    /// zero unless a glide is in progress.
    pub(super) glide_total_frames: u64,
    pub(super) glide_done_frames: u64,
    /// The track length this voice plans against, if known. `None` means the
    /// container declared none and the caller supplied none, and every
    /// transition that would be scheduled against the end of this track is
    /// therefore impossible.
    pub(super) known_duration: Option<f64>,
    /// Catalogue loudness figure for the normalization stage (`None` = no
    /// figure, no correction). The master switch lives on the mixer state, so
    /// this is the measurement only.
    pub(super) loudness_db: Option<f64>,
    pub(super) replay_gain: crate::metadata::ReplayGain,
    pub(super) normalization_mode: LoudnessMode,
    pub(super) measured_loudness: Option<crate::LoudnessMeasurement>,
    pub(super) smoothed_normalization: f32,
    /// The transition's level correction in dB, relative to this deck's own
    /// normalization. Zero except while a blend is matching this deck to the one
    /// it is replacing.
    pub(super) match_db: f32,
    /// The correction actually applied, ramped towards `match_db` so arming and
    /// releasing it are never a step.
    pub(super) smoothed_match: f32,
    /// True while the correction is being walked back after the blend, which
    /// uses a slower ramp than arming it does.
    pub(super) match_releasing: bool,
    /// Bypass all source/device resampling and voice DSP when rates match.
    pub(super) bit_perfect: bool,
    /// F's loop bridge: one measured, vocal-free unit of the outro captured as
    /// it plays and replayed at the same point in the fade. `None` unless the
    /// planner asked for one and the blend has started.
    pub(super) bridge: Option<VoiceBridge>,
}

impl Voice {
    /// `cue` marks a fresh start — a track being cued or mixed in — which the
    /// 5-second clearance rule may pull back to the top rather than let it open
    /// inside the outro. A source swap continues a track that is already
    /// playing, so its position is where the listener actually is and nothing
    /// may move it.
    pub(super) fn open(
        request: &TrackSource,
        spatial_enabled: bool,
        head_yaw: f32,
        device_rate: u32,
        playback_speed: f32,
        skip_silence: bool,
        cue: bool,
    ) -> Result<Voice, String> {
        let source_lease = crate::playback_regions::SourceLease::new(&request.source);
        let kind = SourceKind::parse(&request.source);
        let mut decoder =
            SymphoniaDecoder::open(&kind, &request.headers).map_err(|e| e.to_string())?;
        let src_rate = decoder.sample_rate();
        crate::diagnostics::source(&request.source, src_rate);
        // Spatial + transition filter run in the device domain, after
        // resample — same 15 ms Haas window the DAC hears, and clip
        // harmonics never hit the sinc. Upstream's processor is also at
        // the sink rate; an earlier port ran this on source PCM and then
        // resampled the hard-clipped output, which rang.
        let mut spatial = SpatialRenderer::new(device_rate);
        spatial.set_enabled(spatial_enabled && decoder.channels() >= 2);
        spatial.set_head_yaw(head_yaw);
        let decoder_duration = decoder.duration_seconds();
        let mut duration = known_duration(decoder_duration, request.duration_seconds);
        // A download still in progress reports the container length of the
        // bytes on disk. Scheduling the blend against that starts the next
        // song in the middle of this one, over whatever fragment has arrived.
        // The catalogue figure is the length the blend has to land on.
        if source_still_growing(&request.source) && request.duration_seconds > duration + 1.0 {
            duration = request.duration_seconds;
        }
        let regions = crate::playback_regions::get(&request.source).normalized(duration);
        let start = if cue {
            let cleared = clamp_start_seconds(request.start_seconds, duration);
            bound_mix_in_depth(cleared, &request.plan, duration)
        } else {
            request.start_seconds.max(0.0)
        };
        let start = if request.start_seconds <= 0.0 || cue {
            regions.next_allowed(start)
        } else {
            start
        };
        if start > 0.0 {
            decoder.seek_seconds(start).map_err(|e| e.to_string())?;
        }
        log::info!(
            "voice opened: title={:?} requested_start={:.3}s clamped_start={:.3}s actual_start={:.3}s \
             duration={:.1}s (container={:?} declared={:.1}s) source_rate={} device_rate={}",
            request.title,
            request.start_seconds,
            start,
            decoder.position_seconds(),
            duration,
            decoder_duration,
            request.duration_seconds,
            src_rate,
            device_rate,
        );
        let plan_rate = if request.plan.playback_rate > 0.05 {
            request.plan.playback_rate
        } else {
            1.0
        };
        // The handoff stretch is *not* engaged here. It exists to hold two
        // records on one grid for the length of a blend, and `start_fade` is
        // where a blend begins — a `Load` (a tap on the queue, a skip) carries
        // the same plan record but has no blend to serve, and opening the stage
        // on it would leave a track playing 3 % slow for its whole length with
        // nothing to release it. `plan_rate` is recorded either way so the
        // glide has a number to walk from.
        let effective = (playback_speed as f64).clamp(0.5, 2.0) as f32;
        Ok(Voice {
            _source_lease: source_lease,
            head_trim_allowed: request.start_seconds <= 0.0 || cue,
            region_fade_in: 0,
            skipped_regions: vec![false; regions.excluded.len()],
            regions,
            resampler: StreamResampler::stereo(src_rate, device_rate),
            user_stretch: user_stretcher(device_rate, effective),
            spatial,
            filter: TransitionFilter::new(2, device_rate),
            gain: 1.0,
            applied_gain: 1.0,
            base_position: 0.0,
            info: TrackInfo {
                title: request.title.clone(),
                artist: request.artist.clone(),
                source: request.source.clone(),
                duration_seconds: duration,
                codec: decoder.codec().to_string(),
                sample_rate: src_rate,
                bit_depth: decoder.bit_depth(),
                channels: decoder.channels() as u32,
                kbps: request.claimed_kbps,
            },
            decoder,
            finished: false,
            decode_error: None,
            emitted_dev_frames: 0,
            device_rate,
            pending_dev: Vec::new(),
            pending_dev_cursor: 0,
            skip_silence,
            silent_dev_frames: 0,
            effective_speed: effective,
            stretch: None,
            stretch_draining: false,
            plan_rate,
            base_plan_rate: plan_rate,
            post_glide_seconds: request.plan.post_glide_seconds.max(0.0),
            glide_total_frames: 0,
            glide_done_frames: 0,
            known_duration: (duration > 0.0).then_some(duration),
            loudness_db: request.loudness_db,
            replay_gain: if let SourceKind::Path(path) = &kind {
                crate::metadata::read_replay_gain(path)
            } else {
                crate::metadata::ReplayGain::default()
            },
            normalization_mode: LoudnessMode::Off,
            measured_loudness: None,
            smoothed_normalization: 1.0,
            match_db: 0.0,
            smoothed_match: 1.0,
            match_releasing: false,
            bit_perfect: false,
            bridge: None,
        })
    }

    /// What this deck's integrated loudness says, for the transition match, and
    /// what normalization is already doing about it.
    pub(super) fn deck_loudness(&self, normalization: bool) -> DeckLoudness {
        DeckLoudness {
            lufs: self.measured_loudness.as_ref().map(|m| m.track_lufs),
            relative_db: self.loudness_db,
            applied_normalization_db: normalization_gain(self, normalization)
                .1
                .map(|db| db as f64),
            true_peak_dbtp: self.measured_loudness.as_ref().and_then(|m| m.true_peak_dbtp),
        }
    }

    /// Matches this deck's level to the one it is fading in over.
    ///
    /// Armed when the blend is armed, and the correction is **applied** here
    /// rather than ramped in during the first 0.4 s of the fade: the owner's
    /// "the next song blasts, then corrects" is exactly that late ramp. The
    /// fade gain is zero at the first audible frame and the incoming's own
    /// normalization is settled at the same time, so setting the targets here
    /// has no step to click — there is nothing audible yet to step.
    pub(super) fn arm_transition_match(&mut self, outgoing: Option<&Voice>, normalization: bool) {
        // The incoming's own normalization also ramps for 50 ms inside
        // `mix_into`; the first audible frame must already carry it.
        self.smoothed_normalization = normalization_gain(self, normalization).0;
        let db = outgoing.and_then(|outgoing| {
            DeckLoudness::match_gain_db(
                self.deck_loudness(normalization),
                outgoing.deck_loudness(normalization),
            )
        });
        match db.filter(|db| db.abs() >= 0.05) {
            Some(db) => {
                if (db as f32 - self.match_db).abs() >= 0.05 || self.match_releasing {
                    log::info!(
                        "transition loudness match: {db:+.1} dB (applied before the first frame)"
                    );
                }
                self.match_db = db as f32;
                self.smoothed_match = db_to_gain(db) as f32;
                self.match_releasing = false;
            }
            None => {
                if self.match_db.abs() >= 0.05 {
                    log::info!("transition loudness match: cleared before the first frame");
                }
                self.match_db = 0.0;
                self.smoothed_match = 1.0;
                self.match_releasing = false;
            }
        }
    }

    /// Walks the correction back to none after the blend.
    ///
    /// The incoming deck keeps its normalization for the rest of the track; the
    /// match only existed to make the join level. Releasing it slowly means the
    /// join has no step in it either.
    pub(super) fn release_transition_match(&mut self) {
        if self.match_db.abs() < 1e-4 {
            return;
        }
        self.match_db = 0.0;
        self.match_releasing = true;
    }

    pub(super) fn mix_into(
        &mut self,
        samples: &[f32],
        destination: &mut [f32],
        fade_gain: f32,
        normalization: bool,
    ) {
        let target = normalization_gain(self, normalization).0;
        let alpha = 1.0 - (-1.0 / (self.device_rate as f32 * 0.050)).exp();
        // 0.4 s to put the match in place, 1.5 s to take it away: a boost that
        // appeared instantly would click, and one that vanished instantly would
        // be a step at the end of the mix.
        let match_seconds = if self.match_releasing { 1.5 } else { 0.4 };
        let alpha_match = 1.0 - (-1.0 / (self.device_rate as f32 * match_seconds)).exp();
        let target_match = db_to_gain(self.match_db as f64) as f32;
        // The caller recomputes the fade target once per 512-frame chunk, so
        // applying it flat makes a slow crossing a staircase (a zipper at the
        // low end). Walk from the previous chunk's applied gain to this one
        // across the samples; the next call starts where this one ended.
        let start_gain = self.applied_gain;
        self.applied_gain = fade_gain;
        let frames = (samples.len() / 2).max(1) as f32;
        for (index, (source, mix)) in samples
            .chunks_exact(2)
            .zip(destination.chunks_exact_mut(2))
            .enumerate()
        {
            let progress = index as f32 / (frames - 1.0).max(1.0);
            let fade = start_gain + (fade_gain - start_gain) * progress;
            self.smoothed_normalization += (target - self.smoothed_normalization) * alpha;
            if (target - self.smoothed_normalization).abs() < 1e-6 {
                self.smoothed_normalization = target;
            }
            self.smoothed_match += (target_match - self.smoothed_match) * alpha_match;
            let gain = fade * self.smoothed_normalization * self.smoothed_match;
            mix[0] += source[0] * gain;
            mix[1] += source[1] * gain;
        }
    }

    pub(super) fn set_playback_speed(&mut self, speed: f32, device_rate: u32) {
        let effective = (speed as f64).clamp(0.5, 2.0) as f32;
        if (effective - self.effective_speed).abs() < 0.001 {
            return;
        }
        self.effective_speed = effective;
        if let Some(stretch) = &mut self.user_stretch {
            stretch.set_rate(effective as f64);
        } else {
            self.user_stretch = user_stretcher(device_rate, effective);
        }
    }

    /// Moves the automix handoff stretch to `rate`.
    ///
    /// Engages the WSOLA stage on the way up and retires it on the way back to
    /// unity. Retiring is a flush-and-drop rather than a bypass: at a rate of
    /// exactly 1.0 the stage reconstructs its input sample for sample, so the
    /// drained tail meets the pass-through with nothing to hear — which is what
    /// lets the glide land on unity at the end of a blend and leave nothing
    /// behind but the audio.
    pub(super) fn set_plan_rate(&mut self, rate: f64) {
        let rate = if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            1.0
        };
        self.plan_rate = rate;
        if (rate - 1.0).abs() <= 1e-4 {
            // Hold the stage until the next pull drains it.
            self.stretch_draining = self.stretch.is_some();
        } else {
            self.stretch_draining = false;
            if let Some(stretch) = self.stretch.as_mut() {
                stretch.set_rate(rate);
            } else {
                self.stretch = Some(TimeStretch::with_sample_rate(rate, self.device_rate));
            }
        }
    }

    /// The natural end of a stretched stream: drain the stage into the pending
    /// buffer and drop it. Returns the drained samples so the caller can splice
    /// them in ahead of whatever comes next.
    pub(super) fn drain_stretch(&mut self) -> Vec<f32> {
        match self.stretch.as_mut() {
            Some(stretch) => {
                stretch.flush();
                let tail = stretch.take();
                self.stretch = None;
                log::info!("handoff stretch retired: {}", self.info.title);
                tail
            }
            None => Vec::new(),
        }
    }

    /// Releases the tempo stretch a beatmatched handoff stacked on this voice.
    ///
    /// The stretch exists only to hold two tracks on a shared grid for the
    /// length of the blend. Upstream undoes it unconditionally and idempotently
    /// the moment the fade ends — `CrossfadeController.finish` and `retire`
    /// both call `setPlaybackSpeed(AppSettings.playbackSpeed.value)` — because a
    /// voice that keeps it plays fast for its whole length: +3 % tempo and +51
    /// cents on every track that ever arrived through a beatmatched blend, with
    /// the published position running at the stretched rate as well.
    ///
    /// The guarantee for a voice that never got to glide: a skip, a bail, a
    /// cut at end-of-stream. Snaps the rate to unity and drains the stage on
    /// the next pull. A finished blend does not come here first — it calls
    /// [`Voice::begin_tempo_glide`] and this runs when that glide lands.
    pub(super) fn release_plan_stretch(&mut self, speed: f32, device_rate: u32) {
        self.glide_total_frames = 0;
        self.glide_done_frames = 0;
        if self.stretch.is_none()
            && (self.plan_rate - 1.0).abs() < f64::EPSILON
            && !self.stretch_draining
        {
            return;
        }
        // Clear the multiplier *before* re-deriving, or `set_playback_speed`
        // would simply stack the stretch back on.
        self.plan_rate = 1.0;
        self.stretch_draining = self.stretch.is_some();
        self.set_playback_speed(speed, device_rate);
    }

    /// Starts the post-blend walk from the matched tempo back to the record's
    /// own. No-op when the plan asked for none, in which case the caller
    /// releases immediately.
    pub(super) fn begin_tempo_glide(&mut self, device_rate: u32) {
        if self.post_glide_seconds <= 0.05 || (self.base_plan_rate - 1.0).abs() < 1e-4 {
            return;
        }
        let frames = (self.post_glide_seconds * device_rate.max(1) as f64)
            .round()
            .max(1.0) as u64;
        self.glide_total_frames = frames;
        self.glide_done_frames = 0;
        log::info!(
            "handoff tempo glide: {} duration={:.2}s",
            self.info.title,
            self.post_glide_seconds
        );
        self.set_plan_rate(self.base_plan_rate);
    }

    /// Sets the stretch to where the glide has reached. Call once per rendered
    /// chunk, before the pull, then [`Voice::note_glide_frames`] with what
    /// came out.
    pub(super) fn drive_tempo_glide(&mut self, device_rate: u32, speed: f32) {
        if self.glide_total_frames == 0 {
            return;
        }
        let p = (self.glide_done_frames as f64 / self.glide_total_frames as f64).clamp(0.0, 1.0);
        if p >= 1.0 {
            self.release_plan_stretch(speed, device_rate);
            return;
        }
        let target = glided_plan_rate(self.base_plan_rate, p);
        if (target - self.plan_rate).abs() >= 1e-5 {
            self.set_plan_rate(target);
        }
    }

    pub(super) fn note_glide_frames(&mut self, frames: u64) {
        if self.glide_total_frames > 0 {
            self.glide_done_frames = self.glide_done_frames.saturating_add(frames);
        }
    }

    /// Rebuild post-decode DSP for a new DAC rate (AirPods connect/disconnect).
    pub(super) fn retarget_device(
        &mut self,
        device_rate: u32,
        spatial_enabled: bool,
        head_yaw: f32,
    ) {
        let src = self.info.sample_rate.max(1);
        self.device_rate = device_rate;
        let enabled = self.spatial.enabled();
        self.resampler = StreamResampler::stereo(src, device_rate);
        self.user_stretch = user_stretcher(device_rate, self.effective_speed);
        self.stretch = None;
        self.stretch_draining = false;
        let mut spatial = SpatialRenderer::new(device_rate);
        spatial.set_enabled(enabled && spatial_enabled && self.info.channels >= 2);
        spatial.set_head_yaw(head_yaw);
        self.spatial = spatial;
        self.filter = TransitionFilter::new(2, device_rate);
        self.pending_dev.clear();
        self.pending_dev_cursor = 0;
    }

    pub(super) fn reset_processing(&mut self) {
        self.region_fade_in = 0;
        self.resampler.reset();
        self.user_stretch = user_stretcher(self.device_rate, self.effective_speed);
        self.stretch = None;
        self.stretch_draining = false;
        self.plan_rate = 1.0;
        self.glide_total_frames = 0;
        self.glide_done_frames = 0;
        self.spatial.flush();
        self.filter.flush();
        self.pending_dev.clear();
        self.pending_dev_cursor = 0;
        self.silent_dev_frames = 0;
        self.finished = false;
    }

    pub(super) fn position_seconds(&self) -> f64 {
        let decoder = self.base_position + self.decoder.position_seconds();
        // The WSOLA stage holds roughly a frame of input it has not placed yet,
        // so the decoder runs that far ahead of the audio the listener is
        // hearing. Bounded at `FRAME + SEARCH` — about 12 ms — and only while a
        // handoff stretch is running, but a playhead that leads the sound is
        // the same bug as one that trails it.
        (decoder
            - self.stretch_latency_seconds()
            - self.resampler.delay_frames() / self.device_rate as f64
            - self
                .user_stretch
                .as_ref()
                .map(|s| s.latency_frames() / self.device_rate as f64)
                .unwrap_or(0.0))
        .max(0.0)
    }

    /// How far ahead of the output the decoder is running, in source seconds.
    pub(super) fn stretch_latency_seconds(&self) -> f64 {
        self.stretch
            .as_ref()
            .map(|stretch| stretch.latency_frames() / self.device_rate.max(1) as f64)
            .unwrap_or(0.0)
    }

    /// Where this voice's *next served sample* sits on the source timeline.
    ///
    /// [`position_seconds`] is the decoder's read head, which runs ahead of the
    /// output by whatever `pending_dev` is holding. Opening a replacement
    /// source at that figure would leave it up to a full decode batch — around
    /// 190 ms at 22.05 kHz — behind the voice it is replacing, and a
    /// same-timeline crossfade that far out of alignment is heard as a doubled
    /// voice rather than as a quality change. Resampling preserves time, so the
    /// un-served device tail converts straight back to source seconds; the
    /// playback-speed factor maps it through the speed resampler.
    pub(super) fn emitted_position_seconds(&self) -> f64 {
        let pending_frames = (self
            .pending_dev
            .len()
            .saturating_sub(self.pending_dev_cursor)
            / 2) as f64;
        let pending_seconds = pending_frames / self.device_rate.max(1) as f64
            * self.effective_speed as f64
            * self.plan_rate as f64;
        (self.position_seconds() - pending_seconds).max(0.0)
    }

    /// Remaining source-domain seconds the decoder still holds, if known.
    ///
    /// `info.duration_seconds` rather than the decoder's own figure, so a
    /// container that declares no length still schedules: `remaining_seconds`
    /// is what the arming and the fade start are both measured against, and
    /// `None` here means "there is no end to schedule against", which is a cut,
    /// not a blend.
    pub(super) fn remaining_seconds(&self) -> Option<f64> {
        let total = self.regions.effective_end(self.known_duration?);
        Some((total - self.decoder.position_seconds()).max(0.0))
    }

    /// Remaining audible seconds from the next served sample, so a pre-rolled
    /// buffer counts as content ahead of the playhead rather than as already
    /// played.
    pub(super) fn audible_remaining_seconds(&self) -> Option<f64> {
        let total = self.regions.effective_end(self.known_duration?);
        Some((total - self.emitted_position_seconds()).max(0.0))
    }

    /// Decode ahead without advancing the audible timeline. Existing buffered
    /// frames are retained, including any resampler tail beyond the preview.
    pub(super) fn buffer_preview(&mut self, frames: usize, device_rate: u32) {
        let emitted = self.emitted_dev_frames;
        let preview = self.pull(frames, device_rate);
        let rest = self.pending_dev.split_off(self.pending_dev_cursor);
        self.pending_dev = preview;
        self.pending_dev.extend_from_slice(&rest);
        self.pending_dev_cursor = 0;
        self.emitted_dev_frames = emitted;
    }

    /// Decode the first `frames` of an armed incoming into `pending_dev`
    /// without emitting them. The decoder, resampler and stretch are primed,
    /// and the buffer is what `pull` serves when the blend starts — so the
    /// first fade chunk already holds the record's own cue instead of paying
    /// for a cold decode while the outgoing has begun to fall.
    ///
    /// The audible timeline does not move: `emitted_position_seconds`
    /// subtracts the buffer, and `emitted_dev_frames` is reset because none of
    /// it has been heard.
    pub(super) fn prime_for_fade(&mut self, frames: usize, device_rate: u32) {
        if frames == 0 || self.finished || !self.pending_dev.is_empty() {
            return;
        }
        // The handoff stretch is engaged at the fade normally; engage it here
        // so the pre-rolled samples are at the same rate the blend will use.
        let planned_rate = self.base_plan_rate;
        if (planned_rate - 1.0).abs() > 1e-4 {
            self.set_plan_rate(planned_rate);
        }
        let prefetched = self.pull(frames, device_rate);
        if prefetched.is_empty() {
            return;
        }
        let rest = self.pending_dev.split_off(self.pending_dev_cursor);
        self.pending_dev = prefetched;
        self.pending_dev.extend_from_slice(&rest);
        self.pending_dev_cursor = 0;
        self.emitted_dev_frames = 0;
        // A head-trim seek during the pull resets the processing chain; keep
        // the planned rate if that happened.
        if (planned_rate - 1.0).abs() > 1e-4 && (self.plan_rate - planned_rate).abs() > 1e-9 {
            self.set_plan_rate(planned_rate);
        }
    }

    /// Pulls up to `frames` device-domain interleaved stereo frames.
    /// Pulls up to `frames` device-domain interleaved stereo frames. Never
    /// returns more than asked: decoded batches overshooting the request are
    /// stashed in `pending_dev` and served on later calls.
    /// Serves the next chunk, running F's loop bridge when one is armed.
    ///
    /// While the bridge replays, the decoder is deliberately not pulled: the
    /// music after the captured unit stays in the decoder, so the unit can play
    /// again and the record then continues sample-continuously. The served
    /// frame count (`emitted_dev_frames`) still advances for the replay, so the
    /// blend's clock keeps moving.
    pub(super) fn pull(&mut self, frames: usize, device_rate: u32) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * 2);
        self.pull_into(frames, device_rate, &mut out);
        out
    }

    /// Pull into caller-owned storage so the mixer can reuse its output buffer.
    pub(super) fn pull_into(&mut self, frames: usize, device_rate: u32, out: &mut Vec<f32>) {
        out.clear();
        if out.capacity() < frames * 2 {
            out.reserve(frames * 2);
        }
        if let Some(bridge) = self.bridge.as_mut() {
            if bridge.stage == BridgeStage::Replaying {
                bridge.replay_into(frames, device_rate, out);
                self.emitted_dev_frames += (out.len() / 2) as u64;
                if bridge.stage == BridgeStage::Done && out.len() < frames * 2 {
                    self.pull_decoded_into((frames * 2 - out.len()) / 2, device_rate, out);
                }
                return;
            }
        }
        self.pull_decoded_into(frames, device_rate, out);
        if let Some(mut bridge) = self.bridge.take() {
            let held = bridge.after_pull(out, &mut self.pending_dev, &mut self.pending_dev_cursor);
            if held > 0 {
                self.emitted_dev_frames = self.emitted_dev_frames.saturating_sub(held as u64);
            }
            self.bridge = Some(bridge);
        }
    }

    /// Arms the bridge at the blend: `delay_frames` are served before the unit
    /// starts, the unit is `unit_frames` long, and it is replayed
    /// `repeats` times. Bit-perfect playback has no blend, so it never arms.
    pub(super) fn set_bridge(&mut self, delay_frames: u64, unit_frames: u64, repeats: u32) {
        if unit_frames == 0 || self.bit_perfect {
            return;
        }
        self.bridge = Some(VoiceBridge::new(delay_frames, unit_frames, repeats));
    }

    fn pull_decoded_into(&mut self, frames: usize, device_rate: u32, out: &mut Vec<f32>) {
        if self.bit_perfect
            && device_rate == self.info.sample_rate
            && self.regions.excluded.is_empty()
            && self.regions.audible_start_seconds == 0.0
            && self.regions.audible_end_seconds.is_none()
        {
            self.pull_bitperfect_into(frames, out);
            return;
        }
        let want = out.len() + (frames * 2).max(2);
        if out.capacity() < want {
            out.reserve(want - out.len());
        }

        let serve_pending = |out: &mut Vec<f32>, voice: &mut Voice| {
            let remaining = want - out.len();
            if remaining == 0 || voice.pending_dev_cursor >= voice.pending_dev.len() {
                return;
            }
            let take = (voice.pending_dev.len() - voice.pending_dev_cursor).min(remaining);
            out.extend_from_slice(
                &voice.pending_dev[voice.pending_dev_cursor..voice.pending_dev_cursor + take],
            );
            voice.pending_dev_cursor += take;
            voice.emitted_dev_frames += (take / 2) as u64;
        };
        serve_pending(out, self);
        if out.len() >= want {
            // Keep any decoded backlog ahead of the stretcher's held tail.
            // Draining now could overwrite PCM that did not fit this pull.
            return;
        }

        // The handoff stretch has landed on unity: flush the stage and serve
        // what it was still holding before decoding anything more, so the
        // handover to the plain path is sample-continuous.
        if self.stretch_draining {
            self.stretch_draining = false;
            let tail = self.drain_stretch();
            if !tail.is_empty() {
                let remaining = want - out.len();
                let take = tail.len().min(remaining);
                out.extend_from_slice(&tail[..take]);
                self.emitted_dev_frames += (take / 2) as u64;
                if take < tail.len() {
                    self.pending_dev = tail;
                    self.pending_dev_cursor = take;
                }
                if out.len() >= want {
                    return;
                }
            }
        }

        while out.len() < want && !self.finished {
            let at = self.decoder.position_seconds();
            if self.head_trim_allowed && at < self.regions.audible_start_seconds {
                // Seek once, then let read_stereo consume compressed preroll.
                // Repeating a seek before reading can never advance the codec
                // and can also loop on a boundary between source samples.
                self.head_trim_allowed = false;
                if self
                    .decoder
                    .seek_seconds(self.regions.next_allowed(at))
                    .is_ok()
                {
                    self.reset_processing();
                    continue;
                }
            }
            let end = self
                .regions
                .effective_end(self.known_duration.unwrap_or(0.0));
            let reached_end = end > 0.0 && at >= end;
            if let Some((index, segment)) =
                self.regions.excluded.iter().enumerate().find(|(i, s)| {
                    !self.skipped_regions[*i]
                        && at >= s.start_seconds - 0.0001
                        && at < s.end_seconds
                })
            {
                let target = segment.end_seconds;
                self.skipped_regions[index] = true;
                match self.decoder.seek_seconds(target) {
                    Ok(()) => {
                        self.reset_processing();
                        self.region_fade_in = (self.info.sample_rate as usize / 200).max(1);
                        log::info!("non-music segment skipped {:.3}..{:.3}", at, target);
                    }
                    Err(error) => {
                        log::warn!("segment seek failed: {error}");
                    }
                }
                continue;
            }
            let mut until = if end > 0.0 { end } else { f64::INFINITY };
            for (i, s) in self.regions.excluded.iter().enumerate() {
                if !self.skipped_regions[i] && s.start_seconds > at {
                    until = until.min(s.start_seconds);
                }
            }
            let frames = (((until - at) * self.info.sample_rate as f64).ceil() as usize)
                .max(1)
                .min(crate::diagnostics::chunk());
            let mut src = if reached_end {
                Vec::new()
            } else {
                self.decoder.read_stereo(frames).unwrap_or_else(|e| {
                    self.decode_error = Some(format!("decode failed: {e}"));
                    Vec::new()
                })
            };
            // Five-millisecond ramps around a policy cut avoid introducing a
            // discontinuity while leaving the source timeline untouched.
            let ramp_frames = (self.info.sample_rate as usize / 200).max(1);
            let soften_cut = self
                .regions
                .excluded
                .iter()
                .any(|s| s.start_seconds == until)
                || self.regions.audible_end_seconds == Some(until);
            if self.region_fade_in > 0 || soften_cut {
                for (i, frame) in src.chunks_exact_mut(2).enumerate() {
                    let incoming_gain = if self.region_fade_in > 0 {
                        let gain = 1.0 - self.region_fade_in as f32 / ramp_frames as f32;
                        self.region_fade_in -= 1;
                        gain
                    } else {
                        1.0
                    };
                    let to_cut = until - (at + i as f64 / self.info.sample_rate as f64);
                    let outgoing_gain = if soften_cut {
                        (to_cut / 0.005).clamp(0.0, 1.0) as f32
                    } else {
                        1.0
                    };
                    frame[0] *= incoming_gain * outgoing_gain;
                    frame[1] *= incoming_gain * outgoing_gain;
                }
            }
            if src.is_empty() {
                self.finished = true;
                self.pending_dev = self.resampler.flush();
                self.pending_dev_cursor = 0;
                if let Some(stretch) = &mut self.user_stretch {
                    let mut tail = stretch.process_and_take(&self.pending_dev);
                    stretch.flush();
                    tail.extend(stretch.take());
                    self.pending_dev = tail;
                }
                // A stretch still running at end of stream gets flushed, or the
                // last few milliseconds of the record are simply gone. The
                // resampler tail goes *through* it rather than being pasted onto
                // its output, so the two arrive in the order the chain actually
                // produced them.
                if self.stretch.is_some() {
                    let input = core::mem::take(&mut self.pending_dev);
                    let mut tail = self
                        .stretch
                        .as_mut()
                        .expect("checked above")
                        .process_and_take(&input);
                    let mut rest = self.drain_stretch();
                    tail.append(&mut rest);
                    self.pending_dev = tail;
                }
                if self.spatial.enabled() || self.filter.active() {
                    self.pending_dev
                        .extend(vec![0.0; self.device_rate as usize]);
                }
                // Device-domain DSP (spatial, then transition filter) — same
                // order as upstream's sink processors, at the rate the DAC hears.
                self.spatial.process(&mut self.pending_dev);
                self.filter.process(&mut self.pending_dev);
                serve_pending(out, self);
                return;
            }
            crate::diagnostics::capture_voice(0, &self.info.source, &src);
            self.pending_dev = self.resampler.process(&src);
            self.pending_dev_cursor = 0;
            if let Some(stretch) = &mut self.user_stretch {
                self.pending_dev = stretch.process_and_take(&self.pending_dev);
            }
            // The handoff stretch runs here: in the device domain, after the
            // listener's pitch-preserving speed and before
            // the spatial and transition stages — so the blend's EQ is shaping
            // the stretched signal rather than being stretched itself, and a
            // filter sweep lands on the beat it was aimed at.
            if self.stretch.is_some() {
                let input = core::mem::take(&mut self.pending_dev);
                self.pending_dev = self
                    .stretch
                    .as_mut()
                    .expect("checked above")
                    .process_and_take(&input);
            }
            // Classify silence on the dry buffer. Spatial's 0.82 makeup
            // would otherwise pull quiet music under the threshold whenever
            // widening is on. DSP still runs so the delay line stays
            // continuous across a drop.
            let silent = self.skip_silence && is_silent(&self.pending_dev);
            self.spatial.process(&mut self.pending_dev);
            self.filter.process(&mut self.pending_dev);
            crate::diagnostics::capture_voice(1, &self.info.source, &self.pending_dev);
            if silent {
                self.silent_dev_frames += (self.pending_dev.len() / 2) as u64;
                // Upstream MIN_SILENCE_US = 1 s: keep the first second so a
                // breath still lands, then trim the rest of the gap. Upstream
                // keeps a 20 % sliver (capped at 2 s) at 10 % volume rather
                // than cutting to true digital silence, so a skipped gap still
                // reads as a gap and never clicks.
                if silence_exceeds_floor(self.silent_dev_frames, device_rate) {
                    let silent_s = self.silent_dev_frames as f64 / device_rate as f64;
                    if silent_s <= MIN_SILENCE_SECS + MAX_SILENCE_KEEP_SECS {
                        // Keep every 5th frame (20 % retention) at −20 dB.
                        let mut kept: Vec<f32> = Vec::with_capacity(self.pending_dev.len() / 5 + 2);
                        for frame in self.pending_dev.chunks_exact(2).step_by(5) {
                            kept.push(frame[0] * MIN_VOLUME_TO_KEEP);
                            kept.push(frame[1] * MIN_VOLUME_TO_KEEP);
                        }
                        self.pending_dev = kept;
                        self.pending_dev_cursor = 0;
                    } else {
                        self.pending_dev.clear();
                        self.pending_dev_cursor = 0;
                        continue;
                    }
                }
            } else {
                self.silent_dev_frames = 0;
            }
            serve_pending(out, self);
        }
    }

    /// Reads source frames directly. This branch is only used for stereo
    /// lossless PCM with the DAC clock set to the source rate; it deliberately
    /// avoids the resamplers, silence scan, spatial stage, and transition filter.
    fn pull_bitperfect_into(&mut self, _frames: usize, out: &mut Vec<f32>) {
        if self.finished {
            return;
        }
        match self.decoder.read_stereo(_frames) {
            Ok(samples) => {
                if samples.is_empty() {
                    self.finished = true;
                } else {
                    self.emitted_dev_frames += (samples.len() / 2) as u64;
                }
                out.extend_from_slice(&samples)
            }
            Err(error) => {
                log::warn!("bit-perfect source read failed: {error}");
                self.finished = true;
            }
        }
    }
}
