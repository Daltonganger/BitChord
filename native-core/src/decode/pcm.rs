//! PCM conversions: stereo downmix and interleaved copies.

use super::DecodeError;
use symphonia::core::audio::{Audio, Channels, GenericAudioBufferRef, Position};

/// Normalized Lo/Ro fold: fronts at unity, centre and surround at -3 dB.
/// LFE is omitted (it is an effects channel, not a full-range bass channel).
/// Dividing by each row's absolute coefficient sum prevents correlated
/// surround channels from overflowing, without clipping the float signal.
pub(super) fn downmix_stereo(input: &[f32], layout: &Channels) -> Result<Vec<f32>, DecodeError> {
    let channels = layout.count();
    if channels == 0 || input.len() % channels != 0 {
        return Err(DecodeError("invalid decoded channel layout".into()));
    }
    if channels == 2
        && (matches!(layout, Channels::Discrete(2))
            || matches!(layout, Channels::Positioned(p) if *p == (Position::FRONT_LEFT | Position::FRONT_RIGHT)))
    {
        return Ok(input.to_vec());
    }
    if channels == 1 {
        return Ok(input.iter().flat_map(|s| [*s, *s]).collect());
    }
    let Channels::Positioned(positions) = layout else {
        return Err(DecodeError(
            "surround downmix requires positioned channels".into(),
        ));
    };
    let q = std::f32::consts::FRAC_1_SQRT_2;
    let left = Position::REAR_LEFT
        | Position::SIDE_LEFT
        | Position::FRONT_LEFT_CENTER
        | Position::TOP_FRONT_LEFT
        | Position::TOP_REAR_LEFT
        | Position::TOP_SIDE_LEFT
        | Position::BOTTOM_FRONT_LEFT
        | Position::FRONT_LEFT_WIDE;
    let right = Position::REAR_RIGHT
        | Position::SIDE_RIGHT
        | Position::FRONT_RIGHT_CENTER
        | Position::TOP_FRONT_RIGHT
        | Position::TOP_REAR_RIGHT
        | Position::TOP_SIDE_RIGHT
        | Position::BOTTOM_FRONT_RIGHT
        | Position::FRONT_RIGHT_WIDE;
    let center = Position::FRONT_CENTER
        | Position::REAR_CENTER
        | Position::TOP_CENTER
        | Position::TOP_FRONT_CENTER
        | Position::TOP_REAR_CENTER
        | Position::BOTTOM_FRONT_CENTER;
    let weights: Vec<(f32, f32)> = positions
        .iter()
        .map(|p| {
            Ok(if p == Position::FRONT_LEFT {
                (1.0, 0.0)
            } else if p == Position::FRONT_RIGHT {
                (0.0, 1.0)
            } else if left.contains(p) {
                (q, 0.0)
            } else if right.contains(p) {
                (0.0, q)
            } else if p == Position::LFE1 || p == Position::LFE2 {
                (0.0, 0.0)
            } else if center.contains(p) {
                (q, q)
            } else {
                return Err(DecodeError(format!("unsupported channel position: {p:?}")));
            })
        })
        .collect::<Result<_, DecodeError>>()?;
    let norm_l = weights.iter().map(|w| w.0).sum::<f32>().max(1.0);
    let norm_r = weights.iter().map(|w| w.1).sum::<f32>().max(1.0);
    Ok(input
        .chunks_exact(channels)
        .flat_map(|frame| {
            let l = frame
                .iter()
                .zip(&weights)
                .map(|(s, w)| s * w.0)
                .sum::<f32>()
                / norm_l;
            let r = frame
                .iter()
                .zip(&weights)
                .map(|(s, w)| s * w.1)
                .sum::<f32>()
                / norm_r;
            [l, r]
        })
        .collect())
}

pub(super) fn copy_interleaved_f32(buffer: &GenericAudioBufferRef<'_>, dst: &mut Vec<f32>) {
    match buffer {
        GenericAudioBufferRef::F32(buf) => buf.copy_to_vec_interleaved(dst),
        GenericAudioBufferRef::F64(buf) => {
            let mut tmp: Vec<f64> = Vec::new();
            buf.copy_to_vec_interleaved(&mut tmp);
            dst.extend(tmp.iter().map(|v| *v as f32));
        }
        _ => buffer.copy_to_vec_interleaved(dst),
    }
}

// ---------------------------------------------------------------------------
// Growing file (Ktor still writing the rest)
// ---------------------------------------------------------------------------
