//! Port of upstream `BeatTracker.kt` — Beat This! over the Slaney mel spectrogram.

use super::models;
use super::{
    compute_beat_spectrogram, resample, BEAT_SPECTROGRAM_MELS, BEAT_SPECTROGRAM_SAMPLE_RATE,
};

use rten_tensor::NdTensor;

pub const CHUNK_FRAMES: usize = 1500;
pub const BORDER_FRAMES: usize = 6;
pub const WINDOW_SECONDS: f64 = (CHUNK_FRAMES - 2 * BORDER_FRAMES) as f64 / 50.0;
const PEAK_WINDOW: i32 = 7;
/// Frames two accepted peaks must be apart; see [`pick_peaks`].
const PEAK_MIN_SPACING: i32 = 6;
const MIN_BEATS: usize = 8;
const MIN_TEMPO: f64 = 40.0;
const MAX_TEMPO: f64 = 220.0;
const FRAME_RATE: f64 = 50.0;

#[derive(Debug, Clone)]
pub struct Grid {
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
    pub bpm: f64,
    pub beat_interval: f64,
    pub first_beat: f64,
    pub beat_confidence: f64,
}

/// Track [pcm] already at any rate; resampled to the model's 22.05 kHz mono.
///
/// The whole buffer is tried first, because the end of the outgoing record is
/// where a blend is decided and a head-only grid leaves it on the DSP
/// estimator. When that pass yields nothing usable, the head window is tried
/// before giving up: over a whole track the assembled beat set can go dense
/// (one busy or spoken section is enough), and the model read the same audio
/// as a single chunk of [`WINDOW_SECONDS`] just fine. Discarding that grid
/// because the longer read failed is how nine of the thirteen cached tracks
/// in the corpus ended up planned from a 0.3-confidence DSP tempo.
pub fn track(pcm: &[f32], sample_rate: f64, offset_seconds: f64) -> Option<Grid> {
    best_grid(
        grid_from_pcm(pcm, sample_rate, offset_seconds),
        || {
            let head_samples = (WINDOW_SECONDS * sample_rate).round() as usize;
            // Already the head window (or shorter): there is nothing to retry.
            (head_samples > 0 && head_samples < pcm.len())
                .then(|| grid_from_pcm(&pcm[..head_samples], sample_rate, offset_seconds))
                .flatten()
        },
    )
}

/// The whole-track grid when it is usable, otherwise the head window's.
fn best_grid(whole: Option<Grid>, head: impl FnOnce() -> Option<Grid>) -> Option<Grid> {
    if whole.is_some() {
        return whole;
    }
    let head = head();
    if head.is_some() {
        log::info!(
            "automix analysis: the whole-track beat pass yielded no usable grid; \
             the head window's grid is used instead"
        );
    }
    head
}

fn grid_from_pcm(pcm: &[f32], sample_rate: f64, offset_seconds: f64) -> Option<Grid> {
    if pcm.is_empty() {
        return None;
    }
    let mono = if (sample_rate - BEAT_SPECTROGRAM_SAMPLE_RATE).abs() > 1.0 {
        resample(pcm, sample_rate, BEAT_SPECTROGRAM_SAMPLE_RATE)
    } else {
        pcm.to_vec()
    };
    let spectrogram = compute_beat_spectrogram(&mono, BEAT_SPECTROGRAM_SAMPLE_RATE);
    if spectrogram.frames == 0 {
        return None;
    }
    let (beat_logits, downbeat_logits) = infer(&spectrogram.values, spectrogram.frames)?;
    let beat_frames = pick_peaks(&beat_logits);
    let beats: Vec<f64> = beat_frames
        .iter()
        .map(|f| f / FRAME_RATE + offset_seconds)
        .collect();
    if beats.len() < MIN_BEATS {
        return None;
    }
    let bpm = tempo_from_beats(&beats);
    if bpm <= 0.0 {
        return None;
    }
    let mut downbeats: Vec<f64> = pick_peaks(&downbeat_logits)
        .into_iter()
        .map(|f| f / FRAME_RATE + offset_seconds)
        .map(|time| {
            beats
                .iter()
                .min_by(|a, b| (*a - time).abs().total_cmp(&(*b - time).abs()))
                .copied()
                .unwrap_or(beats[0])
        })
        .collect();
    downbeats.sort_by(|a, b| a.total_cmp(b));
    downbeats.dedup_by(|a, b| (*a - *b).abs() < 1e-4);

    // F-2: the Beat This! downbeat head is conservative — 23 bars over a
    // 188 s track at 93 BPM is about a third of the bars that exist — so the
    // missing bars are derived from the **measured beats**: each trusted model
    // downbeat anchors a chain that walks four beats at a time (each step
    // within 15 ms of the grid) and stops at the first gap it cannot measure.
    // That is measured bar counting, not bpm × 4: the bar times come from the
    // beat timestamps. Over-dense model heads are left alone.
    let interval = 60.0 / bpm;
    let span_bars = if beats.len() >= 5 && interval > 0.0 {
        ((beats[beats.len() - 1] - beats[0]) / (4.0 * interval)).round() as usize + 1
    } else {
        0
    };
    let model_bars = downbeats.len();
    if span_bars >= 4 && model_bars * 4 < span_bars * 3 {
        let derived = derive_bars(&beats, interval, &downbeats);
        let mut merged: Vec<f64> = downbeats.iter().copied().chain(derived).collect();
        merged.sort_by(|a, b| a.total_cmp(b));
        merged.dedup_by(|a, b| (*a - *b).abs() < 0.05);
        log::debug!(
            "downbeat grid: beats={} model bars={} derived bars={} of ~{} expected",
            beats.len(),
            model_bars,
            merged.len().saturating_sub(model_bars),
            span_bars
        );
        downbeats = merged;
    }

    let peak_logits: Vec<f64> = beat_frames
        .iter()
        .map(|frame| {
            let i = frame.round() as isize;
            if i >= 0 && (i as usize) < beat_logits.len() {
                beat_logits[i as usize] as f64
            } else {
                0.0
            }
        })
        .collect();

    Some(Grid {
        bpm,
        beat_interval: 60.0 / bpm,
        first_beat: beats[0],
        beat_confidence: grid_confidence(&beats, &peak_logits),
        beats,
        downbeats,
    })
}

fn infer(values: &[f32], frames: usize) -> Option<(Vec<f32>, Vec<f32>)> {
    models::with_beat(|model| infer_chunks(model, values, frames)).flatten()
}

fn infer_chunks(
    model: &rten::Model,
    values: &[f32],
    frames: usize,
) -> Option<(Vec<f32>, Vec<f32>)> {
    let mels = BEAT_SPECTROGRAM_MELS;
    let input_id = *model.input_ids().first()?;
    let stride = CHUNK_FRAMES - 2 * BORDER_FRAMES;
    let mut beat_logits = vec![0.0f32; frames];
    let mut downbeat_logits = vec![0.0f32; frames];
    let mut start = 0usize;
    while start < frames {
        let length = CHUNK_FRAMES.min(frames - start);
        if length <= 2 * BORDER_FRAMES && start > 0 {
            break;
        }
        let chunk = values[start * mels..(start + length) * mels].to_vec();
        let tensor = NdTensor::from_data([1, length, mels], chunk);
        let outputs = model
            .run(vec![(input_id, tensor.into())], model.output_ids(), None)
            .ok()?;
        if outputs.len() < 2 {
            return None;
        }
        let beat = flatten_f32(&outputs[0])?;
        let downbeat = flatten_f32(&outputs[1])?;
        let keep_from = if start == 0 { 0 } else { BORDER_FRAMES };
        let keep_to = if start + length >= frames {
            length
        } else {
            length.saturating_sub(BORDER_FRAMES)
        };
        for index in keep_from..keep_to {
            let target = start + index;
            if target >= frames {
                break;
            }
            if index < beat.len() {
                beat_logits[target] = beat[index];
            }
            if index < downbeat.len() {
                downbeat_logits[target] = downbeat[index];
            }
        }
        if start + length >= frames {
            break;
        }
        start += stride;
    }
    Some((beat_logits, downbeat_logits))
}

fn flatten_f32(value: &rten::Value) -> Option<Vec<f32>> {
    super::models::flatten_f32(value)
}

/// Frame indices that are local maxima over [PEAK_WINDOW] and positive.
pub fn pick_peaks(logits: &[f32]) -> Vec<f64> {
    let half = PEAK_WINDOW / 2;
    let mut peaks = Vec::new();
    for index in 0..logits.len() {
        if logits[index] <= 0.0 {
            continue;
        }
        let mut is_maximum = true;
        for offset in -half..=half {
            let neighbour = index as i32 + offset;
            if neighbour < 0 || neighbour as usize >= logits.len() {
                continue;
            }
            if logits[neighbour as usize] > logits[index] {
                is_maximum = false;
                break;
            }
        }
        if is_maximum {
            peaks.push(index as i32);
        }
    }

    let mut deduped = Vec::new();
    let mut index = 0usize;
    while index < peaks.len() {
        let mut mean = peaks[index] as f64;
        let mut count = 1.0;
        while index + 1 < peaks.len() && (peaks[index + 1] as f64 - mean) <= 1.0 {
            index += 1;
            count += 1.0;
            mean += (peaks[index] as f64 - mean) / count;
        }
        deduped.push(mean.round() as usize);
        index += 1;
    }

    // A busy section can make almost every frame a local maximum, which halves
    // the median gap below and reads as an octave error or as a tempo out of
    // range. Two beats of any tempo the planner accepts are 13 frames apart at
    // the fastest (220 BPM), so anything closer than six frames is either one
    // beat counted twice or a spurious maximum — keep the stronger of the pair.
    let mut spaced: Vec<usize> = Vec::with_capacity(deduped.len());
    for frame in deduped {
        match spaced.last().copied() {
            Some(previous) if (frame as i32 - previous as i32) < PEAK_MIN_SPACING => {
                if logits[frame] > logits[previous] {
                    spaced.pop();
                    spaced.push(frame);
                }
            }
            _ => spaced.push(frame),
        }
    }

    spaced
        .into_iter()
        .map(|frame| {
            if frame == 0 || frame + 1 >= logits.len() {
                return frame as f64;
            }
            let left = logits[frame - 1] as f64;
            let centre = logits[frame] as f64;
            let right = logits[frame + 1] as f64;
            let denominator = left - 2.0 * centre + right;
            if denominator.abs() <= 1e-9 {
                return frame as f64;
            }
            frame as f64 + (0.5 * (left - right) / denominator).clamp(-0.5, 0.5)
        })
        .collect()
}

pub fn tempo_from_beats(beats: &[f64]) -> f64 {
    if beats.len() < MIN_BEATS {
        return 0.0;
    }
    let gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    let rough = median(&gaps);
    if rough <= 0.0 {
        return 0.0;
    }
    let kept: Vec<f64> = gaps
        .iter()
        .copied()
        .filter(|g| (g - rough).abs() <= rough * 0.2)
        .collect();
    let interval = median(if kept.len() >= 4 { &kept } else { &gaps });
    if interval <= 0.0 {
        return 0.0;
    }
    let bpm = 60.0 / interval;
    if (MIN_TEMPO..=MAX_TEMPO).contains(&bpm) {
        bpm
    } else {
        0.0
    }
}

pub fn grid_confidence(beats: &[f64], peak_logits: &[f64]) -> f64 {
    if beats.len() < MIN_BEATS {
        return 0.0;
    }
    let gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    let interval = median(&gaps);
    if interval <= 0.0 {
        return 0.0;
    }
    let regular = gaps
        .iter()
        .filter(|g| (*g - interval).abs() <= interval * 0.1)
        .count() as f64
        / gaps.len() as f64;
    let strength = 1.0 / (1.0 + (-(median(peak_logits) - 0.5)).exp());
    (0.35 + 0.4 * regular + 0.25 * strength).clamp(0.0, 0.95)
}

/// F-2's missing-bar derivation.
///
/// Each trusted model downbeat anchors a chain: four measured beats later is
/// the next bar, and so on. A step is only accepted when all four gaps in the
/// group are within 15 ms of the grid (the model's own beat interval), so a
/// missing or smeared beat stops the chain instead of inventing a bar; the
/// next trusted model downbeat starts a new one. Both directions are walked so
/// the bars before an anchor are filled too.
fn derive_bars(beats: &[f64], interval: f64, trusted: &[f64]) -> Vec<f64> {
    if beats.len() < 5 || interval <= 0.0 {
        return Vec::new();
    }
    let regular = |start: usize| -> bool {
        if start + 4 >= beats.len() {
            return false;
        }
        (0..4).all(|k| ((beats[start + k + 1] - beats[start + k]) - interval).abs() <= 0.015)
    };
    let nearest = |time: f64| -> Option<usize> {
        beats
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (*a - time).abs().total_cmp(&(*b - time).abs()))
            .and_then(|(index, beat)| ((beat - time).abs() <= 0.03).then_some(index))
    };
    let mut bars = Vec::new();
    for &anchor in trusted {
        let Some(anchor_index) = nearest(anchor) else {
            continue;
        };
        bars.push(beats[anchor_index]);
        let mut forward = anchor_index;
        while regular(forward) {
            forward += 4;
            bars.push(beats[forward]);
        }
        let mut back = anchor_index;
        while back >= 4 && regular(back - 4) {
            back -= 4;
            bars.push(beats[back]);
        }
    }
    bars.sort_by(|a, b| a.total_cmp(b));
    bars.dedup_by(|a, b| (*a - *b).abs() < 0.05);
    bars
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    sorted[sorted.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_model_downbeats_are_filled_from_measured_beats() {
        // 120 BPM, a beat every 0.5 s for 40 bars; the model found only two
        // bars (0 s and 16 s). The chain should fill every 2 s bar.
        let beats: Vec<f64> = (0..161).map(|i| i as f64 * 0.5).collect();
        let trusted = vec![0.0, 16.0];
        let bars = derive_bars(&beats, 0.5, &trusted);
        assert!(bars.len() >= 40, "derived {} bars", bars.len());
        assert!((bars[20] - 40.0).abs() < 1e-6, "bar 20 at {}", bars[20]);
        assert!(bars.windows(2).all(|w| (w[1] - w[0] - 2.0).abs() < 1e-6));
    }

    #[test]
    fn a_missing_beat_stops_the_chain() {
        let mut beats: Vec<f64> = (0..81).map(|i| i as f64 * 0.5).collect();
        // Remove the beat at 10.0 s: the bars around it cannot be measured.
        beats.retain(|b| (*b - 10.0).abs() > 1e-6);
        let bars = derive_bars(&beats, 0.5, &[0.0]);
        assert!(bars.iter().all(|b| *b < 9.0 || *b > 11.0), "{bars:?}");
        assert!(bars.len() < 40, "the chain should stop at the gap");
    }

    #[test]
    fn a_drifting_beat_stops_the_chain() {
        let mut beats: Vec<f64> = (0..81).map(|i| i as f64 * 0.5).collect();
        // A 30 ms displacement at 20 s breaks the 15 ms step check.
        for beat in beats.iter_mut() {
            if *beat >= 20.0 {
                *beat += 0.03;
            }
        }
        let bars = derive_bars(&beats, 0.5, &[0.0, 40.0]);
        assert!(bars.iter().all(|b| (*b - 20.0).abs() > 1e-6), "{bars:?}");
    }

    #[test]
    fn tempo_from_regular_120_bpm_grid() {
        let beats: Vec<f64> = (0..16).map(|i| i as f64 * 0.5).collect();
        let bpm = tempo_from_beats(&beats);
        assert!((bpm - 120.0).abs() < 0.5, "bpm {bpm}");
        let conf = grid_confidence(&beats, &vec![2.0; beats.len()]);
        assert!(conf > 0.7, "conf {conf}");
    }

    fn grid_at(bpm: f64, beats: usize) -> Grid {
        let interval = 60.0 / bpm;
        Grid {
            beats: (0..beats).map(|i| i as f64 * interval).collect(),
            downbeats: (0..beats / 4).map(|i| i as f64 * interval * 4.0).collect(),
            bpm,
            beat_interval: interval,
            first_beat: 0.0,
            beat_confidence: 0.8,
        }
    }

    #[test]
    fn a_dense_beat_set_keeps_at_most_one_peak_per_six_frames() {
        // Every frame positive is the pathological shape the whole-track pass
        // produced on a busy section: without a spacing rule every frame is a
        // "peak", the median gap collapses and the tempo reads far too fast.
        let logits = vec![1.0f32; 300];
        let peaks = pick_peaks(&logits);
        assert!(peaks.len() <= 300 / 6 + 1, "{} peaks", peaks.len());
        for pair in peaks.windows(2) {
            assert!(
                pair[1] - pair[0] >= 6.0,
                "peaks {} and {} are closer than the spacing rule allows",
                pair[0],
                pair[1]
            );
        }
        let seconds: Vec<f64> = peaks.iter().map(|f| f / FRAME_RATE).collect();
        assert_eq!(tempo_from_beats(&seconds), 0.0, "a dense set is not a tempo");
    }

    #[test]
    fn a_clean_grid_keeps_every_beat() {
        // 120 BPM is 25 frames apart at 50 fps, so the spacing rule must not
        // touch a real grid.
        let mut logits = vec![0.0f32; 400];
        for index in (0..400).step_by(25) {
            logits[index] = 2.0;
        }
        let peaks: Vec<f64> = pick_peaks(&logits).iter().map(|f| f / FRAME_RATE).collect();
        assert_eq!(peaks.len(), 16);
        assert!((tempo_from_beats(&peaks) - 120.0).abs() < 1.0);
    }

    #[test]
    fn an_unusable_whole_grid_falls_back_to_the_head_window() {
        let head = grid_at(128.0, 8);
        let chosen = best_grid(None, || Some(head.clone()));
        assert!(chosen.is_some(), "the head grid must be kept");
        assert!((chosen.unwrap().bpm - 128.0).abs() < 1e-9);
    }

    #[test]
    fn a_usable_whole_grid_is_not_second_guessed() {
        let whole = grid_at(93.4, 16);
        let chosen = best_grid(Some(whole), || panic!("the head must not be consulted"));
        assert!((chosen.unwrap().bpm - 93.4).abs() < 1e-9);
    }
}
