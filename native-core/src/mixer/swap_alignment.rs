//! Bounded PCM alignment before a same-recording crossfade. Work runs on the
//! mixer worker, never in the device callback, and previews remain buffered.

const WINDOW_SECONDS: f64 = 0.20;
pub(super) const MAX_OFFSET_SECONDS: f64 = 0.15;
const MIN_CORRELATION: f64 = 0.85;

/// Candidate frame matching reference frame zero. Search a small neighbourhood
/// of the nominal timeline, then verify both halves at full output precision.
fn matching_offset(
    reference: &[f32],
    candidate: &[f32],
    nominal: usize,
    rate: u32,
) -> Option<usize> {
    let frames = (rate as f64 * WINDOW_SECONDS) as usize;
    let radius = (rate as f64 * MAX_OFFSET_SECONDS) as usize;
    if frames < 2 || reference.len() < frames * 2 || candidate.len() < frames * 2 {
        return None;
    }
    let start = nominal.saturating_sub(radius);
    let end = (nominal + radius).min(candidate.len() / 2 - frames);
    if start > end {
        return None;
    }
    // Average blocks for a cheap anti-aliased coarse search; retain stereo so
    // opposite-polarity channels cannot cancel into a silent mono reference.
    let step = (rate as usize / 6_000).max(1);
    let reference_coarse = block_average(&reference[..frames * 2], step);
    let candidate_coarse = block_average(candidate, step);
    let mut peaks = Vec::new();
    for offset in (start / step)..=(end / step) {
        let begin = offset * 2;
        let finish = begin + reference_coarse.len();
        if finish > candidate_coarse.len() {
            break;
        }
        if let Some(score) = correlation(&reference_coarse, &candidate_coarse[begin..finish]) {
            peaks.push((offset * step, score));
        }
    }
    peaks.sort_unstable_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| a.0.abs_diff(nominal).cmp(&b.0.abs_diff(nominal)))
    });
    // Include the nominal position even when a periodic waveform has many peaks.
    let centers = std::iter::once(nominal).chain(peaks.iter().take(8).map(|p| p.0));
    let mut best = None;
    let mut best_score = MIN_CORRELATION;
    let reference = &reference[..frames * 2];
    for center in centers {
        for offset in center.saturating_sub(step).max(start)..=(center + step).min(end) {
            let candidate = &candidate[offset * 2..(offset + frames) * 2];
            let Some(score) = correlation(reference, candidate) else {
                continue;
            };
            let closer = best.is_none_or(|previous: usize| {
                offset.abs_diff(nominal) < previous.abs_diff(nominal)
            });
            if score >= MIN_CORRELATION
                && (score > best_score + 1e-6 || ((score - best_score).abs() <= 1e-6 && closer))
            {
                let half = frames / 2 * 2;
                if correlation(&reference[..half], &candidate[..half]).unwrap_or(0.0)
                    < MIN_CORRELATION
                    || correlation(&reference[half..], &candidate[half..]).unwrap_or(0.0)
                        < MIN_CORRELATION
                {
                    continue;
                }
                best = Some(offset);
                best_score = score;
            }
        }
    }
    best
}

fn block_average(samples: &[f32], frames: usize) -> Vec<f32> {
    samples
        .chunks_exact(frames * 2)
        .flat_map(|chunk| {
            let mut sum = [0.0; 2];
            for pair in chunk.chunks_exact(2) {
                sum[0] += pair[0];
                sum[1] += pair[1];
            }
            [sum[0] / frames as f32, sum[1] / frames as f32]
        })
        .collect()
}

fn correlation(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let mut aa = 0.0;
    let mut ab = 0.0;
    let mut bb = 0.0;
    for (&a, &b) in a.iter().zip(b) {
        aa += f64::from(a).powi(2);
        ab += f64::from(a) * f64::from(b);
        bb += f64::from(b).powi(2);
    }
    (aa > 1e-10 && bb > 1e-10).then(|| ab / (aa * bb).sqrt())
}

pub(super) fn align(
    current: &mut super::voice::Voice,
    incoming: &mut super::voice::Voice,
    rate: u32,
) -> Result<(), String> {
    let speed = current.effective_speed as f64;
    if current.bridge.is_some() || (current.plan_rate - 1.0).abs() > 1e-4 {
        return Err("source swap cannot align an active tempo transition".into());
    }
    let nominal_seconds = current.emitted_position_seconds() - incoming.emitted_position_seconds();
    let nominal = (nominal_seconds.max(0.0) / speed * rate as f64).round() as usize;
    let window = (WINDOW_SECONDS * rate as f64) as usize;
    let radius = (MAX_OFFSET_SECONDS * rate as f64) as usize;
    current.buffer_preview(window, rate);
    incoming.buffer_preview(nominal + radius + window, rate);
    let reference = &current.pending_dev[current.pending_dev_cursor..];
    let candidate = &incoming.pending_dev[incoming.pending_dev_cursor..];
    let Some(offset) = matching_offset(reference, candidate, nominal, rate) else {
        return Err("source swap rejected: no confident PCM alignment within 150ms".into());
    };
    incoming.pending_dev_cursor += offset * 2;
    log::info!(
        "source swap aligned: correction={:.3}ms",
        (offset as f64 - nominal as f64) / rate as f64 * 1000.0
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn noise(frames: usize, seed: u32) -> Vec<f32> {
        let mut x = seed;
        (0..frames * 2)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as i32 as f64 / i32::MAX as f64 * 0.3) as f32
            })
            .collect()
    }
    #[test]
    fn finds_positive_and_negative_submillisecond_and_codec_delays() {
        for rate in [44_100, 48_000, 96_000] {
            let nominal = (rate as f64 * MAX_OFFSET_SECONDS) as usize;
            let candidate = noise(rate as usize, 71);
            for delay in [-400, -17, 0, 29, 731] {
                let expected = (nominal as isize + delay) as usize;
                let reference: Vec<_> = candidate[expected * 2..].iter().map(|s| s * 0.6).collect();
                assert_eq!(
                    matching_offset(&reference, &candidate, nominal, rate),
                    Some(expected),
                    "rate={rate} delay={delay}"
                );
            }
        }
    }
    #[test]
    fn rejects_other_recordings_silence_and_offsets_outside_the_bound() {
        let rate = 44_100;
        let candidate = noise(rate as usize, 71);
        assert_eq!(
            matching_offset(&noise(rate as usize, 99), &candidate, 0, rate),
            None
        );
        assert_eq!(
            matching_offset(&vec![0.0; rate as usize], &candidate, 0, rate),
            None
        );
        assert_eq!(
            matching_offset(&candidate[20_000..], &candidate, 0, rate),
            None
        );
        assert_eq!(matching_offset(&candidate, &[], 0, rate), None);
    }
}
