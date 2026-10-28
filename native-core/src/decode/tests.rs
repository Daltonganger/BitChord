use super::codec::{
    aac_audio_object_type, alac_bit_depth, is_he_aac_config, lc_stereo_asc, open_audio_decoder,
};
use super::growing_file::GrowingFile;
use super::http_source::StreamBuffer;
use super::pcm::downmix_stereo;
use symphonia::core::audio::{Channels, Position};
use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;

#[test]
fn alac_source_depth_handles_raw_and_wrapped_cookies() {
    let mut raw = vec![0u8; 24];
    raw[5] = 24;
    assert_eq!(alac_bit_depth(&raw), Some(24));
    let mut wrapped = vec![0u8; 12];
    wrapped[4..8].copy_from_slice(b"alac");
    wrapped.extend_from_slice(&raw);
    assert_eq!(alac_bit_depth(&wrapped), Some(24));
    let mut both = vec![0u8; 12];
    both[4..8].copy_from_slice(b"frma");
    both.extend_from_slice(&wrapped);
    assert_eq!(alac_bit_depth(&both), Some(24));
    for length in 0..24 {
        assert_eq!(alac_bit_depth(&raw[..length]), None);
    }
    raw[5] = 0;
    assert_eq!(alac_bit_depth(&raw), None);
}

use super::*;

/// A minimal Matroska/WebM header: an EBML header, a Segment whose Info
/// carries the length, one Opus track, and an empty cluster.
///
/// This is the shape the app's stream cache holds — Opus in WebM under an
/// `.m4a` name — and the shape that used to report no duration at all: the
/// MKV reader declares the segment's length on `MediaInfo`
/// (`symphonia-format-mkv/src/demuxer.rs`, `with_duration`) and leaves the
/// *track's* duration unset, which is the field this decoder looked at.
/// Nothing is decoded here, so the bytes are generated rather than a
/// recording committed to the repository.
fn webm_opus_bytes(duration_seconds: Option<f32>, timestamp_scale: u32) -> Vec<u8> {
    // EBML variable-length size: a leading marker bit, then the value.
    fn size(len: usize) -> Vec<u8> {
        match len {
            0..=0x7E => vec![0x80 | len as u8],
            0x7F..=0x3FFE => vec![0x40 | (len >> 8) as u8, len as u8],
            _ => vec![0x20 | (len >> 16) as u8, (len >> 8) as u8, len as u8],
        }
    }
    fn element(id: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend(size(payload.len()));
        out.extend_from_slice(payload);
        out
    }
    fn uint(value: u64) -> Vec<u8> {
        let bytes = value.to_be_bytes();
        let first = bytes.iter().position(|byte| *byte != 0).unwrap_or(7);
        bytes[first..].to_vec()
    }

    let mut header = Vec::new();
    header.extend(element(&[0x42, 0x86], &uint(1))); // EBMLVersion
    header.extend(element(&[0x42, 0xF7], &uint(1))); // EBMLReadVersion
    header.extend(element(&[0x42, 0xF2], &uint(4))); // EBMLMaxIDLength
    header.extend(element(&[0x42, 0xF3], &uint(8))); // EBMLMaxSizeLength
    header.extend(element(&[0x42, 0x82], b"webm")); // DocType
    header.extend(element(&[0x42, 0x87], &uint(4))); // DocTypeVersion
    header.extend(element(&[0x42, 0x85], &uint(2))); // DocTypeReadVersion
    let header = element(&[0x1A, 0x45, 0xDF, 0xA3], &header);

    let mut info = Vec::new();
    // TimestampScale is a count of nanoseconds per tick, so a Duration is
    // expressed in these ticks — 1 ms by default, 2 ms in the case that
    // pins the decoder reading the time base rather than assuming it.
    info.extend(element(&[0x2A, 0xD7, 0xB1], &uint(timestamp_scale as u64))); // TimestampScale
    info.extend(element(&[0x4D, 0x80], b"bitchord-test")); // MuxingApp
    info.extend(element(&[0x57, 0x41], b"bitchord-test")); // WritingApp
    if let Some(seconds) = duration_seconds {
        let ticks = seconds * 1e9 / timestamp_scale as f32;
        info.extend(element(&[0x44, 0x89], &ticks.to_be_bytes())); // Duration
    }
    let info = element(&[0x15, 0x49, 0xA9, 0x66], &info);

    let mut audio = Vec::new();
    audio.extend(element(&[0xB5], &48_000f64.to_be_bytes())); // SamplingFrequency
    audio.extend(element(&[0x9F], &uint(2))); // Channels

    // OpusHead: version 1, two channels, 312 frames of pre-skip at 48 kHz,
    // no output gain, mapping family 0.
    let mut opus_head = b"OpusHead".to_vec();
    opus_head.extend([1, 2]);
    opus_head.extend(312u16.to_le_bytes());
    opus_head.extend(48_000u32.to_le_bytes());
    opus_head.extend([0, 0, 0]);

    let mut track = Vec::new();
    track.extend(element(&[0xD7], &uint(1))); // TrackNumber
    track.extend(element(&[0x73, 0xC5], &uint(1))); // TrackUid
    track.extend(element(&[0x83], &uint(2))); // TrackType: audio
    track.extend(element(&[0x86], b"A_OPUS")); // CodecID
    track.extend(element(&[0x63, 0xA2], &opus_head)); // CodecPrivate
    track.extend(element(&[0xE1], &audio)); // Audio
    let tracks = element(&[0x16, 0x54, 0xAE, 0x6B], &element(&[0xAE], &track));

    // An empty cluster, so the reader has somewhere to descend after the
    // header elements it scans.
    let cluster = element(&[0xE7], &element(&[0xE7], &uint(0)));

    let mut segment = Vec::new();
    segment.extend(info);
    segment.extend(tracks);
    segment.extend(cluster);
    let segment = element(&[0x18, 0x53, 0x80, 0x67], &segment);

    let mut file = header;
    file.extend(segment);
    file
}

/// Writes generated bytes under a name whose extension says something else.
///
/// The `.m4a` name is the app's cache convention, not this fixture being
/// clever: the probe reads the bytes, so the container is found regardless,
/// and a future change that trusted the extension would break the app's
/// whole stream cache.
fn write_generated_webm(
    name: &str,
    duration_seconds: Option<f32>,
    timestamp_scale: u32,
) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("bitcord-decode-{name}.m4a"));
    std::fs::write(&path, webm_opus_bytes(duration_seconds, timestamp_scale)).unwrap();
    path
}

fn open_webm(path: &std::path::Path) -> SymphoniaDecoder {
    SymphoniaDecoder::open_available(
        &SourceKind::parse(&path.to_string_lossy()),
        &Default::default(),
    )
    .expect("a generated WebM/Opus header must open")
}

#[test]
fn opus_in_webm_reports_the_segments_length() {
    let path = write_generated_webm("segment-duration", Some(187.8), 1_000_000);
    let decoder = open_webm(&path);
    assert_eq!(decoder.sample_rate(), 48_000);
    assert_eq!(
        decoder.duration_seconds(),
        Some(187.8),
        "the segment's Duration is the only length a Matroska file declares"
    );
    drop(decoder);
    std::fs::remove_file(&path).ok();
}

#[test]
fn opus_in_webm_without_a_declared_length_reports_none() {
    // The negative side of the same rule: this decoder may not invent a
    // length for a container that declares none, because a wrong one moves
    // the mix-out anchor and the audible symptom is a track faded early.
    let path = write_generated_webm("no-duration", None, 1_000_000);
    let decoder = open_webm(&path);
    assert_eq!(decoder.duration_seconds(), None);
    drop(decoder);
    std::fs::remove_file(&path).ok();
}

#[test]
fn opus_in_webm_honours_a_non_default_timestamp_scale() {
    // The MediaInfo duration is a count of TimestampScale ticks, not
    // milliseconds: with 2 ms ticks the same 187.8 s must still read as
    // 187.8 s, which is what pins the time base being used rather than
    // assumed.
    let path = write_generated_webm("scale-2ms", Some(187.8), 2_000_000);
    let decoder = open_webm(&path);
    assert_eq!(decoder.duration_seconds(), Some(187.8));
    drop(decoder);
    std::fs::remove_file(&path).ok();
}

#[test]
fn compressed_seek_reports_the_next_audible_frame_before_preroll_is_read() {
    let source = SourceKind::parse(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/seek-tone.m4a"
    ));
    let mut decoder = SymphoniaDecoder::open(&source, &Default::default()).unwrap();
    let mut reference = SymphoniaDecoder::open(&source, &Default::default()).unwrap();
    let baseline = reference.read_stereo(44100 * 3).unwrap();
    for target in [0.19, 0.190001, 1.337] {
        decoder.seek_seconds(target).unwrap();
        let rounded =
            (target * decoder.sample_rate() as f64).round() / decoder.sample_rate() as f64;
        assert!(
            (decoder.position_seconds() - rounded).abs() < 1e-9,
            "pending preroll must not expose an earlier playback position"
        );
        let samples = decoder.read_stereo(17).unwrap();
        assert_eq!(samples.len(), 34);
        let start = (target * decoder.sample_rate() as f64).round() as usize * 2;
        assert!(
            samples
                .iter()
                .zip(&baseline[start..start + 34])
                .all(|(a, b)| (a - b).abs() < 1e-6),
            "seek must still decode preroll and return the requested PCM"
        );
        assert!(
            (decoder.position_seconds() - rounded - 17.0 / decoder.sample_rate() as f64).abs()
                < 1e-9
        );
    }
}

#[test]
fn positioned_wide_and_height_channels_keep_their_side() {
    let positions = Position::FRONT_LEFT
        | Position::FRONT_RIGHT
        | Position::FRONT_CENTER
        | Position::LFE1
        | Position::FRONT_LEFT_CENTER
        | Position::FRONT_RIGHT_CENTER
        | Position::TOP_REAR_RIGHT;
    let layout = Channels::from(positions);
    for (i, p) in positions.iter().enumerate() {
        let mut frame = vec![0.0; layout.count()];
        frame[i] = 1.0;
        let stereo = downmix_stereo(&frame, &layout).unwrap();
        if p == Position::FRONT_LEFT_CENTER {
            assert!(stereo[0] > 0.0);
            assert_eq!(stereo[1], 0.0);
        }
        if p == Position::FRONT_RIGHT_CENTER || p == Position::TOP_REAR_RIGHT {
            assert_eq!(stereo[0], 0.0);
            assert!(stereo[1] > 0.0);
        }
        if p == Position::LFE1 {
            assert_eq!(stereo, [0.0, 0.0]);
        }
    }
    let center_lfe = Channels::from(Position::FRONT_CENTER | Position::LFE1);
    assert_eq!(
        downmix_stereo(&[0.0, 1.0], &center_lfe).unwrap(),
        [0.0, 0.0]
    );
    let unity = downmix_stereo(&vec![1.0; layout.count()], &layout).unwrap();
    assert!(unity.iter().all(|v| (*v - 1.0).abs() < 1e-7));
}

#[test]
fn source_kind_parse() {
    assert!(matches!(
        SourceKind::parse("https://example.com/a.webm"),
        SourceKind::Url(_)
    ));
    assert!(matches!(
        SourceKind::parse("http://example.com/a.webm"),
        SourceKind::Url(_)
    ));
    assert!(matches!(
        SourceKind::parse("/tmp/a.flac"),
        SourceKind::Path(_)
    ));
}

#[test]
fn opus_uses_the_bundled_libopus_decoder() {
    let mut audio = symphonia::core::codecs::audio::AudioCodecParameters::new();
    audio
        .for_codec(CODEC_ID_OPUS)
        .with_sample_rate(48_000)
        .with_channels(Channels::from(Position::FRONT_LEFT | Position::FRONT_RIGHT));

    assert!(
        open_audio_decoder(&audio).is_ok(),
        "the Opus branch must construct the bundled decoder",
    );
}

#[test]
fn growing_file_waits_only_with_grow_marker() {
    assert!(!GrowingFile::is_growing("/tmp/bitchord-no-such.mp4"));
}

/// A still-downloading file ends where the bytes end, for analysis. The
/// playback opener would sit here for two minutes waiting on `.complete`.
#[test]
fn an_analysis_read_does_not_wait_for_the_rest_of_the_download() {
    let dir = std::env::temp_dir().join("bitchord-growing-analysis");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("partial.wav");
    let rate = 44_100u32;
    let frames = rate as usize / 10;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + (frames * 4) as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&((frames * 4) as u32).to_le_bytes());
    bytes.extend(std::iter::repeat(0).take(frames * 4));
    std::fs::write(&path, bytes).unwrap();
    let path_str = path.display().to_string();
    std::fs::write(format!("{path_str}.grow"), b"").unwrap();
    let _ = std::fs::remove_file(format!("{path_str}.complete"));

    let started = std::time::Instant::now();
    let mut decoder = SymphoniaDecoder::open_available(
        &SourceKind::parse(&path_str),
        &std::collections::HashMap::new(),
    )
    .unwrap();
    let mut total = 0usize;
    loop {
        let chunk = decoder.read_stereo(4096).unwrap();
        if chunk.is_empty() {
            break;
        }
        total += chunk.len();
    }
    assert!(total > 0, "the bytes already on disk should decode");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "analysis waited {:?} for a download that is still in progress",
        started.elapsed()
    );
}

#[test]
fn lc_asc_is_not_he_aac() {
    let asc = lc_stereo_asc(44_100);
    assert_eq!(aac_audio_object_type(&asc), Some(2));
    assert!(!is_he_aac_config(&asc));
}

#[test]
fn long_lc_config_does_not_imply_sbr() {
    // Extra configuration bytes alone do not establish an SBR profile.
    let mut extra = lc_stereo_asc(22_050).to_vec();
    extra.extend_from_slice(&[0u8; 23]);
    assert!(!is_he_aac_config(&extra));
}

#[test]
fn sbr_and_ps_aot_are_he_aac() {
    assert_eq!(aac_audio_object_type(&[0x2B, 0x10]), Some(5));
    assert!(is_he_aac_config(&[0x2B, 0x10]));
    assert_eq!(aac_audio_object_type(&[0xE8, 0x10]), Some(29));
    assert!(is_he_aac_config(&[0xE8, 0x10]));
}

#[test]
fn stream_buffer_loaded_is_zero_when_cursor_is_behind_window() {
    let buf = StreamBuffer {
        data: vec![],
        start: 7_000_000,
        read_pos: 0,
        total_len: Some(7_737_024),
        fetch_dead: false,
        fetch_error: None,
    };
    assert_eq!(buf.loaded(), 0);
    assert_eq!(buf.cursor(), None);
}

#[test]
fn stream_buffer_cursor_stays_in_window() {
    let buf = StreamBuffer {
        data: vec![0; 1024],
        start: 1000,
        read_pos: 1500,
        total_len: None,
        fetch_dead: false,
        fetch_error: None,
    };
    assert_eq!(buf.cursor(), Some(500));
    assert_eq!(buf.loaded(), 524);
}
#[test]
fn pcm_layout_and_pull_size_are_independent() {
    use std::io::Write;
    for channels in [1u16, 2, 6] {
        let path = std::env::temp_dir().join(format!(
            "bitchord-layout-{}-{channels}.wav",
            std::process::id()
        ));
        let frames = 4097usize;
        let bytes = (frames * channels as usize * 2) as u32;
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend((60 + bytes).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(40u32.to_le_bytes());
        wav.extend(0xfffeu16.to_le_bytes());
        wav.extend(channels.to_le_bytes());
        wav.extend(48000u32.to_le_bytes());
        wav.extend((48000 * channels as u32 * 2).to_le_bytes());
        wav.extend((channels * 2).to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(22u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        let mask = match channels {
            1 => 4u32,
            2 => 3,
            _ => 0x3f,
        };
        wav.extend(mask.to_le_bytes());
        wav.extend([1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]);
        wav.extend(b"data");
        wav.extend(bytes.to_le_bytes());
        for i in 0..frames {
            for ch in 0..channels {
                wav.extend((((i * 17 + ch as usize * 301) % 24000) as i16 - 12000).to_le_bytes());
            }
        }
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&wav)
            .unwrap();
        let render = |chunk| {
            let mut d = SymphoniaDecoder::open(
                &SourceKind::Path(path.to_string_lossy().into()),
                &Default::default(),
            )
            .unwrap();
            let mut x = Vec::new();
            loop {
                let b = d.read_stereo(chunk).unwrap();
                if b.is_empty() {
                    break;
                }
                x.extend(b);
            }
            x
        };
        let large = render(8192);
        assert_eq!(large.len(), frames * 2, "{channels} channels");
        for chunk in [1, 7, 512, 1023, 4096] {
            assert_eq!(render(chunk), large, "{channels} channels, chunk {chunk}");
        }
        if channels == 1 {
            assert!(large.chunks_exact(2).all(|p| p[0] == p[1]));
        }
        let mut seeked = SymphoniaDecoder::open(
            &SourceKind::Path(path.to_string_lossy().into()),
            &Default::default(),
        )
        .unwrap();
        seeked.seek_seconds(1237.0 / 48000.0).unwrap();
        assert_eq!(seeked.read_stereo(100).unwrap(), large[1237 * 2..1337 * 2]);

        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn cancelling_a_stalled_progressive_reader_releases_the_mixer() {
    use std::io::Read;
    let path = std::env::temp_dir().join(format!("bitchord-cancelled-growing-{}", std::process::id()));
    std::fs::write(&path, []).unwrap();
    let marker = format!("{}.playback-cancelled", path.display());
    let _ = std::fs::remove_file(&marker);
    let mut reader = GrowingFile::open(path.to_str().unwrap()).unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    let task = std::thread::spawn(move || { let result = reader.read(&mut [0u8; 1]); sent.send(result).unwrap(); });
    std::thread::sleep(std::time::Duration::from_millis(30));
    std::fs::write(&marker, []).unwrap();
    let result = received.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::ConnectionAborted);
    task.join().unwrap();
    std::fs::remove_file(marker).unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn lossy_codec_labels_use_decoder_identity_without_filename_fallbacks() {
    use symphonia::core::codecs::audio::well_known::*;
    let mut audio = symphonia::core::codecs::audio::AudioCodecParameters::new();
    for (codec, name) in [(CODEC_ID_MP3, "MP3"), (CODEC_ID_AAC, "AAC"), (CODEC_ID_VORBIS, "Vorbis"), (CODEC_ID_FLAC, "FLAC"), (CODEC_ID_ALAC, "ALAC")] {
        audio.codec = codec;
        assert_eq!(super::codec::codec_label(&audio), name);
    }
}
