//! The mixer thread's state: its voices, the transition in flight, and the
//! promotions, swaps and cuts between decks.

use super::types::{EngineEvents, TrackInfo, TrackSource, TransitionPlan};
use super::voice::Voice;
use super::{fx, loudness_gain, ARM_LEAD_MS, SWAP_CROSSFADE_MS};
use crate::eq::EqualizerProcessor;
use crate::sound::{Clarity, ClarityTuning, LoudnessMode, Protector, SoundMode};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(PartialEq, Clone, Copy)]
#[allow(dead_code)]
pub(super) enum Phase {
    Idle,
    Arming,
    Fading,
}

pub(super) struct TransitionState {
    pub(super) phase: Phase,
    pub(super) fade_frames: u64,
    pub(super) handed_off: bool,
    pub(super) plan: TransitionPlan,
    pub(super) arm_deadline: Instant,
    /// A same-track source swap (quality upgrade / alternate version) rather
    /// than a queue advance. Upstream fades these with a plain equal-power
    /// curve and no filter ride — filtering a track against itself would edit
    /// the record — and never reports a track change for them.
    pub(super) swap: bool,
    /// The outgoing deck's effect, for the techniques that finish a record
    /// with one. Created when the window opens.
    pub(super) fx: Option<fx::DeckFx>,
    /// When inside the transition the effect sub-window starts, in frames from
    /// the blend start. Before it the outgoing plays dry on the blend's own
    /// fall; after it the effect dresses that fall.
    pub(super) effect_start_frame: u64,
    /// The outgoing's level when its effect began, held for the rest of the
    /// window: the technique ramps its own dry out and rings its own tail,
    /// so feeding it the blend's still-falling gain fed an echo or a loop
    /// almost nothing, and the effect was 20–40 dB under the mix.
    pub(super) effect_input_gain: Option<f32>,
}

pub(super) struct RetiringVoice {
    pub(super) voice: Voice,
    pub(super) from_gain: f32,
    pub(super) started_at: Instant,
}

pub(super) struct MixerState {
    pub(super) current: Option<Voice>,
    pub(super) incoming: Option<Voice>,
    pub(super) pending_next: Option<TrackSource>,
    /// Latched once a transition has been found unschedulable, so the warning
    /// about it is one line per track rather than one per chunk.
    pub(super) warned_no_duration: bool,
    pub(super) transition: Option<TransitionState>,
    pub(super) retiring: Vec<RetiringVoice>,
    /// Reusable storage for one mixer chunk and the decks feeding it.
    pub(super) render_scratch: Vec<f32>,
    pub(super) voice_output_scratch: Vec<f32>,
    pub(super) effect_dry_scratch: Vec<f32>,
    /// Spare allocation used while retiring voices are filtered in render order.
    pub(super) retiring_scratch: Vec<RetiringVoice>,
    pub(super) playing: bool,
    pub(super) bit_perfect: bool,
    pub(super) volume: f32,
    pub(super) sleep_gain: f32,
    pub(super) hold_end: bool,
    pub(super) crossfade_window_s: f64,
    pub(super) spatial_enabled: bool,
    pub(super) head_yaw: f32,
    pub(super) device_rate: u32,
    /// Frames pushed to the ring minus frames the callback consumed. The
    /// render loop needs this to know how much audio is still *audible* (ring
    /// backlog counts toward a track's tail).
    pub(super) buffered_frames: Arc<AtomicU64>,
    pub(super) position_ms: Arc<AtomicU64>,
    pub(super) duration_ms: Arc<AtomicU64>,
    pub(super) events: Arc<dyn EngineEvents>,
    pub(super) state: crate::PlaybackState,
    /// Set by Load/Stop; the device callback discards leftover ring samples
    /// so the previous track cannot keep sounding under the new one.
    pub(super) flush_ring: Arc<AtomicBool>,
    /// Set by hard_cut (skip / Load / Stop); the device callback ramps down
    /// the outgoing audio over 120 ms before discarding the backlog.
    pub(super) bail_flush: Arc<AtomicBool>,
    pub(super) playback_speed: f32,
    pub(super) skip_silence: bool,
    pub(super) eq: EqualizerProcessor,
    pub(super) sound_mode: SoundMode,
    pub(super) clarity_tuning: ClarityTuning,
    pub(super) clarity: Clarity,
    pub(super) protector: Protector,
    pub(super) pipeline_tail_frames: usize,
    pub(super) output_generation: u64,
    pub(super) queue_target_ms: u64,
    pub(super) underrun_counter: Option<Arc<AtomicU64>>,
    pub(super) observed_underruns: u64,
    pub(super) nerd: Arc<Mutex<NerdSnapshot>>,
    pub(super) last_diagnostic_signature: Option<u32>,
    pub(super) last_diagnostic_publish: Option<Instant>,
    /// Loudness-normalization master switch (upstream
    /// `AppSettings.loudnessNormalization`, on by default). Per-voice figures
    /// ride on the voices; this decides whether the render uses them.
    pub(super) loudness_enabled: bool,
    pub(super) loudness_mode: LoudnessMode,
    /// Correlation probe for a source swap, see [`SwapProbe`].
    pub(super) swap_probe: SwapProbe,
}

/// Measures how correlated the two sides of a source swap really are.
///
/// A swap fades one recording into another copy of itself, and which gain pair
/// keeps that level-flat depends entirely on the correlation `ρ` between the
/// two encodings: the linear pair is flat at ρ=1 and the equal-power pair at
/// ρ=0, and the port's deviation from flat with the linear pair is
/// `10·log10((1+ρ)/2)` — 0 dB at ρ=1, −0.11 at 0.95, −1.25 at 0.5.
///
/// Nothing else can supply that number, because it depends on how two encoders
/// of one master differ, so it has to come from a real upgrade. This reads it
/// off the samples that are genuinely being blended: both voices are already
/// decoded and aligned by the time they are rendered, so the probe costs a few
/// multiplies per sample for the length of the fade — no extra decoding and no
/// perturbation of what the listener hears.
#[derive(Default)]
pub(super) struct SwapProbe {
    /// The outgoing voice's samples for the current chunk.
    pub(super) current: Vec<f32>,
    /// How many of those the incoming voice's chunk also covers.
    pub(super) len: usize,
    pub(super) sum_aa: f64,
    pub(super) sum_ab: f64,
    pub(super) sum_bb: f64,
}

impl SwapProbe {
    pub(super) fn reset(&mut self) {
        self.len = 0;
        self.sum_aa = 0.0;
        self.sum_ab = 0.0;
        self.sum_bb = 0.0;
    }

    /// Records the outgoing side of a chunk.
    pub(super) fn observe_current(&mut self, frames: &[f32]) {
        if self.current.len() < frames.len() {
            self.current.resize(frames.len(), 0.0);
        }
        self.len = frames.len();
        self.current[..self.len].copy_from_slice(frames);
        for sample in &self.current[..self.len] {
            self.sum_aa += (*sample as f64) * (*sample as f64);
        }
    }

    /// Records the incoming side and the cross term. The fade gains are
    /// positive scalars, so they cancel out of a normalised correlation and
    /// the raw samples can be used on both sides.
    pub(super) fn observe_incoming(&mut self, frames: &[f32]) {
        let n = frames.len().min(self.len);
        for index in 0..n {
            let outgoing = self.current[index] as f64;
            let incoming = frames[index] as f64;
            self.sum_ab += outgoing * incoming;
            self.sum_bb += incoming * incoming;
        }
    }

    /// Pearson correlation at zero lag, or `None` until both sides have been
    /// seen and both carry signal.
    pub(super) fn correlation(&self) -> Option<f64> {
        if self.sum_aa <= 0.0 || self.sum_bb <= 0.0 {
            return None;
        }
        Some(self.sum_ab / (self.sum_aa.sqrt() * self.sum_bb.sqrt()))
    }
}

#[derive(Debug, Clone, Default)]
pub struct NerdSnapshot {
    pub codec: String,
    pub sample_rate: u32,
    pub bit_depth: u32,
    pub channels: u32,
    pub kbps: u32,
    /// Applied loudness correction in dB (`None` = no correction: switch off
    /// or no figure). The pipeline panel reads this, not the switch — the
    /// switch is the request, this is the answer.
    pub loudness_gain_db: Option<f32>,
    /// The level correction the last armed transition applied to its incoming
    /// deck, in dB (`None` = none was armed: no comparable figures, or already
    /// level). Kept after the handoff, like the swap correlation, because it is
    /// a fact about the join that was made.
    pub transition_match_db: Option<f32>,
    /// Pearson correlation between the two encodings of the last source swap,
    /// measured on the samples that were actually blended. `None` until a swap
    /// has run. This is data about the catalogue — how alike two encoders of one
    /// master are — and it is the only number that says whether the swap's
    /// linear fade is the flat one.
    pub swap_correlation: Option<f64>,
    pub swap_target: String,
    pub swap_accepted: Option<bool>,
    pub active_stages: Vec<String>,
    pub converter_delay_frames: f64,
    pub protection_reduction_db: f32,
    pub protection_interventions: u64,
    pub build_revision: String,
    pub decoder_implementation: String,
    pub decoded_layout: String,
    pub queued_target_ms: u64,
    pub loudness_origin: String,
}

impl MixerState {
    pub(super) fn diagnostic_settings(&self) -> serde_json::Value {
        serde_json::json!({"sound_mode":format!("{:?}",self.sound_mode),"clarity_preset":format!("{:?}",self.clarity_tuning.preset),"clarity_wet":self.clarity_tuning.wet,"clarity_trims_db":self.clarity_tuning.trims_db,"manual_eq":self.eq.diagnostic_settings(),"loudness_mode":format!("{:?}",self.loudness_mode),"playback_speed":self.playback_speed,"application_volume":self.volume,"bit_perfect":self.bit_perfect,"spatial_enabled":self.spatial_enabled,"head_yaw":self.head_yaw,"skip_silence":self.skip_silence,"crossfade_seconds":self.crossfade_window_s,"underruns":self.underrun_counter.as_ref().map(|c|c.load(Ordering::Relaxed))})
    }
    pub(super) fn set_state(&mut self, state: crate::PlaybackState) {
        if self.state != state {
            self.state = state;
            self.events.state_changed(state);
        }
    }

    pub(super) fn publish_nerd(&self, info: &TrackInfo, loudness_db: Option<f64>) {
        let (_, gain_db) = loudness_gain(loudness_db, self.loudness_enabled);
        if let Ok(mut nerd) = self.nerd.lock() {
            // Preserved: they describe the last swap and the last transition,
            // not this track.
            let correlation = nerd.swap_correlation;
            let transition_match = nerd.transition_match_db;
            let swap_target = nerd.swap_target.clone();
            let swap_accepted = nerd.swap_accepted;
            *nerd = NerdSnapshot {
                codec: info.codec.clone(),
                sample_rate: info.sample_rate,
                bit_depth: info.bit_depth,
                channels: info.channels,
                kbps: info.kbps,
                loudness_gain_db: gain_db,
                swap_correlation: correlation,
                transition_match_db: transition_match,
                swap_target,
                swap_accepted,
                ..NerdSnapshot::default()
            };
        }
    }

    /// Upstream `retire` + skip: drop every voice and the pending next so a
    /// user-chosen track cannot mix with the one it replaced.
    ///
    /// Rather than hard-clearing the output ring on a sample boundary (which
    /// produces an audible click), triggers `bail_flush`: the output callback
    /// smoothly ramps out the next 120 ms of buffered audio and discards the
    /// remaining backlog before the new track starts.
    pub(super) fn hard_cut(&mut self) {
        self.reject_pending_swap();
        self.eq.flush();
        self.clarity.reset();
        self.protector.reset();
        self.pipeline_tail_frames = 0;
        self.transition = None;
        self.incoming = None;
        self.pending_next = None;
        self.retiring.clear();
        self.current = None;
        if self.bit_perfect {
            self.flush_ring.store(true, Ordering::Release);
        } else {
            self.bail_flush.store(true, Ordering::Release);
        }
    }

    /// Gapless: the current track ended and the next file is already queued.
    pub(super) fn promote_pending(&mut self) {
        if self.hold_end {
            return;
        }
        let Some(request) = self.pending_next.take() else {
            return;
        };
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
                voice.normalization_mode = self.loudness_mode;
                voice.arm_transition_match(None, self.loudness_enabled);
                voice.gain = 1.0;
                voice.release_plan_stretch(self.playback_speed, self.device_rate);
                let info = voice.info.clone();
                let duration = info.duration_seconds;
                let (_, gain_db) = loudness_gain(voice.loudness_db, self.loudness_enabled);
                if let Ok(mut nerd) = self.nerd.lock() {
                    let correlation = nerd.swap_correlation;
                    *nerd = NerdSnapshot {
                        codec: info.codec.clone(),
                        sample_rate: info.sample_rate,
                        bit_depth: info.bit_depth,
                        channels: info.channels,
                        kbps: info.kbps,
                        loudness_gain_db: gain_db,
                        swap_correlation: correlation,
                        ..NerdSnapshot::default()
                    };
                }
                log::info!("promoted pending next: {}", info.title);
                self.current = Some(voice);
                self.duration_ms
                    .store((duration * 1000.0) as u64, Ordering::Relaxed);
                if duration > 0.0 {
                    self.events.duration_changed(duration);
                }
                self.publish_audible_position();
                self.events.handoff(info);
            }
            Err(e) => {
                log::warn!("promote pending failed for {}: {e}", request.title);
                self.events
                    .error(format!("could not prepare next track: {e}"));
            }
        }
    }

    pub(super) fn promote_incoming(&mut self) {
        if let Some(mut incoming) = self.incoming.take() {
            incoming.gain = 1.0;
            // Nor does the blend's level match: a skip in the middle of a blend
            // used to leave the incoming boosted for the rest of the track.
            incoming.release_transition_match();
            incoming.filter.open();
            // It is the session voice now; a beatmatched handoff's stretch does
            // not outlive the blend it was for.
            incoming.release_plan_stretch(self.playback_speed, self.device_rate);
            let info = incoming.info.clone();
            let duration = info.duration_seconds;
            self.current = Some(incoming);
            self.duration_ms
                .store((duration * 1000.0) as u64, Ordering::Relaxed);
            if duration > 0.0 {
                self.events.duration_changed(duration);
            }
            let db = self.current.as_ref().and_then(|v| v.loudness_db);
            self.publish_nerd(&info, db);
            self.publish_audible_position();
            self.events.handoff(info);
        }
        self.transition = None;
    }

    /// Replaces the current voice's source in place, crossfading into the
    /// replacement — upstream `swapCurrentToVersion`.
    ///
    /// The replacement opens at the outgoing voice's *emitted* position, so the
    /// moment the fade starts both voices are reading the same instant of the
    /// same recording. The fade is driven by the incoming voice's own output,
    /// so it waits for the decoder to prime rather than guessing how long that
    /// takes; until then the outgoing voice stays at unity and nothing is lost.
    pub(super) fn swap_source(
        &mut self,
        request: TrackSource,
        crossfade_seconds: f64,
    ) -> Result<TrackInfo, String> {
        if self.current.is_none() {
            return Err("no track to swap".into());
        }
        // A queue transition in flight belongs to the track that is leaving.
        // Abandoning it here would promote an incoming track we are about to
        // discard, so the swap is refused instead and the caller can retry.
        if self.transition.is_some() {
            return Err("a transition is already running".into());
        }
        // The blend has not armed yet, but it is inside the window where it
        // will. A 550 ms source swap would take the only transition slot, the
        // song change would never arm, and the next track would cut in.
        if self.pending_next.is_some() {
            if let Some(current) = &self.current {
                let plan = self
                    .pending_next
                    .as_ref()
                    .map(|request| request.plan.clone())
                    .unwrap_or_default();
                let fade_s = self.plan_seconds(&plan, current.known_duration.unwrap_or(0.0));
                let tail = self.audible_tail_s(&plan, current);
                let horizon = fade_s + ARM_LEAD_MS as f64 / 1000.0;
                if tail.is_finite() && tail <= horizon {
                    return Err("a transition is already running".into());
                }
            }
        }
        let anchor = self
            .current
            .as_ref()
            .map(|voice| voice.emitted_position_seconds())
            .unwrap_or(0.0);
        let mut request = request;
        // Leave room before the nominal position for either sign of codec delay.
        let speed = self.playback_speed as f64;
        request.start_seconds =
            (anchor - super::swap_alignment::MAX_OFFSET_SECONDS * speed).max(0.0);
        let mut voice = Voice::open(
            &request,
            self.spatial_enabled && self.sound_mode == SoundMode::Enhanced,
            self.head_yaw,
            self.device_rate,
            self.playback_speed,
            self.skip_silence && self.sound_mode == SoundMode::Enhanced,
            false,
        )?;
        if let Ok(mut nerd) = self.nerd.lock() {
            nerd.swap_correlation = None;
            nerd.swap_target = voice.info.source.clone();
            nerd.swap_accepted = None;
        }
        if let Err(error) = super::swap_alignment::align(
            self.current.as_mut().unwrap(),
            &mut voice,
            self.device_rate,
        ) {
            if let Ok(mut nerd) = self.nerd.lock() {
                nerd.swap_accepted = Some(false);
            }
            return Err(error);
        }
        voice.gain = 0.0; // silent until the fade lifts it
        voice.applied_gain = 0.0;
        let info = voice.info.clone();
        let seconds = if crossfade_seconds > 0.0 {
            crossfade_seconds
        } else {
            SWAP_CROSSFADE_MS / 1000.0
        };
        let fade_frames = (seconds * self.device_rate as f64).max(1.0) as u64;
        log::info!(
            "source swap: {} at {:.3}s, {:.0}ms crossfade",
            info.source,
            anchor,
            seconds * 1000.0
        );
        self.swap_probe.reset();
        if let Ok(mut nerd) = self.nerd.lock() {
            nerd.swap_correlation = None;
            nerd.swap_target = info.source.clone();
            nerd.swap_accepted = None;
        }
        self.incoming = Some(voice);
        self.transition = Some(TransitionState {
            phase: Phase::Fading,
            fade_frames,
            handed_off: true,
            plan: TransitionPlan::default(),
            arm_deadline: Instant::now(),
            swap: true,
            fx: None,
            effect_start_frame: 0,
            effect_input_gain: None,
        });
        // Same track, same instant: the timeline does not move, so this only
        // re-points the playhead at the replacement voice.
        self.publish_audible_position();
        Ok(info)
    }

    /// Arms a transition when the current track's audible tail is close
    /// enough. Returns true if a transition was created.
    /// Where the outgoing track ends, on its own timeline, or `INFINITY` when
    /// nothing knows.
    ///
    /// The planned anchor is authoritative for when to transition. The
    /// duration figures are the fallback when no anchor was analyzed.
    ///
    /// 1. `transition_end_seconds` — the planner's ranked mix-out anchor, which
    ///    is where the blend is meant to *finish* and can sit up to 12 s inside
    ///    the track. Using it is what stops a whole blend landing late.
    /// 2. `outgoing_duration_seconds` — the planner's measurement of the track,
    ///    which exists for exactly the case the decoder cannot cover.
    /// 3. The live decoder's own remaining time, which is the file end and is
    ///    `None` for a container that declares no length.
    ///
    /// The second is not redundant with the third on purpose: the planner reads
    /// the track through the metadata reader to find its mix-out anchor, so it
    /// has a figure in hand whether or not the decoder that will play the track
    /// was able to work one out.
    pub(super) fn outgoing_end_s(&self, plan: &TransitionPlan, current: &Voice) -> f64 {
        let analyzed_end = (plan.transition_end_seconds.is_finite()
            && plan.transition_end_seconds > 0.0)
            .then_some(plan.transition_end_seconds);
        let decoded_end = current
            .known_duration
            .filter(|duration| duration.is_finite() && *duration > 0.0);
        if let Some(end) = analyzed_end {
            // A stale or inaccurate analysis anchor must never schedule the
            // blend past the audio the decoder can actually produce.
            return decoded_end.map_or(end, |duration| {
                end.min(current.regions.effective_end(duration))
            });
        }
        if let Some(duration) = decoded_end {
            return current.regions.effective_end(duration);
        }
        if plan.outgoing_duration_seconds.is_finite() && plan.outgoing_duration_seconds > 0.0 {
            return plan.outgoing_duration_seconds;
        }
        current.position_seconds() + current.remaining_seconds().unwrap_or(f64::INFINITY)
    }

    /// The audible tail left in the outgoing track, in seconds, or `INFINITY`
    /// when the end is unknown and therefore unschedulable.
    pub(super) fn audible_tail_s(&self, plan: &TransitionPlan, current: &Voice) -> f64 {
        let end_s = self.outgoing_end_s(plan, current);
        if !end_s.is_finite() {
            return f64::INFINITY;
        }
        let buffered_s =
            self.buffered_frames.load(Ordering::Relaxed) as f64 / self.device_rate as f64;
        (end_s - current.position_seconds()).max(0.0) + buffered_s
    }

    /// 120 ms ramp-away for an interrupted transition — no click.
    pub(super) fn bail(&mut self) {
        // A seek must keep the original until correlation has accepted the replacement.
        if self.reject_pending_swap() {
            return;
        }
        let Some(t) = &self.transition else {
            return;
        };
        if !t.handed_off {
            // Nothing was ever audible; no ramp to run.
            self.incoming = None;
            self.transition = None;
            if let Some(current) = &mut self.current {
                current.gain = 1.0;
                current.filter.open();
            }
            return;
        }
        // Open glided (never snapped): the incoming track is audible and its
        // low end may be mid-handover.
        let incoming = self.incoming.take().map(|mut incoming| {
            incoming.filter.open();
            incoming.gain = 1.0;
            // A seek abandoned the blend. The glide is for a mix that finished,
            // not for one that was interrupted, and neither is the level match.
            incoming.release_plan_stretch(self.playback_speed, self.device_rate);
            incoming.release_transition_match();
            incoming
        });
        if let Some(mut current) = self.current.take() {
            let from_gain = current.gain;
            current.filter.open();
            self.retiring.push(RetiringVoice {
                voice: current,
                from_gain,
                started_at: Instant::now(),
            });
        }
        // After the handoff the session already belongs to the incoming track,
        // so it becomes the current voice. Leaving it in `incoming` with no
        // transition left the mixer with nothing current and nothing it would
        // render: the seek that bailed was refused as "nothing playing" and the
        // output ran dry until the app was restarted.
        self.current = incoming;
        self.transition = None;
    }

    pub(super) fn reject_pending_swap(&mut self) -> bool {
        if !self
            .transition
            .as_ref()
            .is_some_and(|transition| transition.swap)
        {
            return false;
        }
        if let Ok(mut nerd) = self.nerd.lock() {
            nerd.swap_accepted = Some(false);
        }
        self.incoming = None;
        self.transition = None;
        if let Some(current) = &mut self.current {
            current.gain = 1.0;
            current.filter.open();
        }
        true
    }

    pub(super) fn stop_all(&mut self) {
        self.hard_cut();
        self.playing = false;
        self.set_state(crate::PlaybackState::Stopped);
    }

    /// Playhead the listener hears: decoder time minus samples still in the
    /// output ring (not yet consumed by the device callback). After handoff
    /// this is the incoming track — the session has already moved on.
    pub(super) fn publish_audible_position(&mut self) {
        let handed_off = self.transition.as_ref().is_some_and(|t| t.handed_off);
        let swap = self.transition.as_ref().is_some_and(|t| t.swap);
        let Some(voice) = (if handed_off {
            self.incoming.as_ref().or(self.current.as_ref())
        } else {
            self.current.as_ref()
        }) else {
            return;
        };
        let buffered_s =
            self.buffered_frames.load(Ordering::Relaxed) as f64 / self.device_rate.max(1) as f64;
        // Incoming has not filled the ring yet; subtracting the outgoing
        // backlog would clamp a just-cued track to 0 every time. Only the
        // current (session) voice's own backlog counts.
        //
        // A source swap is the exception: it is the same track at the same
        // instant, so the ring backlog still belongs to the timeline and comes
        // off exactly as it does mid-track. Publishing the replacement's
        // decoder head instead would throw the playhead forward by the whole
        // ring depth.
        let audible = if handed_off && !swap {
            voice.emitted_position_seconds().max(0.0)
        } else {
            (voice.emitted_position_seconds()
                - buffered_s * voice.effective_speed as f64 * voice.plan_rate as f64)
                .max(0.0)
        };
        let processing_delay = if !self.bit_perfect {
            (self.clarity.delay_frames() as usize + self.protector.delay_frames()) as f64
                / self.device_rate.max(1) as f64
                * voice.effective_speed as f64
                * voice.plan_rate as f64
        } else {
            0.0
        };
        self.position_ms.store(
            ((audible - processing_delay).max(0.0) * 1000.0) as u64,
            Ordering::Relaxed,
        );
    }
}
