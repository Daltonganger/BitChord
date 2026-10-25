//! The device callback's rendering: float output, and bit-perfect integer
//! output with its bail ramp.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// Quantizes a normalized f32 to signed 16-bit, matching upstream
/// `PcmBoundary.clamp16FromFloat` exactly: scale by 32768 (asymmetric full
/// scale, so −1.0 → −32768 and +1.0 → +32767), and Java `Math.round`'s
/// half-toward-positive-infinity rounding rather than Rust's half-away-from-zero.
pub(super) fn clamp16_from_float(f: f32) -> i16 {
    if f.is_nan() {
        return 0;
    }
    let scaled = f * 32768.0f32;
    if scaled <= -32768.0 {
        return i16::MIN;
    }
    if scaled >= 32767.0 {
        return i16::MAX;
    }
    (scaled + 0.5).floor() as i16
}

/// Exact inverse of Symphonia's signed PCM normalization for integer formats
/// up to 24 bits. Eligible samples are integer-derived f32 values, so scaling
/// by the matching power of two recovers the original codeword without
/// rounding or dither.
pub(super) fn exact_f32_to_i16(sample: f32) -> i16 {
    if sample <= -1.0 {
        return i16::MIN;
    }
    if sample >= 1.0 {
        return i16::MAX;
    }
    (sample * 32768.0) as i16
}

pub(super) fn exact_f32_to_i24(sample: f32) -> i32 {
    if sample <= -1.0 {
        return -(1 << 23);
    }
    if sample >= 1.0 {
        return (1 << 23) - 1;
    }
    (sample * 8_388_608.0) as i32
}

/// Direct ring drain used only by the integer bit-perfect stream. No volume,
/// fade, remapping, or other sample arithmetic happens here.
pub(super) fn render_bitperfect(
    consumer: &mut rtrb::Consumer<f32>,
    buffered: &Arc<AtomicU64>,
    callback_underruns: &Arc<AtomicU64>,
    output_peak: &Arc<AtomicU32>,
    flush_ring: &Arc<AtomicBool>,
    bail_flush: &Arc<AtomicBool>,
    paused: &Arc<AtomicBool>,
    data: &mut [f32],
) {
    if flush_ring.load(Ordering::Acquire) || bail_flush.load(Ordering::Acquire) {
        while consumer.pop().is_ok() {}
        buffered.store(0, Ordering::Relaxed);
        flush_ring.store(false, Ordering::Release);
        bail_flush.store(false, Ordering::Release);
    }
    if paused.load(Ordering::Acquire) {
        data.fill(0.0);
        output_peak.store(0.0f32.to_bits(), Ordering::Relaxed);
        return;
    }
    let mut underflowed = false;
    let mut peak = 0.0f32;
    let mut consumed = 0u64;
    for frame in data.chunks_exact_mut(2) {
        // The producer publishes scalar samples. Wait for both channels so an
        // underrun between L and R cannot permanently rotate stereo framing.
        if consumer.slots() >= 2 {
            frame[0] = consumer.pop().unwrap();
            frame[1] = consumer.pop().unwrap();
            consumed += 1;
        } else {
            underflowed = true;
            frame.fill(0.0);
        }
        peak = peak.max(frame[0].abs()).max(frame[1].abs());
    }
    if data.len() % 2 != 0 {
        *data.last_mut().unwrap() = 0.0;
    }
    output_peak.store(peak.to_bits(), Ordering::Relaxed);
    if underflowed {
        callback_underruns.fetch_add(1, Ordering::Relaxed);
    }
    let mut current = buffered.load(Ordering::Relaxed);
    while current > 0 {
        let next = current.saturating_sub(consumed);
        match buffered.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

#[derive(Default)]
pub(super) struct BailRamp {
    pub(super) app_gain: f32,
    pub(super) recovery_gain: f32,
    pub(super) last_frame: [f32; 2],
    pub(super) primed: bool,
    pub(super) prebuffer: bool,
    pub(super) waited_frames: usize,
    pub(super) buffer: Vec<f32>,
    pub(super) cursor: usize,
    pub(super) total_frames: usize,
}

/// Drains the mixer's ring into `data`: volume applied, underruns zero-filled,
/// leftover-ring flush, 120 ms deferred bail ramp-out, and pause-silence handled.
/// The one render path both output formats share — the word length is a property
/// of the unit, not of the mix.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_f32(
    consumer: &mut rtrb::Consumer<f32>,
    volume: &Arc<AtomicU32>,
    buffered: &Arc<AtomicU64>,
    callback_underruns: &Arc<AtomicU64>,
    output_peak: &Arc<AtomicU32>,
    flush_ring: &Arc<AtomicBool>,
    bail_flush: &Arc<AtomicBool>,
    paused: &Arc<AtomicBool>,
    bail_ramp: &mut BailRamp,
    sample_rate: u32,
    channels: usize,
    data: &mut [f32],
) {
    if flush_ring.load(Ordering::Acquire) {
        bail_ramp.buffer.clear();
        bail_ramp.cursor = 0;
        bail_ramp.total_frames = 0;
        while consumer.pop().is_ok() {}
        buffered.store(0, Ordering::Relaxed);
        bail_ramp.primed = false;
        bail_ramp.recovery_gain = 0.0;
        bail_ramp.waited_frames = 0;
        flush_ring.store(false, Ordering::Release);
    }
    if bail_flush.load(Ordering::Acquire) {
        // 120 ms ramp-down (matching upstream BAIL_MS).
        // The mixer ring always contains interleaved stereo (2 channels).
        let bail_frames = ((sample_rate as usize * 120) / 1000).max(1);
        let bail_samples = bail_frames * 2;
        bail_ramp.buffer.clear();
        bail_ramp.cursor = 0;
        for _ in 0..bail_samples {
            match consumer.pop() {
                Ok(sample) => bail_ramp.buffer.push(sample),
                Err(_) => break,
            }
        }
        bail_ramp.total_frames = (bail_ramp.buffer.len() / 2).max(1);
        // Deferred flush: discard the remaining old audio backlog in the ring,
        // so the new track can enter an empty ring without playing seconds of the old track.
        while consumer.pop().is_ok() {}
        buffered.store(0, Ordering::Relaxed);
        bail_flush.store(false, Ordering::Release);
    }
    if paused.load(Ordering::Acquire) {
        // Keep the queued music intact and bring the last audible frame to
        // silence over 2 ms. The same recovery envelope ramps back on resume.
        let step = 1.0 / (sample_rate.max(1) as f32 * 0.002);
        let mut peak = 0.0f32;
        for frame in data.chunks_exact_mut(channels.max(1)) {
            bail_ramp.recovery_gain = (bail_ramp.recovery_gain - step).max(0.0);
            let gain = bail_ramp.app_gain * bail_ramp.recovery_gain;
            frame.fill(0.0);
            if channels == 1 {
                frame[0] = (bail_ramp.last_frame[0] + bail_ramp.last_frame[1]) * 0.5 * gain;
            } else {
                frame[0] = bail_ramp.last_frame[0] * gain;
                frame[1] = bail_ramp.last_frame[1] * gain;
            }
            for value in frame {
                peak = peak.max(value.abs());
            }
        }
        output_peak.store(peak.to_bits(), Ordering::Relaxed);
        return;
    }
    let vol = f32::from_bits(volume.load(Ordering::Relaxed)).clamp(0.0, 1.0);
    if bail_ramp.prebuffer && !bail_ramp.primed && bail_ramp.buffer.is_empty() {
        let queued = buffered.load(Ordering::Relaxed);
        if queued > 0 {
            bail_ramp.waited_frames += data.len() / channels.max(1);
        }
        if queued < sample_rate as u64 * 150 / 1000
            && (queued == 0 || bail_ramp.waited_frames < sample_rate as usize * 150 / 1000)
        {
            data.fill(0.0);
            return;
        }
        bail_ramp.primed = true;
    }
    let mut underflowed = false;
    let mut consumed_frames = 0u64;
    for frame in data.chunks_exact_mut(channels.max(1)) {
        let mut stereo = [0.0; 2];
        let mut missing = false;
        let mut from_ring = false;
        let pair_available = consumer.slots() >= 2;
        for sample in &mut stereo {
            if bail_ramp.cursor < bail_ramp.buffer.len() {
                let progress = (bail_ramp.cursor / 2) as f64 / bail_ramp.total_frames.max(1) as f64;
                *sample = bail_ramp.buffer[bail_ramp.cursor]
                    * (progress * core::f64::consts::PI / 2.0).cos() as f32;
                bail_ramp.cursor += 1;
                if bail_ramp.cursor == bail_ramp.buffer.len() {
                    bail_ramp.last_frame = [0.0; 2];
                    bail_ramp.recovery_gain = 0.0;
                }
            } else if pair_available {
                match consumer.pop() {
                    Ok(s) => {
                        *sample = s;
                        from_ring = true;
                    }
                    Err(_) => missing = true,
                }
            } else {
                missing = true;
            }
        }
        if from_ring {
            consumed_frames += 1;
        }
        let recovery_step = 1.0 / (sample_rate.max(1) as f32 * 0.002);
        if missing {
            underflowed = true;
            bail_ramp.recovery_gain = (bail_ramp.recovery_gain - recovery_step).max(0.0);
            stereo = bail_ramp.last_frame;
        } else {
            bail_ramp.last_frame = stereo;
            bail_ramp.recovery_gain = (bail_ramp.recovery_gain + recovery_step).min(1.0);
        }
        let gain_step = 1.0 / (sample_rate.max(1) as f32 * 0.010);
        bail_ramp.app_gain += (vol - bail_ramp.app_gain).clamp(-gain_step, gain_step);
        let gain = bail_ramp.app_gain * bail_ramp.recovery_gain;
        frame.fill(0.0);
        if channels == 1 {
            frame[0] = (stereo[0] + stereo[1]) * 0.5 * gain;
        } else {
            frame[0] = stereo[0] * gain;
            frame[1] = stereo[1] * gain;
        }
    }
    // What actually left the callback. Zero here with a non-zero ring means the
    // data is silent; non-zero here with no audible output means the stream is
    // not reaching the speaker.
    let mut peak = 0.0f32;
    for sample in data.iter() {
        let magnitude = sample.abs();
        if magnitude > peak {
            peak = magnitude;
        }
    }
    output_peak.store(peak.to_bits(), Ordering::Relaxed);
    if underflowed {
        callback_underruns.fetch_add(1, Ordering::Relaxed);
    }
    let consumed = consumed_frames;
    let mut current = buffered.load(Ordering::Relaxed);
    while current > 0 {
        let next = current.saturating_sub(consumed);
        match buffered.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}
