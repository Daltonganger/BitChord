//! A voice bridge: repeats a bar-aligned unit of the outgoing track to cover
//! the gap until the incoming lands.

/// F's loop bridge: one measured, vocal-free unit of the outgoing's outro,
/// captured as it is served and replayed at the same point in the fade.
///
/// The unit's length is the planner's measured downbeat span (not bpm × 4), and
/// the seam is a **10 ms equal-gain** crossfade from the unit's end to its
/// start: both sides are the same record, so an equal-power seam would add
/// about +3 dB at the join.
pub(super) struct VoiceBridge {
    /// Served frames to wait, from the blend start, before the unit begins.
    pub(super) delay_frames: u64,
    /// The unit's length in device frames; the captured buffer is this long.
    pub(super) unit_frames: u64,
    /// Extra passes after the first one.
    pub(super) repeats: u32,
    /// Frames served since arming, for the delay.
    pub(super) served: u64,
    /// The captured unit, interleaved stereo, `unit_frames` frames.
    pub(super) captured: Vec<f32>,
    /// Frames already served of the current replay.
    pub(super) replay_at: usize,
    pub(super) done_replays: u32,
    pub(super) stage: BridgeStage,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BridgeStage {
    Waiting,
    Capturing,
    Replaying,
    Done,
}

impl VoiceBridge {
    pub(super) fn new(delay_frames: u64, unit_frames: u64, repeats: u32) -> Self {
        Self {
            delay_frames,
            unit_frames,
            repeats: repeats.max(1),
            served: 0,
            captured: Vec::with_capacity(unit_frames as usize * 2),
            replay_at: 0,
            done_replays: 0,
            stage: BridgeStage::Waiting,
        }
    }

    /// The replay's output for up to `frames` frames. The first 10 ms of each
    /// pass crossfades the audio just heard (the unit's end) into the unit's
    /// start; the replay's last sample is the same one the first pass ended on,
    /// so returning to the live stream needs no second fade.
    pub(super) fn replay_into(&mut self, frames: usize, rate: u32, out: &mut Vec<f32>) {
        let seam =
            ((rate as u64 / 100).max(1) as usize).min((self.unit_frames / 4).max(1) as usize);
        while out.len() < frames * 2 && self.stage == BridgeStage::Replaying {
            let at = self.replay_at;
            if at >= self.unit_frames as usize {
                self.replay_at = 0;
                self.done_replays += 1;
                if self.done_replays >= self.repeats {
                    self.stage = BridgeStage::Done;
                }
                continue;
            }
            if at < seam {
                let gain = (at + 1) as f32 / seam as f32;
                let tail = (self.unit_frames as usize - seam + at) * 2;
                out.push(self.captured[tail] * (1.0 - gain) + self.captured[at * 2] * gain);
                out.push(self.captured[tail + 1] * (1.0 - gain) + self.captured[at * 2 + 1] * gain);
            } else {
                out.push(self.captured[at * 2]);
                out.push(self.captured[at * 2 + 1]);
            }
            self.replay_at += 1;
        }
    }

    /// Reads the frames just served. While waiting it counts down; while
    /// capturing it fills the unit; when the unit completes mid-chunk it holds
    /// the frames after it in `pending` (so the replay can play first) and
    /// switches to replaying. Returns the number of frames held back.
    pub(super) fn after_pull(
        &mut self,
        out: &mut Vec<f32>,
        pending: &mut Vec<f32>,
        pending_cursor: &mut usize,
    ) -> usize {
        if self.stage != BridgeStage::Waiting && self.stage != BridgeStage::Capturing {
            return 0;
        }
        let mut frames = out.len() / 2;
        let mut skip = 0usize;
        if self.stage == BridgeStage::Waiting {
            if self.served + frames as u64 <= self.delay_frames {
                self.served += frames as u64;
                return 0;
            }
            let into = (self.delay_frames - self.served) as usize;
            skip = into.min(frames);
            self.served += skip as u64;
            frames -= skip;
            self.stage = BridgeStage::Capturing;
        }
        let captured_frames = self.captured.len() / 2;
        let remaining = (self.unit_frames as usize).saturating_sub(captured_frames);
        let take = frames.min(remaining);
        if take > 0 {
            let start = skip * 2;
            self.captured
                .extend_from_slice(&out[start..start + take * 2]);
        }
        if self.captured.len() / 2 == self.unit_frames as usize {
            let cut = (skip + take) * 2;
            let held = out.split_off(cut);
            let held_frames = held.len() / 2;
            let rest = pending[*pending_cursor..].to_vec();
            *pending = held;
            pending.extend(rest);
            *pending_cursor = 0;
            self.replay_at = 0;
            self.done_replays = 0;
            self.stage = BridgeStage::Replaying;
            return held_frames;
        }
        0
    }
}
