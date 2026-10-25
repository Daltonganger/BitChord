//! Source-time playback policy. Detection reads dry stereo PCM; no file writes
//! or analysis run on the output callback. Unfinished files have no tail bound.
use crate::decode::{SourceKind, SymphoniaDecoder};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(uniffi::Record, Clone, Debug, Default)]
pub struct PlaybackInterval {
    pub start_seconds: f64,
    pub end_seconds: f64,
}
#[derive(uniffi::Record, Clone, Debug, Default)]
pub struct PlaybackRegions {
    pub audible_start_seconds: f64,
    pub audible_end_seconds: Option<f64>,
    pub excluded: Vec<PlaybackInterval>,
}
impl PlaybackRegions {
    pub fn normalized(mut self, duration: f64) -> Self {
        if !self.audible_start_seconds.is_finite() {
            self.audible_start_seconds = 0.0;
        }
        self.audible_start_seconds = self.audible_start_seconds.max(0.0);
        self.audible_end_seconds = self
            .audible_end_seconds
            .filter(|e| e.is_finite() && *e > self.audible_start_seconds);
        if duration > 0.0 {
            self.audible_start_seconds = self.audible_start_seconds.min(duration);
            self.audible_end_seconds = self.audible_end_seconds.map(|e| e.min(duration));
        }
        self.excluded.retain(|s| {
            s.start_seconds.is_finite()
                && s.end_seconds.is_finite()
                && s.start_seconds >= 0.0
                && s.end_seconds > s.start_seconds
        });
        for segment in &mut self.excluded {
            if duration > 0.0 {
                segment.end_seconds = segment.end_seconds.min(duration);
            }
        }
        self.excluded.retain(|s| s.end_seconds > s.start_seconds);
        self.excluded
            .sort_by(|a, b| a.start_seconds.total_cmp(&b.start_seconds));
        let mut merged: Vec<PlaybackInterval> = Vec::new();
        for segment in self.excluded {
            if let Some(last) = merged
                .last_mut()
                .filter(|s| segment.start_seconds <= s.end_seconds)
            {
                last.end_seconds = last.end_seconds.max(segment.end_seconds);
            } else {
                merged.push(segment);
            }
        }
        self.excluded = merged;
        self
    }
    pub fn effective_end(&self, duration: f64) -> f64 {
        let mut end = self.audible_end_seconds.unwrap_or(duration);
        for s in self.excluded.iter().rev() {
            if end > 0.0 && s.end_seconds >= end - 0.05 && s.start_seconds < end {
                end = s.start_seconds;
            }
        }
        end
    }
    pub fn next_allowed(&self, mut position: f64) -> f64 {
        position = position.max(self.audible_start_seconds);
        for s in &self.excluded {
            if position >= s.start_seconds && position < s.end_seconds {
                position = s.end_seconds;
            }
        }
        position
    }
}
fn registry() -> &'static Mutex<HashMap<String, PlaybackRegions>> {
    static REGIONS: OnceLock<Mutex<HashMap<String, PlaybackRegions>>> = OnceLock::new();
    REGIONS.get_or_init(|| Mutex::new(HashMap::new()))
}
pub fn get(source: &str) -> PlaybackRegions {
    registry()
        .lock()
        .unwrap()
        .get(source)
        .cloned()
        .unwrap_or_default()
}
pub fn put(source: String, regions: PlaybackRegions) {
    let mut entries = registry().lock().unwrap();
    if entries.len() > 256 {
        entries.clear();
    }
    entries.insert(source, regions);
}

/// One 10 ms stereo window: the loudest channel's RMS and the frame peak.
struct WindowLevel {
    rms: f64,
    peak: f32,
}

/// −30 dBFS peak (0.0316) is the ceiling for "this window is the quiet edge of
/// the recording, not music". A fade tail or a room-noise floor has a low peak;
/// a quiet musical passage can have a low RMS and still peaks above this.
const EDGE_PEAK_CEILING: f32 = 0.031_622_8;
/// How far under the recording's own level a window must sit to be an edge.
/// 35 dB, not 30: a quiet coda or a soft intro more than 30 dB under the
/// record's own level is still music; a near-silent fade tail is 40+ dB under.
const EDGE_BELOW_REFERENCE_DB: f64 = 35.0;
/// A shorter run is a breath, not an edge to trim.
const MIN_EDGE_SECONDS: f64 = 1.0;

fn window_is_quiet(level: &WindowLevel, reference: f64) -> bool {
    let threshold = reference * 10.0f64.powf(-EDGE_BELOW_REFERENCE_DB / 20.0);
    level.rms < threshold && level.peak < EDGE_PEAK_CEILING
}

/// The 90th percentile of window RMS, not the mean: a mostly-quiet recording
/// must not make everything "loud" the moment one chorus arrives.
fn reference_level(levels: &[WindowLevel]) -> f64 {
    let mut rms: Vec<f64> = levels.iter().map(|level| level.rms).collect();
    if rms.is_empty() {
        return 0.0;
    }
    rms.sort_by(|a, b| a.total_cmp(b));
    let index = ((rms.len() - 1) as f64 * 0.9).round() as usize;
    rms[index.min(rms.len() - 1)]
}

/// Ten-millisecond stereo windows, classified against the recording's own
/// level. Both channels must be quiet: never sum L+R, so out-of-phase music
/// cannot disappear into a mono detector.
pub fn boundaries(quiet: &[bool], window: f64, duration: f64, complete: bool) -> PlaybackRegions {
    if quiet.is_empty() {
        return PlaybackRegions::default();
    }
    let min_run = ((MIN_EDGE_SECONDS / window.max(1e-6)).round() as usize).max(1);
    let head_run = quiet.iter().take_while(|q| **q).count();
    if head_run == quiet.len() {
        return PlaybackRegions::default(); // all-silent: preserve the timeline
    }
    let tail_run = quiet.iter().rev().take_while(|q| **q).count();
    let first = if head_run >= min_run { head_run } else { 0 };
    let last = if tail_run >= min_run {
        quiet.len() - tail_run
    } else {
        quiet.len()
    };
    let head = first as f64 * window;
    let tail = duration - last as f64 * window;
    PlaybackRegions {
        audible_start_seconds: if first > 0 {
            (head - 0.1).max(0.0)
        } else {
            0.0
        },
        audible_end_seconds: if complete && last < quiet.len() && tail >= 0.35 {
            Some((last as f64 * window + 0.1).min(duration))
        } else {
            None
        },
        excluded: vec![],
    }
}
pub fn detect(source: &str, complete: bool) -> Result<PlaybackRegions, String> {
    if source.starts_with("http") {
        return Err("boundary analysis requires local bytes".into());
    }
    let complete = complete
        && (!std::path::Path::new(&format!("{source}.grow")).exists()
            || std::path::Path::new(&format!("{source}.complete")).exists());
    let _lease = SourceLease::new(source);
    let mut decoder = SymphoniaDecoder::open_available(&SourceKind::parse(source), &HashMap::new())
        .map_err(|e| e.to_string())?;
    let rate = decoder.sample_rate().max(1) as usize;
    let size = (rate / 100).max(1);
    let mut levels = Vec::new();
    let mut samples = 0usize;
    let mut squares = [0f64; 2];
    let mut peak = 0f32;
    let mut frames = 0usize;
    loop {
        let data = decoder.read_stereo(4096).map_err(|e| e.to_string())?;
        if data.is_empty() {
            break;
        }
        for frame in data.chunks_exact(2) {
            for (channel, &s) in frame.iter().enumerate() {
                squares[channel] += (s as f64).powi(2);
                peak = peak.max(s.abs());
            }
            samples += 2;
            frames += 1;
            if samples == size * 2 {
                levels.push(WindowLevel {
                    rms: squares
                        .iter()
                        .map(|sum| (sum / (samples / 2) as f64).sqrt())
                        .fold(0.0, f64::max),
                    peak,
                });
                samples = 0;
                squares = [0.0; 2];
                peak = 0.0;
            }
        }
        if !complete && frames >= rate * 30 {
            break;
        }
    }
    if samples > 0 {
        levels.push(WindowLevel {
            rms: squares
                .iter()
                .map(|sum| (sum / (samples / 2) as f64).sqrt())
                .fold(0.0, f64::max),
            peak,
        });
    }
    let reference = reference_level(&levels);
    let quiet: Vec<bool> = levels
        .iter()
        .map(|level| window_is_quiet(level, reference))
        .collect();
    Ok(boundaries(
        &quiet,
        size as f64 / rate as f64,
        frames as f64 / rate as f64,
        complete,
    ))
}

/// Clamp the entire outgoing/incoming blend windows, not only their anchors.
pub fn constrain(
    mut plan: crate::TransitionPlanRec,
    out: PlaybackRegions,
    incoming: PlaybackRegions,
    out_duration: f64,
    in_duration: f64,
) -> crate::TransitionPlanRec {
    if out.audible_start_seconds == 0.0
        && out.audible_end_seconds.is_none()
        && out.excluded.is_empty()
        && incoming.audible_start_seconds == 0.0
        && incoming.audible_end_seconds.is_none()
        && incoming.excluded.is_empty()
    {
        return plan;
    }
    let out = out.normalized(out_duration);
    let incoming = incoming.normalized(in_duration);
    let end = out.effective_end(out_duration);
    let mut anchor = if plan.transition_end_seconds > 0.0 {
        if end > 0.0 {
            plan.transition_end_seconds.min(end)
        } else {
            plan.transition_end_seconds
        }
    } else {
        end
    };
    // E-2's hold is anchored to the analysed ending. If a guard moves that
    // ending, there is no protected chord to hold for at the new one.
    let anchor_moved = plan.transition_end_seconds > 0.0
        && (anchor - plan.transition_end_seconds).abs() > 0.05;
    let mut fade = if plan.fade_seconds > 0.0 {
        plan.fade_seconds
    } else {
        1.0
    };
    let cue = incoming.next_allowed(plan.cue_seconds);
    for segment in out.excluded.iter().rev() {
        if segment.start_seconds < anchor && segment.end_seconds > anchor - fade {
            if anchor - segment.end_seconds >= 0.05 {
                fade = fade.min(anchor - segment.end_seconds);
            } else {
                anchor = segment.start_seconds;
            }
        }
    }
    fade = fade.min((anchor - out.audible_start_seconds).max(0.0));
    let incoming_end = incoming
        .excluded
        .iter()
        .find(|s| s.start_seconds >= cue)
        .map(|s| s.start_seconds)
        .unwrap_or_else(|| incoming.effective_end(in_duration));
    if incoming_end > 0.0 || in_duration > 0.0 {
        fade = fade.min((incoming_end - cue).max(0.0) / plan.playback_rate.max(0.5));
    }
    let gapless = fade < 0.05;
    if gapless {
        fade = 0.001;
    }
    if (fade - plan.fade_seconds).abs() > 0.05 || (cue - plan.cue_seconds).abs() > 0.05 {
        plan.style = crate::TransitionStyleRec::EqualPower;
        plan.bass_swap = false;
        plan.filter_sweep = 0.0;
        plan.bed_fraction = 0.0;
        plan.playback_rate = 1.0;
    }
    if gapless {
        plan.style = crate::TransitionStyleRec::Gapless;
    }
    // F's bridge is anchored to a measured unit of the record's own outro. A
    // guard that moves the ending, delays the blend start past the unit, or
    // excludes audio inside it invalidates the loop; revert the extension
    // rather than leave a blend waiting for music that will not be served.
    let mut reverted_bridge = 0.0;
    if plan.bridge_seconds > 0.0 && plan.bridge_repeats > 0 {
        let extension = plan.bridge_seconds * plan.bridge_repeats as f64;
        let bridge_start = plan.bridge_start_seconds;
        let bridge_end = bridge_start + plan.bridge_seconds;
        let overlaps_exclusion = out
            .excluded
            .iter()
            .any(|s| s.start_seconds < bridge_end && s.end_seconds > bridge_start);
        let start_moved_past = anchor - fade > bridge_start + 0.05;
        if anchor_moved || overlaps_exclusion || start_moved_past {
            // The bottom of this function writes `anchor`/`fade`, so the
            // revert has to move the locals as well as the plan's own fields.
            anchor = (anchor - extension).max(0.0);
            fade = (fade - extension).max(0.001);
            plan.outgoing_hold_seconds = (plan.outgoing_hold_seconds - extension).max(0.0);
            plan.bridge_start_seconds = 0.0;
            plan.bridge_seconds = 0.0;
            plan.bridge_repeats = 0;
            reverted_bridge = extension;
        }
    }
    // A hold is anchored to the blend start. When a guard moves the start later
    // (a shorter fade), the time remaining before the protected chord shrinks
    // by the same amount — the chord itself has not moved. A moved anchor
    // abandons the hold instead: the chord it protected is no longer the end.
    if plan.outgoing_hold_seconds > 0.0 {
        let moved = ((plan.fade_seconds - reverted_bridge) - fade).max(0.0);
        plan.outgoing_hold_seconds =
            (plan.outgoing_hold_seconds - moved).clamp(0.0, (fade - 0.05).max(0.0));
        if anchor_moved {
            plan.outgoing_hold_seconds = 0.0;
        }
    }
    plan.cue_seconds = cue;
    plan.transition_end_seconds = anchor;
    plan.fade_seconds = fade;
    plan
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edges_preserve_guards_and_partial_tail() {
        let mut q = vec![true; 100];
        q.extend(vec![false; 200]);
        q.extend(vec![true; 100]);
        let r = boundaries(&q, 0.01, 4.0, true);
        assert!((r.audible_start_seconds - 0.9).abs() < 0.001);
        assert_eq!(r.audible_end_seconds, Some(3.1));
        assert_eq!(boundaries(&q, 0.01, 4.0, false).audible_end_seconds, None);
        assert_eq!(
            boundaries(&[true; 100], 0.01, 1.0, true).audible_start_seconds,
            0.0
        );
    }
    #[test]
    fn intervals_merge_and_terminal_region_sets_end() {
        let r = PlaybackRegions {
            excluded: vec![
                PlaybackInterval {
                    start_seconds: 8.0,
                    end_seconds: 12.0,
                },
                PlaybackInterval {
                    start_seconds: 9.0,
                    end_seconds: 10.0,
                },
            ],
            ..Default::default()
        }
        .normalized(12.0);
        assert_eq!(r.excluded.len(), 1);
        assert_eq!(r.effective_end(12.0), 8.0);
        assert_eq!(r.next_allowed(9.0), 12.0);
    }
    fn fixture(
        name: &str,
        head: f32,
        music: f32,
        tail: f32,
        amplitude: f32,
        stereo: bool,
    ) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "bitchord-regions-{}-{name}.wav",
            std::process::id()
        ));
        let rate = 10000u32;
        let frames = ((head + music + tail) * rate as f32) as usize;
        let length = frames as u32 * 8;
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend((36 + length).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(3u16.to_le_bytes());
        bytes.extend(2u16.to_le_bytes());
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * 8).to_le_bytes());
        bytes.extend(8u16.to_le_bytes());
        bytes.extend(32u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(length.to_le_bytes());
        for i in 0..frames {
            let t = i as f32 / rate as f32;
            let value = if t >= head && t < head + music {
                amplitude * (t * 200.0 * std::f32::consts::PI).sin()
            } else {
                0.000003
            };
            bytes.extend(value.to_le_bytes());
            bytes.extend(if stereo {
                (-value).to_le_bytes()
            } else {
                0.0f32.to_le_bytes()
            });
        }
        std::fs::write(&path, bytes).unwrap();
        path
    }
    /// A stereo float WAV whose sample is a function of time, for the shapes a
    /// single sine-and-silence fixture cannot make.
    fn float_wav(name: &str, seconds: f32, sample: impl Fn(f32) -> (f32, f32)) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "bitchord-regions-{}-{name}.wav",
            std::process::id()
        ));
        let rate = 10000u32;
        let frames = (seconds * rate as f32) as usize;
        let length = frames as u32 * 8;
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend((36 + length).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(3u16.to_le_bytes());
        bytes.extend(2u16.to_le_bytes());
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * 8).to_le_bytes());
        bytes.extend(8u16.to_le_bytes());
        bytes.extend(32u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(length.to_le_bytes());
        for i in 0..frames {
            let (left, right) = sample(i as f32 / rate as f32);
            bytes.extend(left.to_le_bytes());
            bytes.extend(right.to_le_bytes());
        }
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// A −48 dBFS room-noise lead-in and fade tail around loud music. The old
    /// absolute gate trimmed digital silence only, so this is the shape C is
    /// for; both edges are longer than a second.
    #[test]
    fn a_noise_lead_in_and_fade_tail_are_trimmed() {
        let path = float_wav("edges", 6.0, |t| {
            let noise = ((t * 7919.0).sin() * 0.5 + (t * 127.1).sin() * 0.5) * 0.002;
            if t < 1.5 || t >= 4.5 {
                (noise, -noise)
            } else {
                let music = (t * 300.0 * std::f32::consts::PI).sin() * 0.2;
                (music, music)
            }
        });
        let r = detect(path.to_str().unwrap(), true).unwrap();
        assert!((r.audible_start_seconds - 1.4).abs() < 0.06, "{r:?}");
        let end = r.audible_end_seconds.expect("the tail must be trimmed");
        assert!((end - 4.6).abs() < 0.06, "{r:?}");
        std::fs::remove_file(path).unwrap();
    }

    /// A quiet musical passage is not an edge: its RMS is lower, but it is
    /// within 30 dB of the recording and it is music.
    #[test]
    fn a_quiet_musical_intro_is_not_trimmed() {
        let path = float_wav("quiet-intro", 6.0, |t| {
            let level = if t < 2.0 || t >= 5.0 { 0.0126 } else { 0.2 };
            let music = (t * 300.0 * std::f32::consts::PI).sin() * level;
            (music, music)
        });
        let r = detect(path.to_str().unwrap(), true).unwrap();
        assert_eq!(r.audible_start_seconds, 0.0, "{r:?}");
        assert_eq!(r.audible_end_seconds, None, "{r:?}");
        std::fs::remove_file(path).unwrap();
    }

    /// The edge gate: a coda 32 dB under the recording survives, a fade tail
    /// 40 dB under it is trimmed.
    #[test]
    fn a_thirty_two_db_coda_survives_and_a_forty_db_tail_is_trimmed() {
        let coda = float_wav("coda", 6.0, |t| {
            let level = if t < 4.0 { 0.2 } else { 0.2 * 10f32.powf(-32.0 / 20.0) };
            let music = (t * 300.0 * std::f32::consts::PI).sin() * level;
            (music, music)
        });
        let r = detect(coda.to_str().unwrap(), true).unwrap();
        assert_eq!(r.audible_start_seconds, 0.0, "{r:?}");
        assert_eq!(r.audible_end_seconds, None, "{r:?}");
        std::fs::remove_file(coda).unwrap();

        let tail = float_wav("fade-tail", 5.5, |t| {
            let level = if t < 4.0 { 0.2 } else { 0.2 * 10f32.powf(-40.0 / 20.0) };
            let music = (t * 300.0 * std::f32::consts::PI).sin() * level;
            (music, music)
        });
        let r = detect(tail.to_str().unwrap(), true).unwrap();
        let end = r.audible_end_seconds.expect("a −40 dB tail must be trimmed");
        assert!((end - 4.1).abs() < 0.06, "{r:?}");
        std::fs::remove_file(tail).unwrap();
    }

    /// The RMS rule alone would call a quiet passage silence; the peak ceiling
    /// is what keeps a transient-heavy quiet section audible.
    #[test]
    fn a_quiet_window_still_needs_a_low_peak() {
        let reference = 0.5;
        assert!(window_is_quiet(
            &WindowLevel { rms: 0.005, peak: 0.03 },
            reference
        ));
        assert!(!window_is_quiet(
            &WindowLevel { rms: 0.005, peak: 0.1 },
            reference
        ));
        assert!(!window_is_quiet(
            &WindowLevel { rms: 0.02, peak: 0.02 },
            reference
        ));
    }

    #[test]
    fn short_quiet_runs_are_not_edges_and_all_silence_keeps_the_timeline() {
        let mut q = vec![true; 50];
        q.extend(vec![false; 100]);
        q.extend(vec![true; 50]);
        let r = boundaries(&q, 0.01, 2.0, true);
        assert_eq!(r.audible_start_seconds, 0.0);
        assert_eq!(r.audible_end_seconds, None);

        let path = float_wav("silent", 3.0, |_| (0.0, 0.0));
        let r = detect(path.to_str().unwrap(), true).unwrap();
        assert_eq!(r.audible_start_seconds, 0.0, "{r:?}");
        assert_eq!(r.audible_end_seconds, None, "{r:?}");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn dry_stereo_detection_preserves_quiet_music_and_residual_noise() {
        for (name, stereo, level) in [("stereo", true, 0.01), ("quiet", false, 0.002)] {
            let path = fixture(name, 1.0, 2.0, 1.0, level, stereo);
            let r = detect(path.to_str().unwrap(), true).unwrap();
            assert!((r.audible_start_seconds - 0.9).abs() < 0.02, "{r:?}");
            assert!((r.audible_end_seconds.unwrap() - 3.1).abs() < 0.02, "{r:?}");
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn unfinished_recording_does_not_trim_tail_or_block() {
        let path = fixture("partial", 1.0, 2.0, 1.0, 0.1, true);
        std::fs::write(format!("{}.grow", path.display()), "").unwrap();
        let r = detect(path.to_str().unwrap(), true).unwrap();
        assert_eq!(r.audible_end_seconds, None);
        assert!((r.audible_start_seconds - 0.9).abs() < 0.02);
        // The same bytes, once the file is declared complete, get their tail.
        std::fs::remove_file(format!("{}.grow", path.display())).unwrap();
        let r = detect(path.to_str().unwrap(), true).unwrap();
        let end = r.audible_end_seconds.expect("a complete file reports its tail");
        assert!((end - 3.1).abs() < 0.02, "{r:?}");
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn safe_windows_shorten_overlap_and_clear_incoming_exclusion() {
        let plan: crate::TransitionPlanRec = crate::mixer::TransitionPlan {
            fade_seconds: 10.0,
            transition_end_seconds: 110.0,
            cue_seconds: 1.0,
            ..Default::default()
        }
        .into();
        let out = PlaybackRegions {
            excluded: vec![PlaybackInterval {
                start_seconds: 100.0,
                end_seconds: 107.0,
            }],
            ..Default::default()
        };
        let incoming = PlaybackRegions {
            excluded: vec![
                PlaybackInterval {
                    start_seconds: 0.0,
                    end_seconds: 6.0,
                },
                PlaybackInterval {
                    start_seconds: 8.0,
                    end_seconds: 10.0,
                },
            ],
            ..Default::default()
        };
        let safe = constrain(plan, out, incoming, 120.0, 60.0);
        assert_eq!(safe.cue_seconds, 6.0);
        assert_eq!(safe.transition_end_seconds, 110.0);
        assert_eq!(safe.fade_seconds, 2.0);
    }
    #[test]
    fn a_guard_moves_or_clears_the_cold_ending_hold() {
        // E-2's shape: a 4.86 s blend with a 3.36 s hold, ending at 216.5 s.
        let plan: crate::TransitionPlanRec = crate::mixer::TransitionPlan {
            fade_seconds: 4.86,
            outgoing_hold_seconds: 3.36,
            transition_end_seconds: 216.5,
            ..Default::default()
        }
        .into();
        // A SponsorBlock segment inside the blend moves its start later by
        // 0.36 s. The chord at 215.0 s has not moved, so the hold shrinks by
        // that much and the crossing still starts on the chord.
        let out = PlaybackRegions {
            excluded: vec![PlaybackInterval {
                start_seconds: 210.0,
                end_seconds: 212.0,
            }],
            ..Default::default()
        };
        let safe = constrain(plan.clone(), out, PlaybackRegions::default(), 220.0, 200.0);
        assert_eq!(safe.transition_end_seconds, 216.5);
        assert!((safe.fade_seconds - 4.5).abs() < 1e-6, "{safe:?}");
        assert!((safe.outgoing_hold_seconds - 3.0).abs() < 1e-6, "{safe:?}");
        assert!(
            (safe.transition_end_seconds - safe.fade_seconds + safe.outgoing_hold_seconds - 215.0)
                .abs()
                < 1e-6,
            "the crossing no longer starts on the chord: {safe:?}"
        );
        // A guard that moves the end itself abandons the hold: there is no
        // protected chord at the new ending.
        let out = PlaybackRegions {
            audible_end_seconds: Some(200.0),
            ..Default::default()
        };
        let safe = constrain(plan, out, PlaybackRegions::default(), 220.0, 200.0);
        assert_eq!(safe.transition_end_seconds, 200.0);
        assert_eq!(safe.outgoing_hold_seconds, 0.0);
    }
    #[test]
    fn a_guard_that_excludes_the_bridge_unit_reverts_the_extension() {
        let plan: crate::TransitionPlanRec = crate::mixer::TransitionPlan {
            fade_seconds: 12.0,
            transition_end_seconds: 104.0,
            bridge_start_seconds: 96.0,
            bridge_seconds: 4.0,
            bridge_repeats: 1,
            ..Default::default()
        }
        .into();
        let clean = constrain(plan.clone(), PlaybackRegions::default(), PlaybackRegions::default(), 120.0, 90.0);
        assert!((clean.transition_end_seconds - 104.0).abs() < 1e-6);
        assert!((clean.fade_seconds - 12.0).abs() < 1e-6);
        assert_eq!(clean.bridge_repeats, 1);
        // A SponsorBlock segment inside the unit invalidates it: the end and
        // fade return to the pre-bridge shape and no bridge is left behind.
        let out = PlaybackRegions {
            excluded: vec![PlaybackInterval {
                start_seconds: 97.0,
                end_seconds: 98.0,
            }],
            ..Default::default()
        };
        let safe = constrain(plan, out, PlaybackRegions::default(), 120.0, 90.0);
        assert!((safe.transition_end_seconds - 100.0).abs() < 1e-6, "{safe:?}");
        // The exclusion also shortens the remaining blend: [98, 100] is what
        // is left after the skipped segment and the reverted unit.
        assert!((safe.fade_seconds - 2.0).abs() < 1e-6, "{safe:?}");
        assert_eq!(safe.bridge_repeats, 0);
        assert_eq!(safe.bridge_seconds, 0.0);
    }
    #[test]
    fn insufficient_window_is_gapless_and_invalid_intervals_are_removed() {
        let plan = crate::mixer::TransitionPlan {
            fade_seconds: 4.0,
            ..Default::default()
        }
        .into();
        let incoming = PlaybackRegions {
            excluded: vec![PlaybackInterval {
                start_seconds: 0.0,
                end_seconds: 60.0,
            }],
            ..Default::default()
        };
        let safe = constrain(plan, PlaybackRegions::default(), incoming, 120.0, 60.0);
        assert_eq!(safe.style, crate::TransitionStyleRec::Gapless);
        let r = PlaybackRegions {
            excluded: vec![
                PlaybackInterval {
                    start_seconds: f64::NAN,
                    end_seconds: 5.0,
                },
                PlaybackInterval {
                    start_seconds: 9.0,
                    end_seconds: 2.0,
                },
            ],
            ..Default::default()
        }
        .normalized(10.0);
        assert!(r.excluded.is_empty());
    }
}

fn readers() -> &'static Mutex<HashMap<String, usize>> {
    static READERS: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    READERS.get_or_init(|| Mutex::new(HashMap::new()))
}
pub struct SourceLease(String);
impl SourceLease {
    pub fn new(source: &str) -> Self {
        *readers().lock().unwrap().entry(source.into()).or_default() += 1;
        Self(source.into())
    }
}
impl Drop for SourceLease {
    fn drop(&mut self) {
        let mut map = readers().lock().unwrap();
        if let Some(count) = map.get_mut(&self.0) {
            *count -= 1;
            if *count == 0 {
                map.remove(&self.0);
            }
        }
    }
}
#[uniffi::export]
pub fn source_has_playback_readers(source: String) -> bool {
    readers().lock().unwrap().contains_key(&source)
}
