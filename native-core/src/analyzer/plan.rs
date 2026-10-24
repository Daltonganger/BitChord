//! Compact port of upstream `TransitionPolicy` + `planTransition` for a pair of files.

use super::audio_analysis;
use super::beat::{self, Grid, WINDOW_SECONDS};
use super::resample;
use super::vocal;
use crate::mixer::{TransitionPlan, TransitionStyle, MAX_DUCK_DEPTH};
use std::sync::Arc;

const MIN_BEATMATCH_CONFIDENCE: f64 = 0.55;
const MIN_DJ_CONFIDENCE: f64 = 0.2;
const MIN_BPM: f64 = 40.0;
const MAX_BPM: f64 = 220.0;
/// How far a Beatmatched pair may drift from unity and still stretch.
///
/// Apple AutoMix's documented failure mode is comic warps (reviewers report
/// ~0.75×–1.5×). Beatmatched stretch stays inside this band; DjAssisted never
/// stretches at all — it rides filters instead.
const MAX_STRETCH_DEVIATION: f64 = 0.04;
/// Alias used at call sites that stretch an incoming track onto the outgoing.
const MAX_BEATMATCH_STRETCH: f64 = MAX_STRETCH_DEVIATION;
const VOCAL_ACTIVE_THRESHOLD: f64 = 0.6;
const FILTER_SWEEP: f64 = 1.0;
const AUTO_TRANSITION_MAX_BEATS: f64 = 16.0;
const AUTO_TRANSITION_MAX_SECONDS: f64 = 12.0;
/// F's bridged transition may run longer than an ordinary one: the owner wants
/// long, natural overlaps, and the 12 s ceiling was upstream's choice rather
/// than a listening result. The base fade still respects 12 s; only the bridge
/// may take the total to this.
const MAX_BRIDGED_SECONDS: f64 = 16.0;
const AUTO_MIN_SECONDS: f64 = 4.0;
const AUTO_FAST_TRACK_MIN_SECONDS: f64 = 6.0;
/// Upstream `AUTO_FALLBACK_SECONDS` — the fade used when no analysis landed.
const AUTO_FALLBACK_SECONDS: f64 = 8.0;
/// Upstream `MIN_SMART_DURATION_SECONDS` — a track shorter than this gets a
/// plain crossfade rather than a smart transition.
const MIN_SMART_DURATION_SECONDS: f64 = 45.0;
/// Upstream `TrackAnalyzer.MIN_DECODED_FRACTION` — how much of the container's
/// stated duration must actually decode before the whole-track pass is trusted.
///
/// Not 1.0: a decoder legitimately comes up a frame or two short of the
/// container's rounding, and refusing over that would refuse everything. Not
/// lower either: the failure this catches is a still-downloading file, and the
/// cost of trusting one is a mix-out anchor placed where the bytes ran out.
const MIN_DECODED_FRACTION: f64 = 0.95;
/// Pickup inside this margin of the end is the outro, not a mix-in
/// (upstream `incomingCuePoint`: `pickup < duration - 10`).
const MIX_IN_END_MARGIN_SECONDS: f64 = 10.0;
/// How deep a mix-in may reach, in seconds, in beats, and as a fraction of the
/// incoming track.
///
/// A DJ enters the next record at the top of its own intro. Each bound exists
/// for a different track:
///
/// * `MAX_CUE_SECONDS` is the one that normally bites — four bars is about 7 s
///   at 140 BPM, so anything past that is a skip, not a mix-in.
/// * `MAX_CUE_BEATS` stops a slow track turning 16 beats into half a minute,
///   which is what the seconds bound would otherwise allow at 40 BPM.
/// * `MAX_CUE_FRACTION` is a floor under both, for short material: 6 % of a
///   one-minute clip is 3.6 s, and an 8 s skip there is 13 % of the record. It
///   never binds above about 133 s, where the seconds bound takes over.
const MAX_CUE_SECONDS: f64 = 8.0;
const MAX_CUE_BEATS: f64 = 16.0;
const MAX_CUE_FRACTION: f64 = 0.06;
/// Upstream bass-swap tuning constants (TransitionPlanner.kt:351-364).
const HANDOFF_FRACTION: f64 = 0.5;
const DEFAULT_BASS_SWAP_FRACTION: f64 = 0.7;
const MAX_BASS_SWAP_FRACTION: f64 = 0.85;
const MIN_BASS_STRUCTURE_SCORE: f64 = 0.25;
const BASS_SWAP_MAX_SECONDS: f64 = 6.0;
/// Album tracks played in order still join, they just do not get a DJ blend.
///
/// Upstream uses 0.12 s here (TransitionPlanner.kt:901), which is a splice.
/// A few seconds of equal-power is the shortest ramp that still reads as one
/// record giving way to the next.
const GAPLESS_FADE_SECONDS: f64 = AUTO_MIN_SECONDS;
/// Upstream `MAX_DISCARDED_MUSIC_SECONDS` — a mix-out anchor may not skip more
/// than this much audible music.
const MAX_DISCARDED_MUSIC_SECONDS: f64 = 12.0;
/// Upstream `AUDIBLE_ENERGY_FRACTION` — the energy threshold that counts a
/// point as audible when measuring skipped music.
const AUDIBLE_ENERGY_FRACTION: f64 = 0.1;
/// Upstream WSOLA phrase-switch constants (TransitionPlanner.kt:329-374),
/// plus the long blend a confident, unclashing pair is allowed.
const MIN_FADE_BEATS: usize = 4;
/// Upstream ceiling. A confident beat match may go further; see
/// [`LONG_BLEND_BEATS`].
const UPSTREAM_MAX_FADE_BEATS: usize = 16;
const UPSTREAM_MAX_OVERLAP_SECONDS: f64 = 16.0;
/// Apple-style long blend: up to 64 beats / 32 s when both grids are trusted
/// and the vocal-clash loop does not shrink it. 64 beats at 120 BPM is 32 s.
const LONG_BLEND_BEATS: usize = 64;
const LONG_BLEND_SECONDS: f64 = 32.0;
/// After the blend, walk the matched tempo back to the record's own over
/// this many beats of the tempo it was matched to.
const POST_BLEND_GLIDE_BEATS: f64 = 32.0;
/// Incoming level while it plays underneath, before the swap.
const BED_GAIN_DB: f64 = -14.0;
const ARRANGEMENT_OVERLAP_BEATS: f64 = 8.0;
const MIN_CLEARANCE_SECONDS: f64 = 5.0;
const VOCAL_CLASH_TOLERANCE: f64 = 0.05;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct Analysis {
    bpm: f64,
    beat_interval: f64,
    beat_confidence: f64,
    downbeats: Vec<f64>,
    first_beat: f64,
    duration: f64,
    /// Detected key (e.g. "C major") and its confidence — phase-1
    /// `AnalyzeKeyAndTimbre`.
    key: String,
    key_confidence: f64,
    /// Where the audible content ends (trailing silence trimmed) — phase-1
    /// `AnalyzeEnvelope`. 0 means unknown.
    content_end: f64,
    outro_start: f64,
    mix_out_time: f64,
    mix_in_time: f64,
    audible_start_time: f64,
    mix_in_candidates: Vec<audio_analysis::MixCuePoint>,
    mix_out_candidates: Vec<audio_analysis::MixCuePoint>,
    /// Phrase starts, for snapping a transition onto a boundary.
    phrase_boundaries: Vec<f64>,
    section_boundaries: Vec<f64>,
    segment_boundaries: Vec<f64>,
    energy_curve: Vec<audio_analysis::EnergyPoint>,
    low_energy_curve: Vec<audio_analysis::EnergyPoint>,
    pace_curve: Vec<audio_analysis::EnergyPoint>,
    /// DSP per-frame vocal mask (parallel to `energy_curve`), from
    /// `AnalyzeKeyAndTimbre`. Drives `vocalActivityBetween` /
    /// `simultaneousVocalFraction`.
    vocal_activity_mask: Vec<f64>,
    drum_activity_mask: Vec<f64>,
    bass_activity_mask: Vec<f64>,
    /// The full beat grid, for downbeat snapping.
    beats: Vec<f64>,
    /// Whole-track DSP vocal heuristic (`AnalyzeKeyAndTimbre`), not the model
    /// mask mean.
    vocal_probability: f64,
    /// F-3: the beat length measured from the chosen **bar** grid (median bar
    /// / 4), when the bars come from the PLL because the model's were
    /// incomplete or an octave out. 0 means "use `beat_interval`". The tempo
    /// and tier still use the model's bpm; only the phrase length follows this.
    bar_beat_seconds: f64,
    /// Where the strongest analysis for this track came from.
    /// `music_understanding` | `beat_this` | `dsp` | `disk_cache`.
    analysis_source: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    Beatmatched,
    DjAssisted,
    Plain,
}

/// Analysis cache — the difference between planning as a decision and planning
/// as a workload.
///
/// One [`analyze`] is three decodes of the same file (whole track, head window,
/// vocal window), a resample, and two ONNX passes, which for a four-minute
/// track is seconds of CPU. The app asks for the same pair over and over:
/// `syncEngineQueueNext` alone is reached from nine call sites, and each one
/// re-plans the track that follows the current one. Uncached, that is the whole
/// analysis re-run for every transport command — and it runs at the same
/// priority band as the audio decoder, which is why opening a page could be
/// heard as a stutter.
///
/// The plan itself is *not* cached: it also depends on the crossfade setting,
/// the caller's text and the album flag, all of which are cheap to re-apply.
/// Only the analysis — the expensive, file-derived half plus its duration
/// hint — is kept.
///
/// The compute runs under the lock on purpose. Two callers asking for the same
/// track is the common case, and serialising means the second one waits and
/// then hits, instead of both decoding the same file at once.
mod analysis_cache {
    use super::Analysis;
    use std::collections::{HashMap, VecDeque};
    use std::io::Write;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::UNIX_EPOCH;

    /// Enough for the head and tail of a queue; beyond that, re-decoding costs
    /// less than holding it. Each entry is tens to hundreds of kilobytes.
    const CAPACITY: usize = 6;
    const DISK_LIMIT_BYTES: u64 = 50 * 1024 * 1024;
    const SCHEMA_VERSION: u32 = 1;

    pub(super) type Key = (String, u64, u64, u64, String, bool);

    #[derive(Default)]
    struct Cache {
        entries: HashMap<Key, Arc<Analysis>>,
        /// Insertion order, for eviction. A key never appears twice: it is only
        /// pushed on a miss, and a hit returns before that.
        order: VecDeque<Key>,
    }

    fn cache() -> &'static Mutex<Cache> {
        static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(Cache::default()))
    }

    pub(super) fn clear() {
        let mut guard = cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.entries.clear();
        guard.order.clear();
    }

    /// File length is part of the key because a streamed download is appended
    /// to under a stable path. Duration is part too: URL sources can start with
    /// no usable duration and acquire one from the playback/catalogue metadata
    /// later, which changes whether whole-track analysis can run.
    pub(super) fn file_identity(path: &str) -> (u64, u64) {
        std::fs::metadata(path)
            .map(|meta| {
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos() as u64)
                    .unwrap_or(0);
                (meta.len(), modified)
            })
            .unwrap_or((0, 0))
    }

    fn disk_dir() -> Option<std::path::PathBuf> {
        #[cfg(test)]
        {
            // Unit tests must opt into an isolated directory; never fall back
            // to the developer's real application cache.
            return TEST_DISK_DIR.with(|value| value.borrow().clone());
        }
        #[cfg(not(test))]
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .map(|home| home.join("Library/Caches/BitChord/Analysis"))
    }

    #[cfg(test)]
    thread_local! {
        #[cfg(test)]
        static TEST_DISK_DIR: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) };
    }

    #[cfg(test)]
    pub(super) fn set_test_disk_dir(dir: std::path::PathBuf) {
        TEST_DISK_DIR.with(|value| *value.borrow_mut() = Some(dir));
    }

    pub(super) fn disk_key(key: &Key) -> String {
        let mut hasher = sha2::Sha256::new();
        use sha2::Digest;
        hasher.update(SCHEMA_VERSION.to_le_bytes());
        hasher.update(serde_json::to_vec(key).unwrap_or_default());
        format!("{:x}", hasher.finalize())
    }

    fn read_disk(key: &Key) -> Option<Analysis> {
        let path = disk_dir()?.join(format!("{}.json", disk_key(key)));
        let bytes = std::fs::read(path).ok()?;
        let value = serde_json::from_slice(&bytes).ok()?;
        // Updating the access time makes filesystem modification time the LRU
        // signal used by the size cap.
        if let Ok(file) = std::fs::File::options().append(true).open(disk_dir()?.join(format!("{}.json", disk_key(key)))) {
            let _ = file.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now()));
        }
        Some(value)
    }

    fn write_disk(key: &Key, analysis: &Analysis) {
        let Some(dir) = disk_dir() else { return };
        if std::fs::create_dir_all(&dir).is_err() { return; }
        let path = dir.join(format!("{}.json", disk_key(key)));
        let temp = dir.join(format!("{}.tmp", disk_key(key)));
        let Ok(bytes) = serde_json::to_vec(analysis) else { return };
        if let Ok(mut file) = std::fs::File::create(&temp) {
            if file.write_all(&bytes).is_ok() && std::fs::rename(&temp, &path).is_ok() {
                trim_disk(&dir);
                return;
            }
        }
        let _ = std::fs::remove_file(temp);
    }

    fn trim_disk(dir: &std::path::Path) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut files: Vec<_> = entries.flatten().filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            meta.is_file().then(|| (entry.path(), meta.len(), meta.modified().ok()))
        }).collect();
        let mut total: u64 = files.iter().map(|(_, size, _)| *size).sum();
        files.sort_by_key(|(_, _, modified)| *modified);
        for (path, size, _) in files {
            if total <= DISK_LIMIT_BYTES { break; }
            if std::fs::remove_file(path).is_ok() { total = total.saturating_sub(size); }
        }
    }

    pub(super) fn get_or_compute(
        path: &str,
        skip_vocals: bool,
        duration: f64,
        compute: impl FnOnce() -> Analysis,
    ) -> Arc<Analysis> {
        get_or_compute_for_identity(path, skip_vocals, duration, super::super::models::analysis_identity(), compute)
    }

    pub(super) fn get_or_compute_for_identity(
        path: &str,
        skip_vocals: bool,
        duration: f64,
        model_identity: String,
        compute: impl FnOnce() -> Analysis,
    ) -> Arc<Analysis> {
        let (size, modified) = file_identity(path);
        let key: Key = (
            path.to_string(),
            size,
            modified,
            duration.to_bits(),
            model_identity,
            skip_vocals,
        );
        // A panic while analysing must not poison the cache for the session.
        let mut guard = cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(hit) = guard.entries.get(&key) {
            return hit.clone();
        }
        if let Some(hit) = read_disk(&key) {
            let hit = Arc::new(hit);
            guard.entries.insert(key.clone(), hit.clone());
            guard.order.push_back(key);
            return hit;
        }
        let analysis = Arc::new(compute());
        // A transient decode failure or a short/incomplete file can produce a
        // default analysis. Keep it only in memory so a later call can retry.
        if duration > 0.0 && (analysis.bpm > 0.0 || !analysis.beats.is_empty()) {
            write_disk(&key, &analysis);
        }
        guard.entries.insert(key.clone(), analysis.clone());
        guard.order.push_back(key);
        while guard.order.len() > CAPACITY {
            if let Some(evicted) = guard.order.pop_front() {
                guard.entries.remove(&evicted);
            }
        }
        analysis
    }

    pub(super) fn invalidate(path: &str) {
        let norm = path.strip_prefix("file://").unwrap_or(path);
        let mut guard = cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let doomed: Vec<Key> = guard
            .entries
            .keys()
            .filter(|(p, _, _, _, _, _)| {
                let p_norm = p.strip_prefix("file://").unwrap_or(p);
                p_norm == norm
            })
            .cloned()
            .collect();
        for key in doomed {
            guard.entries.remove(&key);
            guard.order.retain(|k| k != &key);
        }
    }

    pub(super) fn get_cached_energy_curve(
        path: &str,
    ) -> Option<Vec<super::audio_analysis::EnergyPoint>> {
        let norm_path = path.strip_prefix("file://").unwrap_or(path);
        let guard = cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.order.iter().rev().find_map(|key| {
            let key_path = key.0.strip_prefix("file://").unwrap_or(&key.0);
            if key_path == norm_path {
                guard
                    .entries
                    .get(key)
                    .map(|analysis| analysis.energy_curve.clone())
            } else {
                None
            }
        })
    }
}

pub(super) fn clear_analysis_cache() {
    analysis_cache::clear();
    overlays::clear();
}

/// External analysis overlay (Music Understanding, disk cache, …) merged onto
/// the DSP/ONNX result after `analyze` finishes. Rhythm/key/structure from a
/// stronger source win; envelope/energy from DSP stay unless the overlay
/// supplies replacements.
#[derive(Clone, Debug, Default)]
pub struct AnalysisOverlay {
    pub bpm: Option<f64>,
    pub beat_confidence: Option<f64>,
    pub beat_interval: Option<f64>,
    pub beats: Option<Vec<f64>>,
    pub downbeats: Option<Vec<f64>>,
    pub key: Option<String>,
    pub key_confidence: Option<f64>,
    pub phrase_boundaries: Option<Vec<f64>>,
    pub vocal_probability: Option<f64>,
    pub content_end: Option<f64>,
    pub audible_start: Option<f64>,
    pub outro_start: Option<f64>,
    pub mix_in_time: Option<f64>,
    /// `music_understanding` | `disk_cache` | …
    pub source: String,
}

mod overlays {
    use super::AnalysisOverlay;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    fn map() -> &'static Mutex<HashMap<String, AnalysisOverlay>> {
        static MAP: OnceLock<Mutex<HashMap<String, AnalysisOverlay>>> = OnceLock::new();
        MAP.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub(super) fn clear() {
        map().lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    pub(super) fn put(path: &str, overlay: AnalysisOverlay) {
        let key = path.strip_prefix("file://").unwrap_or(path).to_string();
        map()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, overlay);
    }

    pub(super) fn get(path: &str) -> Option<AnalysisOverlay> {
        let key = path.strip_prefix("file://").unwrap_or(path);
        map()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .cloned()
    }
}

/// Register an external analysis overlay for `path`. The next `plan_pair`
/// analysis of that file merges it on top of DSP/ONNX results. Returns true
/// when stored.
pub fn seed_analysis_overlay(path: &str, overlay: AnalysisOverlay) -> bool {
    if path.is_empty() {
        return false;
    }
    // Drop any cached analysis so the next plan re-runs with the overlay.
    analysis_cache::invalidate(path);
    overlays::put(path, overlay);
    true
}

/// Sources used by the most recent `plan_pair` (outgoing, incoming).
pub fn last_analysis_sources() -> (String, String) {
    last_sources::get()
}

mod last_sources {
    use std::sync::{Mutex, OnceLock};

    fn pair() -> &'static Mutex<(String, String)> {
        static PAIR: OnceLock<Mutex<(String, String)>> = OnceLock::new();
        PAIR.get_or_init(|| Mutex::new((String::new(), String::new())))
    }

    pub(super) fn set(outgoing: &str, incoming: &str) {
        let out = if outgoing.is_empty() { "dsp" } else { outgoing };
        let inc = if incoming.is_empty() { "dsp" } else { incoming };
        *pair().lock().unwrap_or_else(|p| p.into_inner()) = (out.into(), inc.into());
    }

    pub(super) fn get() -> (String, String) {
        pair().lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

fn apply_overlay(analysis: &mut Analysis, overlay: &AnalysisOverlay) {
    // Prefer Beat This / DSP tempo when MU reports a clear half/double of an
    // already-trusted grid (common with electronic tracks). Key/structure from
    // MU still apply.
    let trust_existing_tempo =
        analysis.beat_confidence >= 0.55 && !analysis.downbeats.is_empty() && analysis.bpm > 0.0;
    let overlay_bpm = overlay.bpm.filter(|b| *b > 0.0);
    let tempo_octave = overlay_bpm.is_some_and(|bpm| {
        let ratio = bpm / analysis.bpm;
        (ratio - 2.0).abs() < 0.12
            || (ratio - 0.5).abs() < 0.06
            || (ratio - 3.0).abs() < 0.15
            || (ratio - 1.0 / 3.0).abs() < 0.05
    });
    let accept_overlay_tempo = !(trust_existing_tempo && tempo_octave);

    if accept_overlay_tempo {
        if let Some(bpm) = overlay_bpm {
            analysis.bpm = bpm;
        }
        if let Some(c) = overlay.beat_confidence {
            analysis.beat_confidence = c.clamp(0.0, 1.0);
        }
        if let Some(interval) = overlay.beat_interval.filter(|i| *i > 0.0) {
            analysis.beat_interval = interval;
        } else if analysis.bpm > 0.0 {
            analysis.beat_interval = 60.0 / analysis.bpm;
        }
        if let Some(beats) = overlay.beats.as_ref().filter(|b| b.len() >= 2) {
            analysis.beats = beats.clone();
            analysis.first_beat = beats[0];
        }
        if let Some(downbeats) = overlay.downbeats.as_ref().filter(|d| !d.is_empty()) {
            analysis.downbeats = downbeats.clone();
        }
    } else if let Some(bpm) = overlay_bpm {
        log::info!(
            "automix overlay: keeping {:.1} bpm grid; ignoring MU tempo {:.1} (octave)",
            analysis.bpm,
            bpm
        );
    }
    if let Some(key) = overlay.key.as_ref().filter(|k| !k.is_empty()) {
        analysis.key = key.clone();
    }
    if let Some(c) = overlay.key_confidence {
        analysis.key_confidence = c.clamp(0.0, 1.0);
    }
    if let Some(phrases) = overlay.phrase_boundaries.as_ref().filter(|p| !p.is_empty()) {
        analysis.phrase_boundaries = phrases.clone();
        // Treat phrase overlays as section hints when MU provides hierarchy as phrases.
        if analysis.section_boundaries.is_empty() {
            analysis.section_boundaries = phrases.clone();
        }
    }
    if let Some(v) = overlay.vocal_probability {
        analysis.vocal_probability = v.clamp(0.0, 1.0);
    }
    if let Some(end) = overlay.content_end.filter(|e| *e > 0.0) {
        analysis.content_end = end;
    }
    if let Some(start) = overlay.audible_start.filter(|s| s.is_finite() && *s >= 0.0) {
        analysis.audible_start_time = start;
    }
    if let Some(outro) = overlay.outro_start.filter(|s| *s > 0.0) {
        analysis.outro_start = outro;
    }
    if let Some(mix_in) = overlay.mix_in_time.filter(|s| *s > 0.0) {
        analysis.mix_in_time = mix_in;
    }
    if !overlay.source.is_empty() {
        // Keep a blended label when we refused the MU tempo but took structure.
        if !accept_overlay_tempo && analysis.analysis_source == "beat_this" {
            analysis.analysis_source = "beat_this+music_understanding".into();
        } else {
            analysis.analysis_source = overlay.source.clone();
        }
        log::info!(
            "automix overlay [{}]: bpm={:.1} conf={:.2} key={} phrases={}",
            analysis.analysis_source,
            analysis.bpm,
            analysis.beat_confidence,
            analysis.key,
            analysis.phrase_boundaries.len()
        );
    }
}

/// Locates the next local energy dip in the audio curve for the given source
/// within [position_seconds + 0.1, position_seconds + 2.0].
/// Used to defer source swap execution so crossfades happen in a valley.
pub fn next_energy_dip(source: &str, position_seconds: f64) -> Option<f64> {
    let curve = analysis_cache::get_cached_energy_curve(source)?;
    find_next_energy_dip(&curve, position_seconds)
}

pub(crate) fn find_next_energy_dip(
    curve: &[audio_analysis::EnergyPoint],
    position_seconds: f64,
) -> Option<f64> {
    if curve.is_empty() {
        return None;
    }
    let window_start = position_seconds + 0.1;
    let window_end = position_seconds + 2.0;

    let current_energy = curve
        .iter()
        .min_by(|a, b| {
            (a.time - position_seconds)
                .abs()
                .total_cmp(&(b.time - position_seconds).abs())
        })
        .map(|p| p.energy)
        .unwrap_or(1.0);

    let candidates: Vec<(usize, &audio_analysis::EnergyPoint)> = curve
        .iter()
        .enumerate()
        .filter(|(_, p)| p.time >= window_start && p.time <= window_end)
        .collect();

    if candidates.is_empty() {
        return None;
    }

    let mut local_minima = Vec::new();
    for (idx, p) in &candidates {
        let left_energy = if *idx > 0 {
            curve[*idx - 1].energy
        } else {
            p.energy
        };
        let right_energy = if *idx + 1 < curve.len() {
            curve[*idx + 1].energy
        } else {
            p.energy
        };
        if p.energy <= left_energy && p.energy <= right_energy {
            local_minima.push(*p);
        }
    }

    if let Some(best) = local_minima
        .iter()
        .filter(|p| p.energy < current_energy * 0.95 || p.energy < 0.7)
        .min_by(|a, b| a.energy.total_cmp(&b.energy))
    {
        return Some(best.time);
    }

    if let Some((_, min_cand)) = candidates
        .iter()
        .min_by(|(_, a), (_, b)| a.energy.total_cmp(&b.energy))
    {
        if min_cand.energy < current_energy * 0.8 && min_cand.energy < 0.6 {
            return Some(min_cand.time);
        }
    }

    None
}

/// [`analyze`], memoised on the file's identity and the vocal-model switch.
fn analyze_cached(
    path: &str,
    skip_vocals: bool,
    decode: &impl Fn(&str, f64, f64, bool) -> Option<(Vec<f32>, u32, f64)>,
    duration: f64,
) -> Arc<Analysis> {
    if duration <= 0.0 {
        // A caller that handed us something that is not a readable audio file —
        // a `yt:<videoId>` identifier rather than a path, most likely — lands
        // here, and the degradation is otherwise silent: `analyze` skips its
        // whole-track branch because the window is `duration` seconds long, so
        // `bpm` stays 0 and `assess` can only return `Tier::Plain`. Every
        // transition becomes a plain crossfade and nothing says why. The other
        // side of the pair still analyses normally, which is what made this so
        // hard to see: the plan's cue looked right.
        log::warn!(
            "automix: no duration for {path} — whole-track analysis and tempo verdict \
             skipped, so this pair can only plan a plain crossfade (not a readable audio file?)"
        );
    }
    analysis_cache::get_or_compute(path, skip_vocals, duration, || {
        analyze(path, skip_vocals, decode, duration)
    })
}

pub fn plan_pair(
    outgoing_path: &str,
    incoming_path: &str,
    outgoing_text: &str,
    incoming_text: &str,
    album_sequential: bool,
    crossfade_seconds: f64,
    outgoing_duration_hint: f64,
    incoming_duration_hint: f64,
    skip_vocals: bool,
    tier_label: &str,
    allow_effects: bool,
    decode: impl Fn(&str, f64, f64, bool) -> Option<(Vec<f32>, u32, f64)>,
    duration_of: impl Fn(&str) -> f64,
) -> TransitionPlan {
    let fade = if crossfade_seconds > 0.0 {
        crossfade_seconds.clamp(1.0, 12.0)
    } else {
        AUTO_FALLBACK_SECONDS
    };
    // Upstream `BLOCKED_TEXT`: spoken or already-performed material is never
    // smart-mixed — fall back to a plain equal-power crossfade.
    //
    // Even this one carries the outgoing track's length, because a plain
    // crossfade is still *scheduled* against the end of the outgoing track and
    // the mixer has to know where that is.
    let hinted_duration = |hint: f64, path: &str| {
        if hint.is_finite() && hint > 0.0 {
            hint
        } else {
            duration_of(path)
        }
    };
    let outgoing_duration = hinted_duration(outgoing_duration_hint, outgoing_path);
    let out = analyze_cached(outgoing_path, skip_vocals, &decode, outgoing_duration);
    // Spoken or already-performed material is never smart-mixed. The fallback
    // is still anchored on the outgoing content end, so it does not wait out
    // the trailing silence and then cut.
    if blocked_text(outgoing_text) || blocked_text(incoming_text) {
        return plain_fallback(fade, &out);
    }

    // Upstream gapless path (TransitionPlanner.kt:893-905): an album played
    // through in order skips the DJ blend, unless the outgoing track has an
    // interior energy cliff worth mixing out of. The join itself is still a
    // ramp — upstream's 0.12 s handoff is a cut.
    if album_sequential {
        let (anchor, _) = resolve_mix_out_anchor(&out);
        let end = if out.content_end > 0.0 {
            out.content_end
        } else {
            out.duration
        };
        let has_interior_mix_out = anchor > 0.0 && anchor < end - 1.0;
        if !has_interior_mix_out {
            return TransitionPlan {
                style: TransitionStyle::Gapless,
                bass_swap: false,
                bass_swap_fraction: DEFAULT_BASS_SWAP_FRACTION,
                filter_sweep: 0.0,
                vocal_overlap: 0.0,
                fade_seconds: GAPLESS_FADE_SECONDS,
                outgoing_hold_seconds: 0.0,
                bridge_start_seconds: 0.0,
                bridge_seconds: 0.0,
                bridge_repeats: 0,
                effect_seconds: 0.0,
                effect_start_seconds: 0.0,
                transition_end_seconds: 0.0,
                cue_seconds: 0.0,
                playback_rate: 1.0,
                bed_fraction: 0.0,
                bed_gain_db: 0.0,
                dip_depth: 0.0,
                dip_width: 0.0,
                post_glide_seconds: 0.0,
                outgoing_duration_seconds: outgoing_duration,
                beat_seconds: 0.0,
                reason: String::new(),
            };
        }
    }

    let incoming_duration = hinted_duration(incoming_duration_hint, incoming_path);
    let inc = analyze_cached(incoming_path, skip_vocals, &decode, incoming_duration);

    // The most ambitious move: a beat-matched, harmonically-compatible pair gets
    // a WSOLA phrase-switch before the adaptive overlap (upstream `phraseSwitch`).
    let mut plan = phrase_switch(&out, &inc)
        .unwrap_or_else(|| plan_from(&out, &inc, fade, allow_effects));
    respect_music_edges(&out, &inc, &mut plan);
    apply_bridge(&out, &mut plan);
    record_transition_edges(&out, &inc, &plan);
    log_plan(&out, &inc, &plan, tier_label);
    plan
}

/// One line per planned transition, with the evidence that produced it.
///
/// The bug this exists to make diagnosable was invisible from the outside: a
/// track that lost forty-five seconds to a mix-in sounded exactly like a track
/// that had always been cued that way, and the mixer's `voice opened: …` line
/// reports the symptom (a large `start`) without the cause. These are the
/// inputs to [`incoming_cue`] and to [`assess`], so a wrong cue can be traced to
/// the read that produced it.
fn log_plan(outgoing: &Analysis, incoming: &Analysis, plan: &TransitionPlan, tier: &str) {
    let grid = |a: &Analysis| {
        format!(
            "bpm={:.1} conf={:.2} beat={:.3} downbeats={} audible={:.1} content_end={:.1} dur={:.1} vocal_p={:.2} \
             key={}@{:.2}",
            a.bpm,
            a.beat_confidence,
            a.beat_interval,
            a.downbeats.len(),
            a.audible_start_time,
            a.content_end,
            a.duration,
            a.vocal_probability,
            if a.key.is_empty() { "?" } else { a.key.as_str() },
            a.key_confidence,
        )
    };
    // What the plan could see about the voice, beside the whole-track scalar
    // the effect rule used to read. `vocal_p` is one number for the record; the
    // tail and the head are the two windows an actual handover covers.
    let window_vocal = |a: &Analysis, from: f64, to: f64| {
        vocal_activity_between(a, from, to)
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "-".into())
    };
    let out_beat = if outgoing.beat_interval > 0.0 {
        outgoing.beat_interval
    } else {
        0.5
    };
    let in_beat = if incoming.beat_interval > 0.0 {
        incoming.beat_interval
    } else {
        0.5
    };
    let out_end = if outgoing.content_end > 0.0 {
        outgoing.content_end
    } else {
        outgoing.duration
    };
    let in_start = incoming.audible_start_time.max(0.0);
    log::info!(
        "automix plan [{}] style={:?} cue={:.2}s fade={:.2}s end={:.1}s rate={:.4} \
         bed={:.2}@{:.0}dB dip={:.2} glide={:.1}s vocal_overlap={:.2} bass_swap={}@{:.2}",
        tier,
        plan.style,
        plan.cue_seconds,
        plan.fade_seconds,
        plan.transition_end_seconds,
        plan.playback_rate,
        plan.bed_fraction,
        plan.bed_gain_db,
        plan.dip_depth,
        plan.post_glide_seconds,
        plan.vocal_overlap,
        plan.bass_swap,
        plan.bass_swap_fraction,
    );
    log::info!(
        "automix out [{}]: {} vocal_tail={}",
        if outgoing.analysis_source.is_empty() {
            "dsp"
        } else {
            &outgoing.analysis_source
        },
        grid(outgoing),
        window_vocal(outgoing, out_end - 8.0 * out_beat, out_end)
    );
    log::info!(
        "automix in [{}]: {} vocal_head={}",
        if incoming.analysis_source.is_empty() {
            "dsp"
        } else {
            &incoming.analysis_source
        },
        grid(incoming),
        window_vocal(incoming, in_start, in_start + 8.0 * in_beat)
    );
    last_sources::set(&outgoing.analysis_source, &incoming.analysis_source);
}

/// The plain equal-power fallback shared by the speech guard and the tier.
///
/// Anchored on the outgoing mix-out (or its content end), never on the file's
/// last byte — that is the silence after the song, and fading there is why a
/// transition waited for one record to finish before the next began.
fn plain_fallback(fade: f64, outgoing: &Analysis) -> TransitionPlan {
    let mut end = resolve_mix_out_anchor(outgoing).0;
    if end <= 0.0 {
        end = if outgoing.content_end > 0.0 {
            outgoing.content_end
        } else {
            outgoing.duration
        };
    }
    TransitionPlan {
        style: TransitionStyle::EqualPower,
        bass_swap: false,
        bass_swap_fraction: DEFAULT_BASS_SWAP_FRACTION,
        filter_sweep: 0.0,
        vocal_overlap: 0.0,
        fade_seconds: fade,
        outgoing_hold_seconds: 0.0,
        bridge_start_seconds: 0.0,
        bridge_seconds: 0.0,
        bridge_repeats: 0,
        effect_seconds: 0.0,
        effect_start_seconds: 0.0,
        transition_end_seconds: end,
        cue_seconds: 0.0,
        playback_rate: 1.0,
        bed_fraction: 0.0,
        bed_gain_db: 0.0,
        dip_depth: 0.0,
        dip_width: 0.0,
        post_glide_seconds: 0.0,
        outgoing_duration_seconds: outgoing.duration,
        beat_seconds: 0.0,
        reason: String::new(),
    }
}

/// Upstream `BLOCKED_TEXT` regex — `\b(podcast|episode|audiobook|live|concert|performance)\b`
/// case-insensitive — checked with word boundaries, no regex dependency.
fn blocked_text(text: &str) -> bool {
    const BLOCKED: [&str; 6] = [
        "podcast",
        "episode",
        "audiobook",
        "live",
        "concert",
        "performance",
    ];
    let lower = text.to_lowercase();
    let bytes = lower.as_bytes();
    for word in BLOCKED {
        let mut start = 0;
        while let Some(relative) = lower[start..].find(word) {
            let absolute = start + relative;
            let before_ok = absolute == 0 || !bytes[absolute - 1].is_ascii_alphanumeric();
            let end = absolute + word.len();
            let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return true;
            }
            start = absolute + word.len();
        }
    }
    false
}

fn analyze(
    path: &str,
    skip_vocals: bool,
    decode: &impl Fn(&str, f64, f64, bool) -> Option<(Vec<f32>, u32, f64)>,
    duration: f64,
) -> Analysis {
    let analysis_started = std::time::Instant::now();
    let mut analysis = Analysis {
        duration,
        ..Analysis::default()
    };
    let head_len = if duration > 0.0 {
        WINDOW_SECONDS.min(duration)
    } else {
        WINDOW_SECONDS
    };

    // Whole-track DSP analysis — envelope, tempo (PLL), key/chroma, spectral
    // bands, structure, and the DSP vocal heuristic. This is upstream's
    // `AnalyzeAudio`, the evidence the policy works from when the Beat This!
    // model is absent (which is the shipped state).
    //
    // The buffer is kept so the head window below can read it rather than
    // decoding the same file again: three decodes per track was the port's
    // shape, and the whole-track pass is the expensive one — measured at 2.8 s
    // of a 4.2 s plan for a pair of four-minute tracks, more than twice both
    // ONNX models together.
    let mut whole: Option<(Vec<f32>, u32)> = None;
    if duration > 0.0 {
        if let Some((mono, rate, _)) = decode(path, 0.0, duration, true) {
            let decoded_seconds = if rate > 0 {
                mono.len() as f64 / rate as f64
            } else {
                0.0
            };
            let duration_shortfall = (duration - decoded_seconds).max(0.0);
            let tolerated_shortfall = (duration * 0.005).max(0.3);
            if decoded_seconds < duration * MIN_DECODED_FRACTION
                || duration_shortfall > tolerated_shortfall
            {
                // Upstream refuses a short whole-track decode outright rather
                // than publishing it (`TrackAnalyzer.structure`). An analysis
                // that stops where the bytes ran out is indistinguishable,
                // downstream, from a track that simply goes quiet: `content_end`
                // lands at the end of the *bytes* and the mix-out anchor with
                // it, and the audible symptom is the track faded out minutes
                // early. A missing analysis degrades to a plain crossfade; a
                // confidently wrong one does not degrade at all. This is exactly
                // a still-downloading file.
                log::warn!(
                    "whole-track analysis refused for {path}: decoded \
                     {decoded_seconds:.1}s of a {duration:.1}s container"
                );
            } else {
                let mono_analysis = resample(&mono, rate as f64, audio_analysis::ANALYSIS_RATE);
                let audio = audio_analysis::analyze_audio(
                    &mono_analysis,
                    audio_analysis::ANALYSIS_RATE,
                    duration,
                );
                analysis.bpm = audio.bpm;
                analysis.beat_interval = audio.beat_interval;
                analysis.beat_confidence = audio.beat_confidence;
                analysis.downbeats = audio.downbeats;
                analysis.first_beat = audio.first_beat;
                analysis.key = audio.key;
                analysis.key_confidence = audio.key_confidence;
                analysis.content_end = audio.content_end_time;
                analysis.outro_start = audio.outro_start_time;
                analysis.mix_out_time = audio.mix_out_time;
                analysis.mix_in_time = audio.mix_in_time;
                analysis.audible_start_time = audio.audible_start_time;
                analysis.mix_in_candidates = audio.mix_in_candidates;
                analysis.mix_out_candidates = audio.mix_out_candidates;
                analysis.phrase_boundaries = audio.phrase_boundaries;
                analysis.section_boundaries = audio.section_boundaries;
                analysis.segment_boundaries = audio.segment_boundaries;
                analysis.energy_curve = audio.energy_curve;
                analysis.low_energy_curve = audio.low_energy_curve;
                analysis.pace_curve = audio.pace_curve;
                analysis.vocal_activity_mask = audio.vocal_activity_mask;
                analysis.drum_activity_mask = audio.drum_activity_mask;
                analysis.bass_activity_mask = audio.bass_activity_mask;
                analysis.beats = audio.beats;
                analysis.vocal_probability = audio.vocal_probability;
                analysis.analysis_source = "dsp".into();
                // Only an accepted whole-track decode may feed the later beat
                // model pass. Keeping a refused partial buffer here made the
                // missing bytes look like trailing silence to the planner.
                whole = Some((mono, rate));
            }
        }
    }

    // The Beat This! ONNX grid, when loaded, overrides the DSP tempo with a
    // stronger beat/downbeat grid on the head window — which is the head of the
    // buffer just decoded, so it is a slice rather than a second decode. The
    // fallback exists only for the case where the whole-track decode failed
    // outright.
    //
    // Whether the model actually ran is worth saying out loud. The graphs are a
    // first-run download rather than a build input, so "Automix is on" and "a
    // beat model is installed" are different states, and a plan built on the DSP
    // estimator is a different — and noticeably worse — plan. The symptom is
    // invisible from the outside: transitions still happen, they are just
    // coarser, and `mix_in_candidates` in particular used to be derived here and
    // never revisited, so a model grid could not fix it even once installed.
    // Beat This! over the whole decoded buffer when we have it. The head-only
    // window left mix-out anchors on a DSP grid while intros looked great —
    // Apple AutoMix's quality lives at the *end* of the outgoing track.
    let model_grid = if let Some((mono, rate)) = whole.as_ref() {
        beat::track(mono, *rate as f64, 0.0)
    } else if let Some((mono, rate, offset)) = decode(path, 0.0, head_len, true) {
        beat::track(&mono, rate as f64, offset)
    } else {
        None
    };
    match model_grid {
        Some(grid) => {
            let model_bars = grid.downbeats.len();
            let model_bpm = grid.bpm;
            let (grid, bar_beat, bar_source) = reconcile_grid(&analysis, grid);
            log::info!(
                "automix analysis {path}: bar grid from {} — model {:.1} bpm/{} bars, DSP {:.1} bpm/{} \
                 bars; final {:.1} bpm, {} bars, model confidence {:.2}, beats {} span {:.1}..{:.1} of {:.1}s",
                bar_source,
                model_bpm,
                model_bars,
                analysis.bpm,
                analysis.downbeats.len(),
                grid.bpm,
                grid.downbeats.len(),
                grid.beat_confidence,
                grid.beats.len(),
                grid.beats.first().copied().unwrap_or(0.0),
                grid.beats.last().copied().unwrap_or(0.0),
                duration,
            );
            fill_grid(&mut analysis, grid);
            analysis.bar_beat_seconds = bar_beat;
        }
        None => {
            log::info!(
                "automix analysis {path}: no beat-model grid — DSP tempo only \
                 ({:.1} bpm, confidence {:.2}). Transitions will be coarser; \
                 install the Automix beat model in Settings.",
                analysis.bpm,
                analysis.beat_confidence,
            );
        }
    }

    // EFFICIENT tier: the beat grid (small graph) stays, the open-unmix vocal
    // model (the expensive half) does not run. The per-frame mask it produces
    // drives vocal-clash avoidance; the scalar `vocal_probability` above stays
    // the DSP heuristic, matching upstream's two-signal split.
    if !skip_vocals {
        let vocal_window = vocal::MAX_WINDOW_SECONDS;
        // Upstream `mergeMasks`: the open-unmix head mask is higher quality than
        // the DSP heuristic, so where it exists it overrides the DSP mask on the
        // energy-curve grid.
        ingest_vocals(&mut analysis, path, 0.0, head_len.min(vocal_window), decode);
        // The tail is the window a handover actually covers, and the DSP
        // heuristic cannot tell a sung outro from an instrumental one there.
        // Run the model over the last window too, ending at the content end
        // rather than the file end (which is silence). The middle keeps the DSP
        // heuristic; the two model windows are merged independently.
        let content_end = if analysis.content_end > 0.0 {
            analysis.content_end
        } else {
            duration
        };
        if content_end > head_len.min(vocal_window) + 2.0 {
            let tail_start = (content_end - vocal_window).max(0.0);
            let tail = (content_end - tail_start).min(vocal_window);
            if tail > 2.0 {
                ingest_vocals(&mut analysis, path, tail_start, tail, decode);
            }
        }
    }
    if let Some(overlay) = overlays::get(path) {
        apply_overlay(&mut analysis, &overlay);
    }
    // Counted in one octave before anything plans from it: a detector that
    // reads 44 or 196 for an 88 BPM record is not wrong so much as
    // uncountable, and every rule downstream (the stretch, the overlap in
    // beats, "is this a slow record", the printed gap) has to agree on which
    // octave that is.
    let counted = fold_tempo(analysis.bpm);
    if (counted - analysis.bpm).abs() > 0.5 {
        log::info!(
            "automix analysis {path}: tempo {:.1} BPM counted as {counted:.1} \
             (octave fold for planning)",
            analysis.bpm
        );
        analysis.bpm = counted;
        analysis.beat_interval = 60.0 / counted;
    }
    // The per-track analysis is the price of the real vocal mask: the owner's
    // devices and the iPad report it, so a slow track is visible rather than
    // just felt.
    log::info!(
        "automix analysis time {path}: {} ms",
        analysis_started.elapsed().as_millis()
    );
    analysis
}

/// The octave a tempo is counted in, for planning.
///
/// Tempo is octave-ambiguous by nature — half-time and double-time readings
/// describe the same record — and the beat models take both: the corpus reads
/// tracks at 44, 196, 198 and 208 BPM that the DSP estimator put near 88–134.
/// Folding into the band where a beat is a useful beat keeps the plan and the
/// log in one octave; the phase is unaffected, because the beat times
/// themselves are left exactly as measured.
fn fold_tempo(bpm: f64) -> f64 {
    if !bpm.is_finite() || bpm <= 0.0 {
        return bpm;
    }
    let mut folded = bpm;
    while folded >= 175.0 {
        folded /= 2.0;
    }
    while folded < 70.0 {
        folded *= 2.0;
    }
    folded
}

/// Overlay one open-unmix window onto the DSP mask on the energy-curve grid.
///
/// The model's probability wins where the window has a frame; outside it the
/// DSP heuristic stands. `offset` is the window's start on the track's timeline,
/// so the head and the tail can be merged independently (upstream `mergeMasks`).
fn merge_open_unmix_window(analysis: &mut Analysis, mask: &[f32], offset: f64) {
    if mask.is_empty() || analysis.energy_curve.is_empty() {
        return;
    }
    let hop = vocal::HOP as f64 / vocal::SAMPLE_RATE;
    let last = offset + (mask.len() as f64 - 1.0) * hop;
    let mut cursor = 0usize;
    for (index, point) in analysis.energy_curve.iter().enumerate() {
        if point.time < offset - 0.5 || point.time > last + 0.5 {
            continue;
        }
        if index >= analysis.vocal_activity_mask.len() {
            break;
        }
        while cursor + 1 < mask.len()
            && (offset + (cursor as f64 + 1.0) * hop - point.time).abs()
                < (offset + cursor as f64 * hop - point.time).abs()
        {
            cursor += 1;
        }
        if cursor < mask.len() {
            let probability = (mask[cursor] as f64
                * if point.energy > 0.25 { 1.0 } else { 0.3 })
            .clamp(0.0, 1.0);
            analysis.vocal_activity_mask[index] = probability;
        }
    }
}

fn ingest_vocals(
    analysis: &mut Analysis,
    path: &str,
    start: f64,
    duration: f64,
    decode: &impl Fn(&str, f64, f64, bool) -> Option<(Vec<f32>, u32, f64)>,
) {
    let Some((mut stereo, rate, offset)) = decode(path, start, duration, false) else {
        return;
    };
    // The decoder reads in 4096-frame chunks and can overshoot the requested
    // window by up to one chunk. The model's input is a fixed 960 frames, and a
    // 22.24 s overshoot lands at 961+ — which `vocal::track` rejects, silently
    // leaving the DSP heuristic in place. Trim to the window that was asked for.
    let wanted = ((duration.max(0.0) * rate as f64) as usize).saturating_mul(2);
    if stereo.len() > wanted {
        stereo.truncate(wanted);
    }
    let (left, right) = split_stereo(&stereo);
    let Some(mask) = vocal::track(&left, &right, rate as f64) else {
        return;
    };
    merge_open_unmix_window(analysis, &mask, offset);
}

/// F-3: the bar grid a plan is phrased on.
///
/// The Beat This! head is conservative and its tempo can be an octave out
/// (196 On a 98 BPM record), while the DSP PLL grid runs continuously through
/// the whole track and its downbeats matched 90-100% of the musical bars on
/// the corpus. So the model keeps the grid when it is complete and in the
/// same octave as the PLL; otherwise the PLL supplies the bars, phase-locked
/// to the model's downbeats where the model has them, and the beat length is
/// **measured from the bars** (median bar / 4) rather than from a possibly
/// doubled tempo. The model keeps its beats, confidence and first beat, so
/// the tier and the tempo-gap decision are unchanged.
fn reconcile_grid(analysis: &Analysis, model: Grid) -> (Grid, f64, &'static str) {
    let dsp_bars = &analysis.downbeats;
    let usable_dsp = dsp_bars.len() >= 4
        && analysis.beat_interval > 0.0
        && dsp_bars.windows(2).all(|w| w[1] > w[0]);
    let model_bars = model.downbeats.len();
    let same_octave = analysis.bpm > 0.0
        && (model.bpm / analysis.bpm).log2().fract().abs() < 0.15;
    // A model grid that fires twice as often as the PLL is not "complete":
    // it is over-dense, and its bars are not the musical bar.
    let bar_ratio = model_bars as f64 / dsp_bars.len() as f64;
    let model_complete = usable_dsp && (0.75..=1.25).contains(&bar_ratio) && same_octave;
    if !usable_dsp || model_complete {
        let source = if usable_dsp { "model (complete)" } else { "model (no PLL grid)" };
        return (model, 0.0, source);
    }
    // The PLL is the backbone. Its phase is chosen by the model's downbeats:
    // keep it when at least half of them sit on a PLL bar, otherwise try the
    // four beat phases and take the best match.
    let beat = analysis.beat_interval.max(1e-3);
    let matches = |shift: f64| -> usize {
        model
            .downbeats
            .iter()
            .filter(|d| {
                dsp_bars
                    .iter()
                    .any(|bar| (bar + shift - *d).abs() <= 0.04)
            })
            .count()
    };
    let mut best_shift = 0.0;
    let mut best = matches(0.0);
    for step in 1..4 {
        let shift = step as f64 * beat;
        let count = matches(shift);
        if count > best {
            best = count;
            best_shift = shift;
        }
    }
    let phase_ok = !model.downbeats.is_empty() && best * 2 >= model.downbeats.len();
    if !phase_ok {
        best_shift = 0.0;
    }
    let mut downbeats: Vec<f64> = dsp_bars
        .iter()
        .map(|bar| bar + best_shift)
        .filter(|bar| bar.is_finite() && *bar >= 0.0)
        .collect();
    downbeats.sort_by(|a, b| a.total_cmp(b));
    if downbeats.len() < 4 {
        return (model, 0.0, "model (PLL too short)");
    }
    // The measured bar sets the beat, so a double-time model cannot halve the
    // phrase. The model's beats/confidence/first beat stay.
    let mut gaps: Vec<f64> = downbeats
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|g| g.is_finite() && *g > 0.0)
        .collect();
    gaps.sort_by(|a, b| a.total_cmp(b));
    let bar_seconds = gaps.get(gaps.len() / 2).copied().unwrap_or(0.0);
    if bar_seconds <= 0.4 {
        return (model, 0.0, "model (PLL bars unusable)");
    }
    // The model keeps its tempo, beats and confidence — the tier and the
    // tempo-gap decision are unchanged — and only the bars move to the PLL.
    // The phrase length is measured from those bars (median / 4).
    let mut grid = model;
    grid.downbeats = downbeats;
    let source = if best_shift.abs() < 1e-6 {
        "PLL (phase kept)"
    } else {
        "PLL (phase shifted to the model)"
    };
    (grid, bar_seconds / 4.0, source)
}

fn fill_grid(analysis: &mut Analysis, grid: Grid) {
    analysis.bpm = grid.bpm;
    analysis.beat_interval = grid.beat_interval;
    analysis.beat_confidence = grid.beat_confidence;
    analysis.downbeats = grid.downbeats;
    analysis.first_beat = grid.first_beat;
    analysis.beats = grid.beats;
    // The DSP phrases were built on the DSP grid. Eight downbeats is the same
    // 32-beat phrase, on the grid that actually won.
    analysis.phrase_boundaries = analysis.downbeats.iter().step_by(8).copied().collect();
    if analysis.analysis_source.is_empty() || analysis.analysis_source == "dsp" {
        analysis.analysis_source = "beat_this".into();
    }
}

fn split_stereo(interleaved: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let mut left = Vec::with_capacity(interleaved.len() / 2);
    let mut right = Vec::with_capacity(interleaved.len() / 2);
    for pair in interleaved.chunks_exact(2) {
        left.push(pair[0]);
        right.push(pair[1]);
    }
    (left, right)
}

// ---- Bass-swap placement (upstream `bassSwapFractionFor`) ------------------

fn average_low_energy(curve: &[audio_analysis::EnergyPoint], from: f64, until: f64) -> Option<f64> {
    if until <= from {
        return None;
    }
    let mut index = curve.partition_point(|p| p.time < from);
    let mut sum = 0.0;
    let mut count = 0usize;
    while index < curve.len() && curve[index].time < until {
        let point = &curve[index];
        if point.time.is_finite() && point.energy.is_finite() && point.energy >= 0.0 {
            sum += point.energy;
            count += 1;
        }
        index += 1;
    }
    if count > 0 {
        Some(sum / count as f64)
    } else {
        None
    }
}

fn low_energy_reference(curve: &[audio_analysis::EnergyPoint]) -> Option<f64> {
    let mut energies: Vec<f64> = curve
        .iter()
        .map(|p| p.energy)
        .filter(|e| e.is_finite() && *e >= 0.0)
        .collect();
    if energies.is_empty() {
        return None;
    }
    energies.sort_by(|a, b| a.total_cmp(b));
    let upper_decile = energies[((energies.len() - 1) as f64 * 0.9) as usize];
    let reference = upper_decile.max(energies.last().copied().unwrap_or(0.0) * 0.25);
    if reference > 1e-9 {
        Some(reference)
    } else {
        None
    }
}

fn low_energy_resolution(curve: &[audio_analysis::EnergyPoint]) -> f64 {
    let mut gaps: Vec<f64> = curve
        .windows(2)
        .map(|w| w[1].time - w[0].time)
        .filter(|g| g.is_finite() && *g > 0.0)
        .collect();
    gaps.sort_by(|a, b| a.total_cmp(b));
    gaps.get(gaps.len() / 2).copied().unwrap_or(0.0)
}

fn low_energy_change(
    curve: &[audio_analysis::EnergyPoint],
    reference: Option<f64>,
    at: f64,
    window_seconds: f64,
) -> Option<f64> {
    if curve.is_empty() || window_seconds <= 0.0 {
        return None;
    }
    let reference = reference?;
    let before = average_low_energy(curve, at - window_seconds, at)?;
    let after = average_low_energy(curve, at, at + window_seconds)?;
    Some((after / reference).clamp(0.0, 1.5) - (before / reference).clamp(0.0, 1.5))
}

/// Chooses one shared-grid beat for the low-end handoff from the low-energy
/// structure of both tracks (upstream `bassSwapFractionFor`).
fn bass_swap_fraction_for(
    outgoing: &Analysis,
    incoming: &Analysis,
    transition_start: f64,
    incoming_cue_time: f64,
    outgoing_beat_seconds: f64,
    incoming_beat_seconds: f64,
    overlap_seconds: f64,
    overlap_beats: usize,
) -> f64 {
    if overlap_seconds <= 0.0 {
        return DEFAULT_BASS_SWAP_FRACTION;
    }
    let latest_fraction =
        (MAX_BASS_SWAP_FRACTION.min(BASS_SWAP_MAX_SECONDS / overlap_seconds)).clamp(0.0, 1.0);
    let prior = DEFAULT_BASS_SWAP_FRACTION.min(latest_fraction);
    if overlap_beats < 2 {
        return prior;
    }

    let earliest_fraction = HANDOFF_FRACTION.min(latest_fraction);
    let earliest_beat = ((earliest_fraction * overlap_beats as f64 - 1e-9).ceil() as usize)
        .clamp(1, overlap_beats - 1);
    let latest_beat = ((latest_fraction * overlap_beats as f64 + 1e-9).floor() as usize)
        .clamp(earliest_beat, overlap_beats - 1);
    let candidates: Vec<usize> = (earliest_beat..=latest_beat).collect();
    // `minWithOrNull`: closest to the prior, then beat-on-a-bar preferred.
    let fallback_beat = match candidates.iter().min_by(|a, b| {
        let da = (**a as f64 / overlap_beats as f64 - prior).abs();
        let db = (**b as f64 / overlap_beats as f64 - prior).abs();
        da.total_cmp(&db).then_with(|| {
            (if **a % 4 == 0 { 0 } else { 1 }).cmp(&(if **b % 4 == 0 { 0 } else { 1 }))
        })
    }) {
        Some(beat) => *beat,
        None => return prior,
    };

    let outgoing_reference = low_energy_reference(&outgoing.low_energy_curve);
    let incoming_reference = low_energy_reference(&incoming.low_energy_curve);
    let outgoing_window =
        outgoing_beat_seconds.max(low_energy_resolution(&outgoing.low_energy_curve) * 1.1);
    let incoming_window =
        incoming_beat_seconds.max(low_energy_resolution(&incoming.low_energy_curve) * 1.1);

    let mut strongest: Option<(usize, f64)> = None;
    for beat in &candidates {
        let outgoing_at = transition_start + *beat as f64 * outgoing_beat_seconds;
        let incoming_at = incoming_cue_time + *beat as f64 * incoming_beat_seconds;
        let incoming_change = low_energy_change(
            &incoming.low_energy_curve,
            incoming_reference,
            incoming_at,
            incoming_window,
        );
        let outgoing_change = low_energy_change(
            &outgoing.low_energy_curve,
            outgoing_reference,
            outgoing_at,
            outgoing_window,
        );
        // Instrument-activity proxy: prefer the beat where outgoing bass falls
        // and incoming bass rises (Music Understanding–style activity masks).
        let out_bass_before = mask_activity_between(
            &outgoing.bass_activity_mask,
            &outgoing.energy_curve,
            outgoing_at - outgoing_window,
            outgoing_at,
        );
        let out_bass_after = mask_activity_between(
            &outgoing.bass_activity_mask,
            &outgoing.energy_curve,
            outgoing_at,
            outgoing_at + outgoing_window,
        );
        let in_bass_before = mask_activity_between(
            &incoming.bass_activity_mask,
            &incoming.energy_curve,
            incoming_at - incoming_window,
            incoming_at,
        );
        let in_bass_after = mask_activity_between(
            &incoming.bass_activity_mask,
            &incoming.energy_curve,
            incoming_at,
            incoming_at + incoming_window,
        );
        let bass_handoff = match (
            out_bass_before,
            out_bass_after,
            in_bass_before,
            in_bass_after,
        ) {
            (Some(ob), Some(oa), Some(ib), Some(ia)) => (ob - oa) + (ia - ib),
            _ => 0.0,
        };
        if incoming_change.is_none() && outgoing_change.is_none() && bass_handoff.abs() < 0.05 {
            continue;
        }
        let score =
            incoming_change.unwrap_or(0.0) - outgoing_change.unwrap_or(0.0) + bass_handoff * 0.35;
        let candidate = (*beat, score);
        strongest = Some(match strongest {
            None => candidate,
            Some(current) => {
                // `maxWithOrNull`: score desc, then beat-on-bar, then closest to prior.
                let mut better = candidate.1 > current.1;
                if candidate.1 == current.1 {
                    let c_bar = if candidate.0 % 4 == 0 { 1 } else { 0 };
                    let s_bar = if current.0 % 4 == 0 { 1 } else { 0 };
                    better = c_bar > s_bar;
                    if c_bar == s_bar {
                        better = (candidate.0 as f64 / overlap_beats as f64 - prior).abs()
                            < (current.0 as f64 / overlap_beats as f64 - prior).abs();
                    }
                }
                if better {
                    candidate
                } else {
                    current
                }
            }
        });
    }

    let chosen_beat = match strongest {
        Some((beat, score)) if score >= MIN_BASS_STRUCTURE_SCORE => beat,
        _ => fallback_beat,
    };
    chosen_beat as f64 / overlap_beats as f64
}

// ---- Mix-out anchor ranking (upstream `rankMixOutCandidates`) ---------------

fn mix_out_type_score(kind: &str) -> f64 {
    match kind {
        "energy_cliff" | "interior_mix_out" => 0.95,
        "outro_start" => 0.9,
        "section_boundary" => 0.88,
        "content_end" => 0.75,
        _ => 0.0,
    }
}

/// Audible seconds (energy ≥ 10 % of the track reference) between `start` and
/// `end`, measured on the energy curve (upstream `audibleSecondsBetween`).
fn audible_seconds_between(
    curve: &[audio_analysis::EnergyPoint],
    start: f64,
    end: f64,
) -> Option<f64> {
    if curve.len() < 2 || end <= start {
        return None;
    }
    let mut energies: Vec<f64> = curve
        .iter()
        .map(|p| p.energy)
        .filter(|e| e.is_finite() && *e >= 0.0)
        .collect();
    if energies.is_empty() {
        return None;
    }
    energies.sort_by(|a, b| a.total_cmp(b));
    let reference = energies[((energies.len() - 1) as f64 * 0.85) as usize];
    if reference <= 0.0 {
        return Some(0.0);
    }
    let threshold = reference * AUDIBLE_ENERGY_FRACTION;
    let first = curve.first().unwrap().time;
    let last = curve.last().unwrap().time;
    if !first.is_finite() || !last.is_finite() || last <= first {
        return None;
    }
    let sample_seconds = (last - first) / (curve.len() - 1) as f64;
    let mut audible = 0.0;
    for point in curve {
        if !point.time.is_finite() || point.time < start || point.time > end {
            continue;
        }
        if point.energy >= threshold {
            audible += sample_seconds;
        }
    }
    Some(audible)
}

/// Mean of a parallel activity mask over `start`..`end` (same grid as energy).
fn mask_activity_between(
    mask: &[f64],
    curve: &[audio_analysis::EnergyPoint],
    start: f64,
    end: f64,
) -> Option<f64> {
    if mask.is_empty() || mask.len() != curve.len() || end <= start {
        return None;
    }
    let mut sum = 0.0;
    let mut count = 0usize;
    for index in 0..mask.len() {
        let time = curve[index].time;
        if !time.is_finite() || time < start || time > end {
            continue;
        }
        let value = mask[index];
        if !value.is_finite() {
            continue;
        }
        sum += value;
        count += 1;
    }
    if count > 0 {
        Some(sum / count as f64)
    } else {
        None
    }
}

/// Bonus when pace drops across a candidate mix-out (structure + novelty cue).
fn pace_drop_bonus(analysis: &Analysis, time: f64) -> f64 {
    if analysis.pace_curve.len() < 4 {
        return 0.0;
    }
    let before = analysis
        .pace_curve
        .iter()
        .filter(|p| p.time >= time - 4.0 && p.time < time)
        .map(|p| p.energy)
        .collect::<Vec<_>>();
    let after = analysis
        .pace_curve
        .iter()
        .filter(|p| p.time >= time && p.time <= time + 4.0)
        .map(|p| p.energy)
        .collect::<Vec<_>>();
    if before.is_empty() || after.is_empty() {
        return 0.0;
    }
    let mean = |vals: &[f64]| vals.iter().sum::<f64>() / vals.len() as f64;
    let drop = mean(&before) - mean(&after);
    if drop > 0.08 {
        0.06
    } else if drop > 0.03 {
        0.03
    } else {
        0.0
    }
}

/// Where the outgoing track's transition ends: the best-ranked mix-out
/// candidate inside the discarded-music budget, or the content end. Returns
/// `(time, discarded_music_seconds)` (upstream `resolveMixOutAnchor`).
fn resolve_mix_out_anchor(outgoing: &Analysis) -> (f64, f64) {
    let end = if outgoing.content_end > 0.0 {
        outgoing.content_end
    } else {
        outgoing.duration
    };
    if end <= 0.0 {
        return (0.0, 0.0);
    }
    let mut candidates = outgoing.mix_out_candidates.clone();
    if !candidates.iter().any(|c| (c.time - end).abs() < 0.05) {
        candidates.push(audio_analysis::MixCuePoint {
            time: end,
            score: 0.75,
            kind: "content_end".into(),
        });
    }

    let mut best: Option<(f64, f64, f64)> = None; // (rank_score, time, discarded)
    for candidate in &candidates {
        let measured = audible_seconds_between(&outgoing.energy_curve, candidate.time, end);
        let discarded = measured.unwrap_or((end - candidate.time).max(0.0));
        if discarded > MAX_DISCARDED_MUSIC_SECONDS {
            continue;
        }
        let rank_score = candidate.score
            + mix_out_type_score(&candidate.kind)
            + pace_drop_bonus(outgoing, candidate.time);
        best = Some(match best {
            None => (rank_score, candidate.time, discarded),
            Some((score, time, discarded_seconds)) => {
                if rank_score > score || (rank_score == score && candidate.time > time) {
                    (rank_score, candidate.time, discarded)
                } else {
                    (score, time, discarded_seconds)
                }
            }
        });
    }
    match best {
        Some((_, time, discarded)) => (time, discarded),
        None => (end, 0.0),
    }
}

// ---- Harmonic key routing (upstream TransitionPlanner key helpers) ----------

fn key_index(name: &str) -> Option<usize> {
    Some(match name {
        "C" => 0,
        "C♯" | "D♭" => 1,
        "D" => 2,
        "D♯" | "E♭" => 3,
        "E" => 4,
        "F" => 5,
        "F♯" | "G♭" => 6,
        "G" => 7,
        "G♯" | "A♭" => 8,
        "A" => 9,
        "A♯" | "B♭" => 10,
        "B" => 11,
        _ => return None,
    })
}

fn split_key(key: &str) -> (Option<usize>, Option<&str>) {
    let mut parts = key.trim().split(' ');
    let index = parts.next().and_then(key_index);
    let mode = parts.next();
    (index, mode)
}

fn key_distance(left: &str, right: &str) -> Option<usize> {
    let (left_index, left_mode) = split_key(left);
    let (right_index, right_mode) = split_key(right);
    let left_index = left_index?;
    let right_index = right_index?;
    let pitch = (left_index + 12 - right_index) % 12;
    let pitch = pitch.min((right_index + 12 - left_index) % 12);
    Some(
        pitch
            + if left_mode.is_some() && right_mode.is_some() && left_mode != right_mode {
                1
            } else {
                0
            },
    )
}

/// A key the analyzer was not confident about is no key at all.
fn trusted_key(analysis: &Analysis) -> String {
    if analysis.key.is_empty() || analysis.key_confidence < 0.25 {
        String::new()
    } else {
        analysis.key.clone()
    }
}

/// Upstream `harmonicallyCompatible`: a second/fifth (same mode) or a second
/// (differing modes) is a move a DJ makes; anything wider is not.
fn harmonically_compatible(left: &str, right: &str) -> bool {
    let (left_index, left_mode) = split_key(left);
    let (right_index, right_mode) = split_key(right);
    let left_index = match left_index {
        Some(i) => i,
        None => return false,
    };
    let right_index = match right_index {
        Some(i) => i,
        None => return false,
    };
    let distance = (left_index + 12 - right_index) % 12;
    let distance = distance.min((right_index + 12 - left_index) % 12);
    if left_mode.is_some() && right_mode.is_some() && left_mode != right_mode {
        return distance <= 1;
    }
    distance <= 2 || distance == 5
}

fn plan_from(
    outgoing: &Analysis,
    incoming: &Analysis,
    fade: f64,
    allow_effects: bool,
) -> TransitionPlan {
    let entry = incoming_entry(incoming);
    // Upstream `MIN_SMART_DURATION_SECONDS`: a track too short to spend a smart
    // transition on gets a plain crossfade (TransitionPlanner.kt:880-882).
    let too_short = (outgoing.duration > 0.0 && outgoing.duration < MIN_SMART_DURATION_SECONDS)
        || (incoming.duration > 0.0 && incoming.duration < MIN_SMART_DURATION_SECONDS);
    let tier = assess(outgoing, incoming);
    // The analysed blend is the default; an effect only seasons a pair the
    // blend cannot take. A track too short to spend a smart transition on gets
    // a plain fade, as upstream does.
    if too_short {
        let mut plan = plain_fallback(fade, outgoing);
        plan.cue_seconds = entry;
        plan.reason = "fade: a track is too short for a technique".into();
        return plan;
    }
    if matches!(tier, Tier::Plain) {
        if allow_effects {
            if let Some(plan) = technique_plan(outgoing, incoming, tier, entry, None, None) {
                return plan;
            }
        }
        let mut plan = plain_fallback(fade, outgoing);
        plan.cue_seconds = entry;
        plan.reason = "fade: no usable tempo on either side".into();
        return plan;
    }

    let out_bpm = outgoing.bpm;
    let in_bpm = incoming.bpm;
    let ratio = normalized_tempo_ratio(out_bpm, in_bpm);
    let same_beat = (1.0 - ratio).abs() <= 0.05
        && (outgoing.beat_confidence >= 0.2 || incoming.beat_confidence >= 0.2);
    // Only Beatmatched may stretch, and only inside MAX_BEATMATCH_STRETCH.
    // DjAssisted used to warp up to ±10% on the 0.9–1.1 band — that is the
    // Apple failure mode (tempo morph that draws attention to itself). A weak
    // grid keeps unity rate and lets the filter ride mask the gap instead.
    let playback_rate = if matches!(tier, Tier::Beatmatched) {
        transparent_playback_rate(out_bpm, in_bpm, MAX_BEATMATCH_STRETCH).unwrap_or(1.0)
    } else {
        1.0
    };
    // The whole-track vocal probability cannot answer "does this handover put
    // two singers together": it sits at 0.82–0.86 on all thirteen corpus
    // tracks, sung or not. The windows the handover actually covers do
    // discriminate — 0.28 through an instrumental outro, 0.91 through a sung
    // one — so the clash is read there.
    let (anchor, _) = resolve_mix_out_anchor(outgoing);
    let vocal_conflict = handover_vocal_clash(outgoing, incoming, anchor, entry);
    // A confident beat match with both voices out of the way earns the long
    // blend. Two singers, or a weak grid, stays inside upstream's 8–16 beats.
    let long = long_blend_allowed(outgoing, incoming) && !vocal_conflict;
    let key_distance = key_distance(&trusted_key(outgoing), &trusted_key(incoming));
    let overlap_beats = if long {
        LONG_BLEND_BEATS as f64
    } else if !vocal_conflict && ((1.0 - ratio).abs() > 0.07 || key_distance.is_some_and(|d| d > 4))
    {
        16.0
    } else {
        8.0
    };
    let beat_seconds = if out_bpm > 0.0 { 60.0 / out_bpm } else { 0.5 };
    let min_overlap = if out_bpm >= 140.0 {
        AUTO_FAST_TRACK_MIN_SECONDS
    } else {
        AUTO_MIN_SECONDS
    };
    let max_seconds = if long {
        LONG_BLEND_SECONDS
    } else {
        AUTO_TRANSITION_MAX_SECONDS
    };
    let max_beats_cap = if long {
        LONG_BLEND_BEATS as f64
    } else {
        AUTO_TRANSITION_MAX_BEATS
    };
    let overlap = (overlap_beats * beat_seconds).clamp(min_overlap, max_seconds);

    // Where the outgoing record should be gone: its mix-out anchor, pulled
    // forward by an arrangement overlap when that anchor is just the content
    // end (upstream `outgoingArrangementOverlap`).
    let content_end = if outgoing.content_end > 0.0 {
        outgoing.content_end
    } else {
        outgoing.duration
    };
    let arrangement = if same_beat && (anchor - content_end).abs() < 0.05 && out_bpm > 0.0 {
        (ARRANGEMENT_OVERLAP_BEATS * 60.0 / out_bpm).min(MAX_DISCARDED_MUSIC_SECONDS)
    } else {
        0.0
    };
    let mix_end = (anchor - arrangement).max(0.0);
    let mut max_overlap = (max_beats_cap * beat_seconds)
        .min(max_seconds)
        .min((mix_end * 0.4).max(min_overlap))
        .min(if incoming.duration > 0.0 {
            (incoming.duration * 0.4).max(min_overlap)
        } else {
            max_seconds
        })
        .max(min_overlap);
    // Mixxx Auto DJ: fade length ≈ min(usable outro, usable intro) when both
    // sections are known. Caps the overlap before handoff math so the cue and
    // the fade agree.
    if let Some(cap) = section_fade_cap(outgoing, incoming) {
        max_overlap = max_overlap.min(cap.max(min_overlap));
    }

    let handoff_beats = if same_beat { 8.0 } else { 4.0 };
    let handoff_seconds = if out_bpm > 0.0 {
        (handoff_beats * 60.0 / out_bpm).clamp(2.0, if same_beat { 6.0 } else { 5.0 })
    } else {
        4.0
    };

    // The drop is the handoff, not the cue. Playback starts `overlap` earlier,
    // and never more than eight seconds past the entry.
    let drop = incoming_handoff(incoming, max_overlap);
    let aligned_in = align_tempo_octave(out_bpm, in_bpm);
    let requested_handoff = if same_beat && aligned_in > 0.0 {
        drop + ARRANGEMENT_OVERLAP_BEATS * 60.0 / aligned_in
    } else {
        drop
    };
    let max_in_handoff = incoming.duration - MIN_CLEARANCE_SECONDS;
    let handoff = if max_in_handoff >= drop {
        requested_handoff.min(max_in_handoff)
    } else {
        drop
    };

    // A Beatmatched pair with a transparent stretch earns DjBlend. Anything
    // else — DjAssisted, far tempo, refused stretch — rides filters so the
    // ear never hears a chipmunk or a slow-mo vocal.
    let beatmatched_blend = matches!(tier, Tier::Beatmatched)
        && same_beat
        && (playback_rate - 1.0).abs() <= MAX_BEATMATCH_STRETCH + 1e-6;

    let (transition_start, fade_seconds) = if beatmatched_blend && beat_seconds > 0.0 {
        let intro_span = handoff / playback_rate.max(0.8);
        let total = intro_span.clamp(12.0_f64.min(max_overlap), max_overlap);
        let target = (mix_end - total).max(0.0);
        let earliest = (mix_end - max_overlap).max(0.0);
        let start = aligned_transition_start(
            outgoing,
            target,
            (mix_end - 0.05).max(earliest),
            true,
            earliest,
        );
        realized_window(mix_end, start, total, min_overlap)
    } else {
        let desired = overlap.max(handoff_seconds * 0.42);
        let actual = desired.clamp(handoff_seconds.min(max_overlap), max_overlap);
        let target = (mix_end - actual).max(0.0);
        let earliest = (mix_end - max_overlap).max(0.0);
        let start = aligned_transition_start(
            outgoing,
            target,
            (mix_end - 0.05).max(earliest),
            desired > overlap + 0.5,
            earliest,
        );
        realized_window(mix_end, start, actual, min_overlap)
    };
    let cue = cue_for(entry, handoff, fade_seconds, playback_rate);

    let incoming_beat_seconds = if in_bpm > 0.0 { 60.0 / in_bpm } else { 0.5 };
    let bass_swap_fraction = if beatmatched_blend {
        bass_swap_fraction_for(
            outgoing,
            incoming,
            transition_start,
            cue,
            beat_seconds,
            incoming_beat_seconds,
            fade_seconds,
            overlap_beats as usize,
        )
    } else {
        DEFAULT_BASS_SWAP_FRACTION
    };
    let vocal_overlap = planned_vocal_overlap(
        outgoing,
        incoming,
        transition_start,
        transition_start + fade_seconds,
        cue,
        playback_rate,
    );
    let style = if beatmatched_blend {
        TransitionStyle::DjBlend
    } else {
        TransitionStyle::DjFilter
    };
    let shape = mix_shape(
        style,
        beatmatched_blend,
        vocal_overlap,
        vocal_conflict,
        fade_seconds,
        beat_seconds,
        bass_swap_fraction,
        playback_rate,
        out_bpm,
    );

    // A usable blend is the answer for every pair except a real clash: a key
    // the stretch cannot reconcile (so the two never overlap dry), a tempo gap
    // past the filter blend's 7 % band, or a vocal-on-vocal overlap the planner
    // could not shorten below the clash threshold. Only then may an effect
    // replace the blend; the v0.1.3 blend remains the default.
    let tempo_gap = (1.0 - ratio).abs();
    // A stretch does not change key; a wide key distance is a real clash on
    // its own. The corpus firing rates will decide whether that should earn an
    // effect or a shorter filtered blend.
    let key_clash = keys_clash(&outgoing.key, outgoing.key_confidence, &incoming.key, incoming.key_confidence);
    // An unmatched tempo or a key clash earns a technique, and the session's
    // pacing (`allow_effects`, up to three transitions in four) decides whether it
    // gets one. The owner's call (2026-10-24): the effects are part of the
    // set, so a spent budget, not a falling tail, is what sends an unmatched
    // pair to the clean crossfade below (principle H, now the default
    // rather than the rule).
    let out_busy = handover_is_busy(outgoing, mix_end);
    let in_busy = handover_is_busy(incoming, cue);
    log::info!(
        "automix clash check [{:?}]: key_clash={key_clash} tempo_gap={tempo_gap:.3} \
         vocal_conflict={vocal_conflict} vocal_overlap={vocal_overlap:.3} \
         out_busy={out_busy} in_busy={in_busy}",
        tier
    );
    let effect_wanted = effect_earned(key_clash, tempo_gap);
    if effect_wanted && allow_effects {
        if let Some(plan) =
            technique_plan(
                outgoing,
                incoming,
                tier,
                entry,
                Some((transition_start, fade_seconds, cue)),
                if out_busy { None } else { last_strong_downbeat(outgoing, mix_end) },
            )
        {
            return plan;
        }
    }
    if !key_clash && tempo_gap > 0.07 && falls_away(out_busy, in_busy) {
        // The tempo gap is the only reason an effect was on the table, and the
        // outgoing record falls away: cross over where it does, for as long as
        // it takes, at the rate the record already plays. Both sides busy is
        // the one case with nowhere to cross, so it is not offered here.
        if let Some(plan) = clean_crossfade(outgoing, mix_end, entry) {
            return plan;
        }
    }

    TransitionPlan {
        // Beatmatched + transparent stretch → DjBlend. DjAssisted used to
        // blend on tempo proximity alone; that stretched weak grids. Filter.
        style,
        bass_swap: true,
        bass_swap_fraction,
        filter_sweep: if same_beat { 0.0 } else { FILTER_SWEEP },
        vocal_overlap,
        fade_seconds,
        outgoing_hold_seconds: 0.0,
        bridge_start_seconds: 0.0,
        bridge_seconds: 0.0,
        bridge_repeats: 0,
        effect_seconds: 0.0,
        effect_start_seconds: 0.0,
        transition_end_seconds: transition_start + fade_seconds,
        cue_seconds: cue,
        playback_rate,
        bed_fraction: shape.bed_fraction,
        bed_gain_db: shape.bed_gain_db,
        dip_depth: shape.dip_depth,
        dip_width: shape.dip_width,
        post_glide_seconds: shape.post_glide_seconds,
        outgoing_duration_seconds: outgoing.duration,
        beat_seconds: 0.0,
        reason: if effect_wanted && !allow_effects {
            // The pair earned a technique by the planner's own rules and the
            // session's pacing budget said no.
            "pacing: effect budget spent, so the pair is blended".to_string()
        } else if vocal_conflict {
            format!(
                "blend: both records sing through the handover; mids ducked {:.0}% for {:.0}% of it",
                shape.dip_depth * 100.0,
                vocal_overlap * 100.0
            )
        } else {
            String::new()
        },
    }
}

struct MixShape {
    bed_fraction: f64,
    bed_gain_db: f64,
    dip_depth: f64,
    dip_width: f64,
    post_glide_seconds: f64,
}

/// Bed, dip and post-blend glide for one planned blend. A plain crossfade
/// gets zeros, which is how the mixer knows to stay equal-power.
fn mix_shape(
    style: TransitionStyle,
    same_beat: bool,
    vocal_overlap: f64,
    vocal_conflict: bool,
    fade_seconds: f64,
    beat_seconds: f64,
    swap_fraction: f64,
    playback_rate: f64,
    bpm: f64,
) -> MixShape {
    if !matches!(style, TransitionStyle::DjBlend | TransitionStyle::DjFilter) {
        return MixShape {
            bed_fraction: 0.0,
            bed_gain_db: 0.0,
            dip_depth: 0.0,
            dip_width: 0.0,
            post_glide_seconds: 0.0,
        };
    }
    let clash = vocal_overlap
        .clamp(0.0, 1.0)
        .max(if vocal_conflict { 0.7 } else { 0.0 });
    let dip = if same_beat {
        (0.28 + 0.5 * clash).clamp(0.0, MAX_DUCK_DEPTH)
    } else {
        (0.2 * clash).clamp(0.0, MAX_DUCK_DEPTH)
    };
    let rise = if fade_seconds > 0.0 && beat_seconds > 0.0 {
        (2.0 * beat_seconds / fade_seconds).clamp(0.08, 0.3)
    } else {
        0.15
    };
    let bed = (swap_fraction - rise).clamp(0.15, 0.8);
    let dip_width = if fade_seconds > 0.0 && beat_seconds > 0.0 {
        (beat_seconds / fade_seconds).clamp(0.02, 0.2)
    } else {
        0.06
    };
    let post_glide_seconds = if same_beat && (playback_rate - 1.0).abs() > 1e-3 && bpm > 0.0 {
        POST_BLEND_GLIDE_BEATS * 60.0 / bpm
    } else {
        0.0
    };
    MixShape {
        bed_fraction: bed,
        bed_gain_db: BED_GAIN_DB,
        dip_depth: dip,
        dip_width,
        post_glide_seconds,
    }
}

// ---- DJ techniques ---------------------------------------------------------
//
// A blend needs two grids that agree. Most pairs an autoplay run produces do
// not: different tempos, different genres, a vocal on both sides. A DJ does not
// fade those — they finish the outgoing record with an effect and drop the next
// one on its own downbeat. [`technique_plan`] picks which effect, from what the
// two ends of the pair actually sound like, and returns `None` only when a
// blend is the better move (a real beatmatch) or nothing is known.

/// Mean energy over `[start, end]`, 0…1-ish; `None` with no curve.
fn energy_between(analysis: &Analysis, start: f64, end: f64) -> Option<f64> {
    let values: Vec<f64> = analysis
        .energy_curve
        .iter()
        .filter(|p| p.time.is_finite() && p.time >= start && p.time <= end && p.energy.is_finite())
        .map(|p| p.energy)
        .collect();
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// Deterministic variety: the same pair always gets the same technique (so a
/// replan does not flip it), different pairs spread across the eligible set.
fn pair_salt(outgoing: &Analysis, incoming: &Analysis) -> usize {
    let a = (outgoing.duration * 1000.0) as u64;
    let b = (incoming.duration * 1000.0) as u64;
    let c = (outgoing.bpm * 100.0) as u64 ^ ((incoming.bpm * 100.0) as u64).rotate_left(17);
    (a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.rotate_left(29) ^ c) as usize
}

/// Whether a pair's blend is a clash an effect should season. The 4–7 %
/// DjFilter band is still a usable blend; only a gap past seven percent or a key
/// the stretch cannot reconcile earns an effect.
///
/// A vocal overlap is deliberately *not* on this list. Two records singing at
/// once is what a filter blend and the mid duck are for — `mix_shape` deepens
/// the dip with the overlap, and the style falls to DjFilter — so the answer is
/// a ducked blend rather than an effect that throws the outgoing record away.
/// The overlap measured by `simultaneous_vocal_fraction` runs 0.0–1.0 across the
/// corpus, so it can tell a real one from silence; it is simply not evidence
/// that a blend is impossible.
fn effect_earned(key_clash: bool, tempo_gap: f64) -> bool {
    key_clash || tempo_gap > 0.07
}

/// Does at least one side fall away under the handover?
///
/// Principle H: an unmatched pair is answered with a clean crossfade *when
/// there is somewhere to cross over*. That means the outgoing record's tail is
/// quieter than the record, or the incoming's opening is — the two cases where
/// an equal-power fade has room to work. Two records still at full energy on
/// both sides of the join leave nothing to fade through, and that is when a
/// technique is the musical answer.
fn falls_away(out_busy: bool, in_busy: bool) -> bool {
    !(out_busy && in_busy)
}

/// The techniques the last few *other* transitions used, newest first, keyed
/// by pair salt so a re-plan replaces its own entry. Process-wide in the app
/// (plans run on several threads); per thread under test, so parallel tests
/// cannot steer each other's choices.
#[cfg(not(test))]
static RECENT_TECHNIQUES: std::sync::Mutex<Vec<(usize, TransitionStyle)>> = std::sync::Mutex::new(Vec::new());
#[cfg(test)]
thread_local! {
    static RECENT_TECHNIQUES: std::cell::RefCell<Vec<(usize, TransitionStyle)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn with_recent_techniques<R>(body: impl FnOnce(&mut Vec<(usize, TransitionStyle)>) -> R) -> R {
    #[cfg(not(test))]
    {
        let mut recent = RECENT_TECHNIQUES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        body(&mut recent)
    }
    #[cfg(test)]
    {
        RECENT_TECHNIQUES.with(|recent| body(&mut recent.borrow_mut()))
    }
}

fn recent_techniques(salt: usize) -> Vec<TransitionStyle> {
    with_recent_techniques(|recent| {
        recent.iter().rev().filter(|(key, _)| *key != salt).map(|(_, style)| *style).collect()
    })
}

fn note_technique(salt: usize, style: TransitionStyle) {
    with_recent_techniques(|recent| {
        recent.retain(|(key, _)| *key != salt);
        recent.push((salt, style));
        if recent.len() > 4 {
            recent.remove(0);
        }
    });
}

/// The most outgoing tail an effect thrown early may skip.
const MAX_THROW_DISCARD_SECONDS: f64 = 10.0;

/// Before an effect takes the outgoing out, the incoming plays under it for
/// this many of its beats (at least [`EFFECT_INTRO_MIN_SECONDS`]), so the
/// listener hears the next record arrive while the last one is still going.
/// Without it the next record sat 12 dB down until the throw: a cloud, then
/// a new song, never the two together.
const EFFECT_INTRO_BEATS: f64 = 8.0;
const EFFECT_INTRO_MIN_SECONDS: f64 = 4.0;
/// The incoming's level by the throw, relative to its matched level. Its bass
/// is held out (and on a tempo clash its top too), so at this level it is
/// plainly there without the two low ends or grids fighting.
const EFFECT_INTRO_DB: f64 = -4.0;
/// Clashing keys overlap for the same span, further down.
const EFFECT_INTRO_KEY_CLASH_DB: f64 = -9.0;
/// A cut keeps its impact: the next record is only a hint before it.
const DROP_CUT_INTRO_DB: f64 = -12.0;

/// Where the outgoing record is last at full strength before `mix_end`
/// (within 3 dB of its loud reference), on the downbeat at or before it: the
/// moment a DJ throws an effect when the record's own outro falls away.
/// `clean_crossfade` measures the same fall.
fn last_strong_downbeat(outgoing: &Analysis, mix_end: f64) -> Option<f64> {
    let reference = loud_reference(outgoing)?;
    let strong = outgoing
        .energy_curve
        .iter()
        .filter(|point| point.time.is_finite() && point.time <= mix_end)
        .filter(|point| point.energy >= reference * 0.707)
        .map(|point| point.time)
        .next_back()?;
    let bar = 4.0 * beat_seconds_of(outgoing);
    outgoing
        .downbeats
        .iter()
        .copied()
        .filter(|time| time.is_finite() && *time <= strong + 1e-6 && strong - *time <= bar + 1e-6)
        .next_back()
        .or(Some(strong))
}

/// Is this record still at full energy at `time`?
fn handover_is_busy(analysis: &Analysis, time: f64) -> bool {
    let Some(reference) = loud_reference(analysis) else {
        return true;
    };
    let window = 2.0;
    let energy = energy_between(analysis, time - window, time + window)
        .or_else(|| energy_between(analysis, time, time + window));
    match energy {
        // Within 3 dB of the level the record spends its loud passages at.
        Some(value) => value >= reference * 0.707,
        None => true,
    }
}

/// The energy a record sits at while it is loud: its 75th percentile.
fn loud_reference(analysis: &Analysis) -> Option<f64> {
    let mut values: Vec<f64> = analysis
        .energy_curve
        .iter()
        .map(|point| point.energy)
        .filter(|energy| energy.is_finite() && *energy > 0.0)
        .collect();
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    values.get((values.len() - 1) * 3 / 4).copied()
}

/// An unmatched pair, mixed with a plain equal-power crossfade.
///
/// The length is the outgoing record's own falling tail — from the last moment
/// it is still at its loud level to its content end — clamped to what a
/// crossfade can use. No rate change, no bed, no duck: the simplest thing that
/// can work, which is what the pair gets when nothing else is justified.
fn clean_crossfade(outgoing: &Analysis, mix_end: f64, incoming_cue: f64) -> Option<TransitionPlan> {
    if mix_end <= 0.0 {
        return None;
    }
    let reference = loud_reference(outgoing)?;
    let fall_start = outgoing
        .energy_curve
        .iter()
        .filter(|point| point.time.is_finite() && point.time <= mix_end)
        .filter(|point| point.energy >= reference * 0.707)
        .map(|point| point.time)
        .next_back()
        .unwrap_or((mix_end - AUTO_FALLBACK_SECONDS).max(0.0));
    // A phrase is the shortest thing a crossfade can be and still be music: the
    // last chord has to ring under it. The beat comes from the record, so the
    // length is the analysis's, and the fall sets the floor when it is longer.
    let beat = beat_seconds_of(outgoing);
    let phrase = 8.0 * beat;
    let fall = (mix_end - fall_start)
        .max(phrase)
        .clamp(2.0, AUTO_TRANSITION_MAX_SECONDS);
    // F-4: a DJ starts a crossfade on the one. The end stays on the fall/anchor
    // rules (E/E-2 own it); the start moves to the nearest downbeat of the
    // chosen bar grid, preferring a phrase boundary (an even bar), taking or
    // giving at most one bar.
    let bar = 4.0 * beat;
    let start = mix_end - fall;
    // The phrase is the crossfade's floor; a bar snap may not eat into it.
    // Otherwise a slightly-nearer phrase bar would shorten a two-bar fade to
    // one bar and undo the length the phrase exists to guarantee.
    let min_keep = phrase.min(AUTO_TRANSITION_MAX_SECONDS);
    let snapped_start = if bar > 0.1 {
        let mut candidates: Vec<(f64, bool, f64)> = Vec::new();
        for (index, bar_time) in outgoing.downbeats.iter().enumerate() {
            if !bar_time.is_finite() {
                continue;
            }
            let distance = (bar_time - start).abs();
            if distance > bar + 1e-6 {
                continue;
            }
            let fall_after = mix_end - *bar_time;
            if fall_after + 1e-6 < min_keep || fall_after > AUTO_TRANSITION_MAX_SECONDS {
                continue;
            }
            candidates.push((*bar_time, index % 2 == 0, distance));
        }
        // A phrase boundary first, then the nearest; the length floor already
        // removed the snaps that would shorten the fade.
        candidates.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then(a.2.total_cmp(&b.2))
                .then(a.0.total_cmp(&b.0))
        });
        candidates.first().map(|(bar_time, _, _)| *bar_time)
    } else {
        None
    };
    let (fall, start, snapped) = match snapped_start {
        Some(bar_time) => (mix_end - bar_time, bar_time, true),
        None => (fall, start, false),
    };
    Some(TransitionPlan {
        // A filter fade, not a bare one: the outgoing closes down through a
        // low-pass while the next record opens up through a high-pass, so
        // two grids that do not agree are never both full-range at once.
        // The owner's rule (2026-10-24): a plain crossfade is only for a
        // pair nothing else suits.
        style: TransitionStyle::DjFilter,
        bass_swap: false,
        bass_swap_fraction: DEFAULT_BASS_SWAP_FRACTION,
        filter_sweep: FILTER_SWEEP,
        vocal_overlap: 0.0,
        fade_seconds: fall,
        outgoing_hold_seconds: 0.0,
        bridge_start_seconds: 0.0,
        bridge_seconds: 0.0,
        bridge_repeats: 0,
        effect_seconds: 0.0,
        effect_start_seconds: 0.0,
        transition_end_seconds: mix_end,
        cue_seconds: incoming_cue,
        playback_rate: 1.0,
        bed_fraction: 0.0,
        bed_gain_db: 0.0,
        dip_depth: 0.0,
        dip_width: 0.0,
        post_glide_seconds: 0.0,
        outgoing_duration_seconds: outgoing.duration,
        beat_seconds: 0.0,
        reason: format!(
            "filter fade: no shared tempo; the outgoing falls away from {fall_start:.1}s and the \
             {fall:.1}s blend starts at {start:.1}s{}",
            if snapped { " (bar-aligned)" } else { "" }
        ),
    })
}

/// Do two confident keys clash?
///
/// Both sides have to be trusted — a key the analyzer is unsure of is no key at
/// all. Relative major and minor (the same number on the Camelot wheel) and its
/// neighbours never clash, and neither do keys the semitone distance already
/// calls harmonically close; a clash needs to fail both tests.
fn keys_clash(
    left_key: &str,
    left_confidence: f64,
    right_key: &str,
    right_confidence: f64,
) -> bool {
    if left_confidence < 0.25 || right_confidence < 0.25 {
        return false;
    }
    if left_key.is_empty() || right_key.is_empty() {
        return false;
    }
    if camelot_compatible(left_key, right_key) {
        return false;
    }
    !harmonically_compatible(left_key, right_key)
}

/// Camelot-wheel neighbours: the same number across the letters (relative major
/// and minor), or one step either way with the same letter.
fn camelot_compatible(left: &str, right: &str) -> bool {
    let Some((left_number, left_minor)) = camelot(left) else {
        return false;
    };
    let Some((right_number, right_minor)) = camelot(right) else {
        return false;
    };
    if left_number == right_number {
        return left_minor != right_minor;
    }
    if left_minor != right_minor {
        return false;
    }
    let step = (left_number + 12 - right_number) % 12;
    step == 1 || step == 11
}

/// A key's Camelot number (1…12) and whether it is a minor key.
fn camelot(key: &str) -> Option<(usize, bool)> {
    let (index, mode) = split_key(key);
    let index = index?;
    let minor = mode? == "minor";
    const MINOR: [usize; 12] = [5, 12, 7, 2, 9, 4, 11, 6, 1, 8, 3, 10];
    const MAJOR: [usize; 12] = [8, 3, 10, 5, 12, 7, 2, 9, 4, 11, 6, 1];
    Some(if minor {
        (MINOR[index], true)
    } else {
        (MAJOR[index], false)
    })
}

fn technique_plan(
    outgoing: &Analysis,
    incoming: &Analysis,
    tier: Tier,
    entry: f64,
    blend: Option<(f64, f64, f64)>,
    throw_at: Option<f64>,
) -> Option<TransitionPlan> {
    let outgoing_falls = throw_at.is_some();
    let out_bpm = outgoing.bpm;
    let in_bpm = incoming.bpm;
    let has_tempo = (MIN_BPM..=MAX_BPM).contains(&out_bpm);
    let ratio = if has_tempo && (MIN_BPM..=MAX_BPM).contains(&in_bpm) {
        normalized_tempo_ratio(out_bpm, in_bpm)
    } else {
        1.0
    };
    let tempo_gap = (1.0 - ratio).abs();
    // A true beatmatch with a transparent stretch is still the best thing a
    // pair can do; leave it to the blend planner.
    if matches!(tier, Tier::Beatmatched) && tempo_gap <= MAX_STRETCH_DEVIATION {
        return None;
    }
    let duration = if outgoing.content_end > 0.0 {
        outgoing.content_end
    } else {
        outgoing.duration
    };
    if duration <= 0.0 {
        return None;
    }
    let beat = if has_tempo { beat_seconds_of(outgoing).max(0.25) } else { 0.5 };
    let grid = has_tempo && outgoing.beat_confidence >= MIN_DJ_CONFIDENCE;
    let incoming_grid = has_tempo && incoming.beat_confidence >= MIN_DJ_CONFIDENCE;

    let (anchor, _) = resolve_mix_out_anchor(outgoing);
    let mix_end = anchor.min(duration).max(0.0);
    let out_energy = energy_between(outgoing, mix_end - 8.0 * beat, mix_end);
    let in_energy = energy_between(incoming, entry, entry + 8.0 * beat.max(0.4));
    let out_vocal = vocal_activity_between(outgoing, mix_end - 4.0 * beat, mix_end)
        .unwrap_or(outgoing.vocal_probability * 0.6);
    let slow = has_tempo && out_bpm < 92.0;
    let energetic_in = in_energy.zip(out_energy).is_some_and(|(i, o)| i >= o * 0.9 && i > 0.35);
    let key_clash = key_distance(&trusted_key(outgoing), &trusted_key(incoming)).is_some_and(|d| d > 3);

    // Eligible techniques, each with why.
    let mut eligible: Vec<(TransitionStyle, &str)> = Vec::new();
    if grid && out_vocal >= 0.35 {
        eligible.push((TransitionStyle::EchoOut, "echo out: the outgoing record ends on a vocal"));
    } else if grid {
        // An echo thrown on the last beat finishes an instrumental ending as
        // well as a sung one. Without it the wash, eligible on almost every
        // unmatched pair, took two effects in three and repeated back to back.
        eligible.push((TransitionStyle::EchoOut, "echo out: thrown on the record's last beat"));
    }
    if grid && outgoing.beat_confidence >= 0.45 && !slow && out_vocal < 0.5 {
        eligible.push((TransitionStyle::LoopRoll, "loop roll: a strong grid and an instrumental ending"));
    }
    // A cut needs a record still going: on a tail that is already falling
    // it holds the next record back across a quiet stretch, a gap.
    if grid && incoming_grid && energetic_in && !outgoing_falls {
        eligible.push((TransitionStyle::DropCut, "cut on the one: the next record comes in hot"));
    }
    if slow || key_clash || !grid || tempo_gap > 0.12 {
        eligible.push((
            TransitionStyle::ReverbWash,
            if slow {
                "reverb wash: a slow record dissolves better than it cuts"
            } else if key_clash {
                "reverb wash: the keys clash, so the two never overlap dry"
            } else if !grid {
                "reverb wash: no reliable beat to cut or echo on"
            } else {
                "reverb wash: the tempos are too far apart to blend"
            },
        ));
    }
    if grid && eligible.is_empty() {
        eligible.push((TransitionStyle::EchoOut, "echo out: tempos too far apart for a blend"));
    }
    // Variety: an effect the last two other transitions used is passed over
    // while an unused one fits. Re-plans of this same pair (keyed by its
    // salt) never count as a repeat of themselves.
    let salt = pair_salt(outgoing, incoming);
    let recent = recent_techniques(salt);
    let fresh = |depth: usize| -> Vec<(TransitionStyle, &str)> {
        eligible
            .iter()
            .copied()
            .filter(|(candidate, _)| !recent.iter().take(depth).any(|used| used == candidate))
            .collect()
    };
    let choices = [fresh(2), fresh(1), eligible.clone()]
        .into_iter()
        .find(|list| !list.is_empty())
        .unwrap_or_default();
    let (style, why) = *choices.get(salt % choices.len().max(1))?;
    note_technique(salt, style);

    // The effect sits on a real crossfade: the analysed blend supplies the
    // length, the end and the cue; the technique gets the last sub-window of
    // it. A plain pair (no blend) still gets an 8 s crossfade under the effect.
    let (blend_start, blend_fade, blend_cue) = match blend {
        Some((start, fade, cue)) => (start, fade, cue),
        None => ((mix_end - AUTO_FALLBACK_SECONDS).max(0.0), AUTO_FALLBACK_SECONDS, entry),
    };
    let technique_window = crate::mixer::technique_window_seconds(style, beat)?;
    let analysed_fade = if style == TransitionStyle::DropCut {
        // A deliberate short swap, at most one bar.
        blend_fade.clamp(0.5, 4.0 * beat)
    } else {
        blend_fade.max(AUTO_MIN_SECONDS)
    };
    // The blend ends where the analysis put it; the intro only starts it
    // earlier, so the incoming is under the outgoing for two bars before the
    // technique begins.
    let blend_fade = if style == TransitionStyle::DropCut {
        analysed_fade
    } else {
        let intro = (EFFECT_INTRO_BEATS * beat).max(EFFECT_INTRO_MIN_SECONDS);
        analysed_fade
            .max(technique_window + intro)
            .min(AUTO_TRANSITION_MAX_SECONDS.max(analysed_fade))
    };
    let effect_seconds = if style == TransitionStyle::DropCut {
        (beat / 4.0).max(0.06)
    } else {
        technique_window
    }
    .min(blend_fade)
    .max(0.06);
    // On a falling tail the effect is thrown where the record is still at
    // full strength, as a DJ throws an echo on the last phrase rather than
    // into a faded outro, and the effect carries it out over what is left
    // of the fall. Without that the technique dressed a record already most
    // of the way down. At most `MAX_THROW_DISCARD_SECONDS` of tail goes.
    let natural_end = blend_start + analysed_fade;
    let end = match throw_at {
        Some(throw) if throw + effect_seconds < natural_end
            && natural_end - (throw + effect_seconds) <= MAX_THROW_DISCARD_SECONDS =>
        {
            throw + effect_seconds
        }
        _ => natural_end,
    };
    let cue = if style == TransitionStyle::DropCut {
        let drop = incoming_handoff(incoming, 8.0 * beat);
        if drop > entry && drop < entry + 32.0 * beat {
            drop
        } else {
            entry
        }
    } else {
        blend_cue
    };
    // The incoming's intro under the outgoing: its level by the throw, and
    // how much of its top is held back. Two grids that do not agree tick
    // against each other in the hats, so those stay soft until the landing.
    let intro_db = if style == TransitionStyle::DropCut {
        DROP_CUT_INTRO_DB
    } else if key_clash {
        EFFECT_INTRO_KEY_CLASH_DB
    } else {
        EFFECT_INTRO_DB
    };
    let grids_clash = !grid || !incoming_grid || tempo_gap > MAX_STRETCH_DEVIATION;
    let intro_band = if grids_clash { 1.0 } else { 0.4 };
    Some(TransitionPlan {
        style,
        bass_swap: false,
        bass_swap_fraction: DEFAULT_BASS_SWAP_FRACTION,
        filter_sweep: intro_band,
        vocal_overlap: 0.0,
        fade_seconds: blend_fade,
        // The outgoing stays at unity, the next record rising underneath it,
        // until the technique takes it out: an echo thrown, a beat rolled or
        // a wash sent from a record already faded most of the way down was
        // 20–40 dB under the mix.
        outgoing_hold_seconds: (blend_fade - effect_seconds).max(0.0),
        bridge_start_seconds: 0.0,
        bridge_seconds: 0.0,
        bridge_repeats: 0,
        effect_seconds,
        effect_start_seconds: (end - effect_seconds).max(0.0),
        transition_end_seconds: end,
        cue_seconds: cue,
        playback_rate: 1.0,
        bed_fraction: 0.0,
        // On an effect handover this is the intro level, not a DJ bed (a
        // bed needs `bed_fraction`): the mixer brings the incoming to it
        // across the hold.
        bed_gain_db: intro_db,
        dip_depth: 0.0,
        dip_width: 0.0,
        post_glide_seconds: 0.0,
        outgoing_duration_seconds: outgoing.duration,
        beat_seconds: beat,
        reason: format!(
            "{why} (clash; {blend_fade:.1}s blend with the next record in at {intro_db:.0} dB, \
             {effect_seconds:.1}s effect; {}, grid {:.2})",
            tempo_summary(out_bpm, in_bpm),
            outgoing.beat_confidence
        ),
    })
}

fn long_blend_allowed(outgoing: &Analysis, incoming: &Analysis) -> bool {
    assess(outgoing, incoming) == Tier::Beatmatched
}

fn assess(outgoing: &Analysis, incoming: &Analysis) -> Tier {
    if !(MIN_BPM..=MAX_BPM).contains(&outgoing.bpm) || !(MIN_BPM..=MAX_BPM).contains(&incoming.bpm)
    {
        return Tier::Plain;
    }
    if outgoing.beat_confidence < MIN_DJ_CONFIDENCE && incoming.beat_confidence < MIN_DJ_CONFIDENCE
    {
        return Tier::Plain;
    }
    let stretch = outgoing.bpm / align_tempo_octave(outgoing.bpm, incoming.bpm).max(1e-6);
    let far = (stretch - 1.0).abs() > MAX_STRETCH_DEVIATION;
    let weak = outgoing.beat_confidence < MIN_BEATMATCH_CONFIDENCE
        || incoming.beat_confidence < MIN_BEATMATCH_CONFIDENCE;
    if far || weak {
        Tier::DjAssisted
    } else {
        Tier::Beatmatched
    }
}

fn align_tempo_octave(outgoing_bpm: f64, incoming_bpm: f64) -> f64 {
    if outgoing_bpm <= 0.0 || incoming_bpm <= 0.0 {
        return incoming_bpm;
    }
    let mut aligned = incoming_bpm;
    while aligned / outgoing_bpm > 1.5 {
        aligned /= 2.0;
    }
    while aligned / outgoing_bpm < 0.67 {
        aligned *= 2.0;
    }
    aligned
}

/// Rate that brings the incoming tempo onto the outgoing one, or `None` when
/// the stretch would be audible. Octave-aligned first.
fn transparent_playback_rate(
    outgoing_bpm: f64,
    incoming_bpm: f64,
    max_deviation: f64,
) -> Option<f64> {
    if outgoing_bpm <= 0.0 || incoming_bpm <= 0.0 || max_deviation < 0.0 {
        return None;
    }
    let aligned = align_tempo_octave(outgoing_bpm, incoming_bpm);
    let rate = outgoing_bpm / aligned;
    if (rate - 1.0).abs() <= max_deviation {
        Some(((rate * 10_000.0).round()) / 10_000.0)
    } else {
        None
    }
}

/// Mixxx-style fade cap: min(outro length, intro length) when both sections
/// look real. `None` means "no useful section lengths — leave the beat math".
fn section_fade_cap(outgoing: &Analysis, incoming: &Analysis) -> Option<f64> {
    let content_end = if outgoing.content_end > 0.0 {
        outgoing.content_end
    } else {
        outgoing.duration
    };
    let outro_len = if outgoing.outro_start > 0.0 && content_end > outgoing.outro_start + 1.0 {
        content_end - outgoing.outro_start
    } else {
        return None;
    };
    let audible = if incoming.audible_start_time.is_finite() && incoming.audible_start_time >= 0.0 {
        incoming.audible_start_time
    } else {
        0.0
    };
    let intro_end = if incoming.mix_in_time > audible + 1.0 {
        incoming.mix_in_time
    } else {
        return None;
    };
    let intro_len = intro_end - audible;
    if outro_len < 2.0 || intro_len < 2.0 {
        return None;
    }
    Some(outro_len.min(intro_len))
}

fn normalized_tempo_ratio(current: f64, next: f64) -> f64 {
    if current <= 0.0 || next <= 0.0 {
        return 1.0;
    }
    let mut ratio = next / current;
    while ratio > 1.5 {
        ratio /= 2.0;
    }
    while ratio < 0.67 {
        ratio *= 2.0;
    }
    ratio
}

/// The tempo relationship a plan was decided from, in words a log can carry.
///
/// A beat tracker reads an octave out often enough that the planner already
/// compares tempos folded — 198 against 99 is the same beat at double time,
/// not a clash — but the reason string printed the raw numbers, so the log read
/// like one (`tempo 198→99 BPM, clash`). The fold is printed with the gap it
/// produced, which is the number [`effect_earned`] actually decided on.
fn tempo_summary(out_bpm: f64, in_bpm: f64) -> String {
    if out_bpm <= 0.0 || in_bpm <= 0.0 {
        return "tempo unknown".into();
    }
    let ratio = normalized_tempo_ratio(out_bpm, in_bpm);
    // `normalized_tempo_ratio` folds `in/out` by a power of two, so this is the
    // octave multiplier it applied to the incoming tempo.
    let fold = ratio * out_bpm / in_bpm;
    let mut summary = format!("tempo {out_bpm:.0}→{in_bpm:.0} BPM");
    if (fold - 1.0).abs() > 0.01 {
        summary.push_str(&format!(" ({fold}× to align)"));
    }
    summary.push_str(&format!(", gap {:.0}%", (1.0 - ratio).abs() * 100.0));
    summary
}

/// Where a record's audible content ends (trailing silence removed).
fn content_end_of(analysis: &Analysis) -> f64 {
    if analysis.content_end > 0.0 {
        analysis.content_end
    } else {
        analysis.duration
    }
}

/// The last point the record is still at its loud level: the final chord's
/// attack, or the last beat before the fade begins, depending on the ending.
fn last_full_energy(analysis: &Analysis) -> Option<f64> {
    let reference = loud_reference(analysis)?;
    let end = content_end_of(analysis);
    analysis
        .energy_curve
        .iter()
        .filter(|point| point.time.is_finite() && point.time <= end + 1e-3)
        .filter(|point| point.energy >= reference * 0.707)
        .map(|point| point.time)
        .next_back()
}

/// The attack of a final chord, if the record ends on one rather than fading.
///
/// Two shapes qualify: an abrupt end (the last loud point is within two
/// seconds of the content end), and a final chord whose attack is a clear rise
/// in the last twelve seconds. A fade-out is monotone and has no such rise, so
/// it is left to the normal blend. The heuristic is deliberately conservative:
/// it is better to miss a chord (the blend is merely longer) than to move every
/// ending.
fn final_chord_attack(analysis: &Analysis) -> Option<f64> {
    let reference = loud_reference(analysis)?;
    let end = content_end_of(analysis);
    if end <= 0.0 {
        return None;
    }
    let last_loud = last_full_energy(analysis)?;
    if end - last_loud <= 2.0 {
        return Some(last_loud);
    }
    let earliest = (end - 12.0).max(0.0);
    let mut time = end - 0.5;
    while time > earliest {
        if let (Some(after), Some(before)) = (
            energy_between(analysis, time, time + 0.25),
            energy_between(analysis, time - 0.5, time - 0.05),
        ) {
            if after >= reference * 0.4 && after > before * 1.6 {
                return Some(time);
            }
        }
        time -= 0.1;
    }
    None
}

/// The first sustained vocal entry at or after `from`, in the mask's own
/// resolution. Two active frames are required so one model blip cannot drag a
/// cue back.
fn first_vocal_onset(analysis: &Analysis, from: f64) -> Option<f64> {
    let mask = &analysis.vocal_activity_mask;
    if mask.is_empty() || mask.len() != analysis.energy_curve.len() {
        return None;
    }
    let mut run = 0usize;
    let mut run_start = 0.0;
    for (index, value) in mask.iter().enumerate() {
        let time = analysis.energy_curve[index].time;
        if !time.is_finite() || time < from {
            run = 0;
            continue;
        }
        if value.is_finite() && *value >= VOCAL_ACTIVE_THRESHOLD {
            if run == 0 {
                run_start = time;
            }
            run += 1;
            if run >= 2 {
                return Some(run_start);
            }
        } else {
            run = 0;
        }
    }
    None
}

/// E: respect the music at both ends of the handover.
///
/// On the outgoing side a final chord keeps its attack at unity: the mix does
/// not begin by fading it. The incoming enters a phrase before the chord and
/// rises underneath it; the outgoing stays at unity through the attack and
/// crosses over across the decay. The blend is the phrase plus the decay, so
/// the overlap is never shorter than the phrase floor even when the chord is
/// only a beat before the record's end.
///
/// On the incoming side a vocal entry or hook is never skipped: a cue past the
/// first sustained vocal is pulled back to it. A gapless album join is
/// untouched, and DropCut keeps its deliberate short swap.
fn respect_music_edges(outgoing: &Analysis, incoming: &Analysis, plan: &mut TransitionPlan) {
    if plan.style == TransitionStyle::Gapless {
        return;
    }
    let entry = incoming_entry(incoming);
    if let Some(onset) = first_vocal_onset(incoming, entry) {
        if plan.cue_seconds > onset + 0.05 {
            plan.cue_seconds = onset.max(entry);
        }
    }
    let end = content_end_of(outgoing);
    // Only the record's actual ending is protected. A plan that mixes out at
    // an interior cliff is deliberately leaving before the ending, so moving
    // it to the file's last chord would undo the analysis the plan is based on.
    if (plan.transition_end_seconds - end).abs() > 1.5 {
        return;
    }
    let Some(chord) = final_chord_attack(outgoing) else {
        return;
    };
    if plan.transition_end_seconds < end {
        plan.transition_end_seconds = end;
    }
    let decay = (end - chord).max(0.0);
    if plan.style == TransitionStyle::DropCut {
        // A drop swap is a deliberate short cut. The chord still may not be
        // faded through, so its effect window stays inside the decay; the
        // phrase floor does not apply to a style whose identity is the cut.
        if plan.transition_end_seconds - plan.fade_seconds + 0.15 < chord {
            plan.fade_seconds = decay.max(0.1);
            plan.effect_seconds = plan.effect_seconds.min(plan.fade_seconds);
            plan.effect_start_seconds = (plan.transition_end_seconds - plan.effect_seconds).max(0.0);
        }
        return;
    }

    // The phrase is the planner's own length for the pair: eight beats of the
    // outgoing record, or the plan's analysed fall when there is no grid. The
    // incoming's entry moves one phrase before the chord, which is what makes
    // the overlap phrase + decay rather than the bare decay E used to leave.
    let beat = beat_seconds_of(outgoing);
    let phrase = if beat > 0.0 {
        (8.0 * beat).clamp(2.0, AUTO_TRANSITION_MAX_SECONDS)
    } else {
        plan.fade_seconds.clamp(2.0, AUTO_TRANSITION_MAX_SECONDS)
    };
    let fade = (phrase + decay).clamp(2.0, AUTO_TRANSITION_MAX_SECONDS);
    plan.fade_seconds = fade;
    plan.outgoing_hold_seconds = (fade - decay).clamp(0.0, (fade - 0.05).max(0.0));
    // The nerd stats have to describe the blend that actually plays. The
    // planner's own reason predates the hold and quotes the pre-E-2 length, so
    // a cold ending replaces it (or leads with the new shape for a style whose
    // reason carries the technique's rationale).
    let note = format!(
        "cold ending: the final chord at {chord:.1}s is struck at unity, and the incoming \
         (entered {phrase:.1}s earlier) takes over across its {decay:.1}s decay \
         ({fade:.1}s blend, {:.1}s hold)",
        plan.outgoing_hold_seconds
    );
    plan.reason = if plan.reason.starts_with("crossfade:") || plan.reason.starts_with("filter fade:") {
        note
    } else {
        format!("{note}; {}", plan.reason)
    };
    // The technique dresses the ending, never the held phrase: its window
    // stays inside the decay, so the chord is still at unity when it starts.
    if plan.effect_seconds > 0.0 {
        plan.effect_seconds = plan.effect_seconds.min(decay.max(0.1)).min(fade);
        plan.effect_start_seconds = (plan.transition_end_seconds - plan.effect_seconds).max(0.0);
    }
}

/// F's bridge decision for one clean crossfade.
#[derive(Clone, Copy, Debug)]
struct BridgeUnit {
    start: f64,
    seconds: f64,
    bars: usize,
}

/// The peak of the vocal mask in `[start, end]`, beside the mean
/// [`vocal_activity_between`] returns. The UMX mask has a low instrumental
/// floor (about 0.28 on the corpus), so "no voice" is a low peak as well as a
/// low mean.
fn vocal_peak_between(analysis: &Analysis, start: f64, end: f64) -> Option<f64> {
    let mask = &analysis.vocal_activity_mask;
    let curve = &analysis.energy_curve;
    if mask.is_empty() || mask.len() != curve.len() || end <= start {
        return None;
    }
    let mut peak = 0.0f64;
    let mut seen = false;
    for (index, value) in mask.iter().enumerate() {
        let time = curve[index].time;
        if !time.is_finite() || time < start || time > end || !value.is_finite() {
            continue;
        }
        peak = peak.max(*value);
        seen = true;
    }
    seen.then_some(peak)
}

/// The "no voice" bar for an outro window: mean and peak. The model's
/// instrumental floor is about 0.28, so this admits an instrumental outro and
/// rejects a sung one. F's bridge and L1's phrase extension read the same bar.
const VOCAL_FREE_MEAN_MAX: f64 = 0.35;
const VOCAL_FREE_PEAK_MAX: f64 = 0.55;

/// F-3: the chosen bar grid only has to be *usable*: enough bars, a median
/// spacing in a musical range. The 15 ms test the instruction names is per
/// **unit** — the loop's own steps are measured against the grid in
/// [`bridge_unit`] — because the PLL's bars drift by 50-90 ms over a whole
/// track while staying locally regular, which is exactly what a two-bar loop
/// can use.
fn bar_grid_usable(analysis: &Analysis) -> bool {
    let bars = &analysis.downbeats;
    if bars.len() < 5 {
        return false;
    }
    let mut gaps: Vec<f64> = bars
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|g| g.is_finite() && *g > 0.0)
        .collect();
    if gaps.len() < 4 {
        return false;
    }
    gaps.sort_by(|a, b| a.total_cmp(b));
    (0.4..=8.0).contains(&gaps[gaps.len() / 2])
}

/// F: the unit to loop, or why there is none.
///
/// Only a clean crossfade is eligible. The unit is a whole number of bars
/// (two, or four on a record at 140 BPM or faster when the cap allows), starts
/// on a phrase boundary, ends on a downbeat at or before the record's last
/// chord (or its content end), is measured from the downbeats themselves, and
/// is vocal-free. Nothing here is tuned to the corpus: the thresholds are the
/// model's own floor and the 15 ms grid tolerance.
fn bridge_unit(outgoing: &Analysis, plan: &TransitionPlan) -> Result<BridgeUnit, &'static str> {
    if !(plan.reason.starts_with("crossfade:")
        || plan.reason.starts_with("filter fade:")
        || plan.reason.starts_with("cold ending:"))
    {
        return Err("not a clean crossfade");
    }
    if beat_seconds_of(outgoing) <= 0.0 || !bar_grid_usable(outgoing) {
        return Err("no regular bar grid");
    }
    let start = plan.transition_end_seconds - plan.fade_seconds;
    let chord = final_chord_attack(outgoing);
    // E-2's protected chord is the unit's ceiling. Without a hold a chord may
    // still sit after the handover (an interior mix-out discards it); it only
    // rules a unit out when it falls *inside* that unit, checked below.
    let point = if plan.outgoing_hold_seconds > 0.0 {
        chord
            .map(|c| c.min(plan.transition_end_seconds))
            .unwrap_or(plan.transition_end_seconds)
    } else {
        plan.transition_end_seconds
    };
    if point <= start + 0.5 || outgoing.downbeats.len() < 5 {
        return Err("the blend is shorter than one unit");
    }
    let beat = beat_seconds_of(outgoing);
    let fast = fold_tempo(outgoing.bpm) > 140.0;
    let orders: &[usize] = if fast { &[4, 2] } else { &[2] };
    let mut failure: &'static str = "no phrase-aligned unit after the blend start";
    for &bars in orders {
        let ideal = 4.0 * bars as f64 * beat;
        if plan.fade_seconds + ideal > MAX_BRIDGED_SECONDS {
            failure = "the unit would push the fade past 16s";
            continue;
        }
        // The last eligible unit, closest to the record's ending.
        for j in (0..outgoing.downbeats.len()).rev() {
            let end = outgoing.downbeats[j];
            if !end.is_finite() || end > point + 1e-6 || j < bars {
                continue;
            }
            let i = j - bars;
            // A phrase boundary: bar 0, 2, 4… from the first downbeat.
            if i % 2 != 0 {
                continue;
            }
            let begin = outgoing.downbeats[i];
            // The unit has to lie inside the blend. A 10 ms numerical
            // allowance, inside the 15 ms grid tolerance, covers the
            // difference between the planner's phrase and the measured span.
            if !begin.is_finite() || begin < start - 0.010 {
                continue;
            }
            let mut grid_ok = true;
            for k in 0..=bars {
                let expected = begin + k as f64 * 4.0 * beat;
                if (outgoing.downbeats[i + k] - expected).abs() > 0.015 {
                    grid_ok = false;
                    break;
                }
            }
            if !grid_ok {
                failure = "the unit's downbeats drift more than 15 ms";
                continue;
            }
            if chord.is_some_and(|c| c > begin + 0.05 && c < end - 0.05) {
                failure = "a final chord sits in the unit";
                continue;
            }
            let measured = end - begin;
            if measured <= 0.5 {
                continue;
            }
            if plan.fade_seconds + measured > MAX_BRIDGED_SECONDS {
                failure = "the unit would push the fade past 16s";
                continue;
            }
            let Some(mean) = vocal_activity_between(outgoing, begin, end) else {
                continue;
            };
            let peak = vocal_peak_between(outgoing, begin, end).unwrap_or(1.0);
            if mean > VOCAL_FREE_MEAN_MAX || peak > VOCAL_FREE_PEAK_MAX {
                failure = "the unit has a voice";
                continue;
            }
            return Ok(BridgeUnit {
                start: begin,
                seconds: measured,
                bars,
            });
        }
    }
    Err(failure)
}

/// Extends a clean crossfade with one replay of a measured, vocal-free unit of
/// the outgoing's outro. The record's end (and a cold ending's hold) move by
/// the unit; the blend start does not move, so the incoming enters where it did
/// and the outgoing has one more unit of music to cross under it.
fn apply_bridge(outgoing: &Analysis, plan: &mut TransitionPlan) {
    // The clean crossfade is a filter fade now; the bridge extends either.
    if !matches!(plan.style, TransitionStyle::EqualPower | TransitionStyle::DjFilter)
        || plan.effect_seconds > 0.0
        || plan.bass_swap
    {
        return;
    }
    match bridge_unit(outgoing, plan) {
        Ok(unit) => {
            plan.transition_end_seconds += unit.seconds;
            plan.fade_seconds += unit.seconds;
            // A cold ending's hold covers the unit; an ordinary crossfade has
            // no hold and its fall starts where it did.
            if plan.outgoing_hold_seconds > 0.0 {
                plan.outgoing_hold_seconds += unit.seconds;
            }
            plan.bridge_start_seconds = unit.start;
            plan.bridge_seconds = unit.seconds;
            plan.bridge_repeats = 1;
            plan.reason = format!(
                "{}; bridge: {} bars ({:.1}s) from {:.1}s, replayed once",
                plan.reason, unit.bars, unit.seconds, unit.start
            );
        }
        Err(why) => {
            plan.reason = format!("{}; bridge: none ({why})", plan.reason);
        }
    }
}

/// The E scorecard numbers for the most recently planned transition.
#[derive(Clone, Copy, Debug, Default)]
pub struct TransitionEdges {
    pub outgoing_discarded_seconds: f64,
    pub incoming_discarded_seconds: f64,
    pub cold_ending: bool,
    pub blend_start_seconds: f64,
    pub final_chord_seconds: Option<f64>,
    pub clipped_final_chord: bool,
}

pub fn last_transition_edges() -> TransitionEdges {
    transition_edges::get()
}

mod transition_edges {
    use super::TransitionEdges;
    use std::sync::{Mutex, OnceLock};

    fn stored() -> &'static Mutex<TransitionEdges> {
        static EDGES: OnceLock<Mutex<TransitionEdges>> = OnceLock::new();
        EDGES.get_or_init(|| Mutex::new(TransitionEdges::default()))
    }

    pub(super) fn set(edges: TransitionEdges) {
        *stored().lock().unwrap_or_else(|p| p.into_inner()) = edges;
    }

    pub(super) fn get() -> TransitionEdges {
        *stored().lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn record_transition_edges(outgoing: &Analysis, incoming: &Analysis, plan: &TransitionPlan) {
    let end = content_end_of(outgoing);
    let start = plan.transition_end_seconds - plan.fade_seconds;
    let chord = final_chord_attack(outgoing);
    // A chord is only *clipped* when this plan hands over at the record's own
    // ending and begins before the chord. An interior mix-out leaves the
    // ending unplayed on purpose; that is the discarded-music figure, not a
    // fade cutting the chord.
    let ending_handover = (plan.transition_end_seconds - end).abs() <= 1.5;
    transition_edges::set(TransitionEdges {
        outgoing_discarded_seconds: (end - plan.transition_end_seconds).max(0.0),
        incoming_discarded_seconds: (plan.cue_seconds - incoming.audible_start_time.max(0.0))
            .max(0.0),
        cold_ending: chord.is_some(),
        blend_start_seconds: start,
        final_chord_seconds: chord,
        clipped_final_chord: ending_handover
            && chord.is_some_and(|attack| {
                start + plan.outgoing_hold_seconds + 0.15 < attack
            }),
    });
}

/// Where playback of the incoming track starts: the earliest point it makes
/// sound, snapped to a downbeat, and bounded so a mix-in cannot skip the front
/// of the record.
///
/// This is not the drop. The drop is [`incoming_handoff`]; playback begins
/// `overlap` before that, and [`cue_for`] keeps the result within eight
/// seconds of this entry.
fn incoming_entry(analysis: &Analysis) -> f64 {
    let duration = analysis.duration;
    let beat_seconds = beat_seconds_of(analysis);
    let grid = beat_seconds > 0.0
        && analysis
            .downbeats
            .iter()
            .any(|d| d.is_finite() && *d >= 0.0);
    if !grid {
        // No grid, no skip. Without a beat to align to there is nothing to say
        // how deep a mix-in should reach, and the honest answer is the top of
        // the file: a missing analysis degrades to "play it from the start",
        // never to "start at a drop we guessed at".
        return 0.0;
    }

    // The earliest music, from the phase-1 candidates (a single `pickup` at the
    // pickup point) with the envelope's own measurement as the backstop.
    let entry = analysis
        .mix_in_candidates
        .iter()
        .filter(|c| c.time.is_finite() && c.time >= 0.0)
        .map(|c| c.time)
        .chain([analysis.audible_start_time].into_iter())
        .fold(f64::INFINITY, f64::min);
    let entry = if entry.is_finite() { entry } else { 0.0 };

    // Snap forward onto the grid: entering between beats is the one thing a
    // listener hears as "edited" rather than "mixed". Bounded to one beat past
    // the pickup, so a grid that starts late cannot drag the entry with it.
    let snapped = analysis
        .downbeats
        .iter()
        .copied()
        .filter(|d| d.is_finite() && *d >= entry - 1e-3 && *d <= entry + beat_seconds)
        .min_by(|a, b| a.total_cmp(b))
        .unwrap_or(entry);

    cap_cue(snapped, entry, beat_seconds, duration)
}

/// The tests still talk about the cue. It is the entry.
#[cfg(test)]
fn incoming_cue(analysis: &Analysis) -> f64 {
    incoming_entry(analysis)
}

/// A track's beat length, or 0 when there is no usable tempo. A BPM outside
/// the analyser's own range means the read failed, and a failed read must not
/// become a 32-beat cue.
fn beat_seconds_of(analysis: &Analysis) -> f64 {
    if analysis.bar_beat_seconds.is_finite() && analysis.bar_beat_seconds > 0.0 {
        return analysis.bar_beat_seconds;
    }
    if !(MIN_BPM..=MAX_BPM).contains(&analysis.bpm) {
        return 0.0;
    }
    let from_bpm = 60.0 / analysis.bpm;
    if analysis.beat_interval.is_finite() && analysis.beat_interval > 0.0 {
        // Trust the grid's own interval when the two agree; the two can differ
        // by an octave after `align_tempo_octave`, and the grid is what the
        // downbeats are actually spaced at.
        if (analysis.beat_interval - from_bpm).abs() / from_bpm < 0.5 {
            return analysis.beat_interval;
        }
    }
    from_bpm
}

/// The three bounds on a mix-in. Together they mean a listener never loses the
/// front of a record to a transition, however long it is or however badly the
/// analysis read.
fn cap_cue(cue: f64, entry: f64, beat_seconds: f64, duration: f64) -> f64 {
    let in_mix_in_window = |t: f64| {
        t.is_finite() && t >= 0.0 && (duration <= 0.0 || t < duration - MIX_IN_END_MARGIN_SECONDS)
    };
    let cue = if in_mix_in_window(cue) { cue } else { 0.0 };
    let mut cap = MAX_CUE_SECONDS.min(MAX_CUE_BEATS * beat_seconds);
    if duration > 0.0 {
        // On a long set or a DJ track, a flat 8 s cap still throws away music;
        // a fraction of the track keeps the skip proportional to the track.
        cap = cap.min(duration * MAX_CUE_FRACTION);
    }
    if cue <= cap {
        return cue;
    }
    // Over the cap, fall back to the pickup — which is the top of the music, and
    // what the track would have played from anyway.
    if entry <= cap && in_mix_in_window(entry) {
        entry
    } else {
        0.0
    }
}

/// Where the incoming arrangement arrives — the drop — if the overlap can
/// cover the distance from the entry. Otherwise the handoff is the entry plus
/// that overlap, which is a long intro played underneath, not a skip.
fn incoming_handoff(analysis: &Analysis, max_overlap_s: f64) -> f64 {
    let entry = incoming_entry(analysis);
    let budget = max_overlap_s.max(0.0);
    let beat = beat_seconds_of(analysis);
    let mut ranked: Vec<&audio_analysis::MixCuePoint> = analysis
        .mix_in_candidates
        .iter()
        .filter(|c| c.kind != "pickup" && c.time.is_finite() && c.time > entry + 0.25)
        .collect();
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
    if let Some(drop) = ranked.first() {
        let snapped = if beat > 0.0 {
            nearest_value(&analysis.downbeats, drop.time, 0.5_f64.max(beat * 2.0))
                .unwrap_or(drop.time)
        } else {
            drop.time
        };
        if snapped >= entry && snapped - entry <= budget + 0.05 {
            return snapped;
        }
    }
    if analysis.mix_in_time.is_finite()
        && analysis.mix_in_time > entry + 0.25
        && analysis.mix_in_time - entry <= budget + 0.05
    {
        return analysis.mix_in_time;
    }
    entry + budget
}

/// Playback starts `overlap * rate` before the handoff, and never more than
/// eight seconds past the entry.
fn cue_for(entry: f64, handoff: f64, overlap: f64, rate: f64) -> f64 {
    let rate = if rate.is_finite() && rate > 0.0 {
        rate
    } else {
        1.0
    };
    let raw = if overlap.is_finite() {
        handoff - overlap * rate
    } else {
        entry
    };
    raw.max(entry).max(0.0).min(entry + MAX_CUE_SECONDS)
}

fn timed_near_or_before(values: &[f64], target: f64, tolerance: f64, minimum: f64) -> Option<f64> {
    values
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v >= minimum && *v <= target && target - *v <= tolerance)
        .max_by(|a, b| a.total_cmp(b))
}

fn nearest_timed(values: &[f64], target: f64, tolerance: f64, minimum: f64) -> Option<f64> {
    values
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v >= minimum && (*v - target).abs() <= tolerance)
        .min_by(|a, b| (*a - target).abs().total_cmp(&(*b - target).abs()))
}

/// Snaps a transition start onto the outgoing grid: a phrase boundary if one
/// is near, a downbeat otherwise (upstream `alignedTransitionStart`).
fn aligned_transition_start(
    analysis: &Analysis,
    target: f64,
    end: f64,
    prefer_earlier: bool,
    minimum: f64,
) -> f64 {
    let interval = beat_seconds_of(analysis);
    let phrase_tolerance = 1.0_f64.max(interval * 4.0);
    let downbeat_tolerance = 0.75_f64.max(interval * 2.0);
    let phrase = if prefer_earlier {
        timed_near_or_before(
            &analysis.phrase_boundaries,
            target,
            phrase_tolerance,
            minimum,
        )
    } else {
        nearest_timed(
            &analysis.phrase_boundaries,
            target,
            phrase_tolerance,
            minimum,
        )
    };
    let downbeat = if prefer_earlier {
        timed_near_or_before(&analysis.downbeats, target, downbeat_tolerance, minimum)
    } else {
        nearest_timed(&analysis.downbeats, target, downbeat_tolerance, minimum)
    };
    let chosen = phrase.or(downbeat).unwrap_or(target);
    if end < minimum {
        return chosen.max(0.0).min(end.max(0.0));
    }
    chosen.clamp(minimum, end)
}

/// The blend that actually plays, after the grid snap.
///
/// [`aligned_transition_start`] clamps onto `mix_end - 0.05`. When the only
/// boundary in range is the end itself, `mix_end - start` collapses, and a
/// floor of half a second rendered that collapse as a cut. Upstream's floor
/// is four seconds (six on a fast track). A snap that kept the blend is left
/// alone; a snap that ate it is discarded and the planned length is restored.
/// A tail shorter than the floor uses the tail — there is nothing else to
/// blend across.
fn realized_window(mix_end: f64, snapped_start: f64, desired: f64, minimum: f64) -> (f64, f64) {
    let mix_end = mix_end.max(0.0);
    let snapped = (mix_end - snapped_start).max(0.0);
    let minimum = minimum.min(mix_end);
    let fade = if snapped + 0.25 >= minimum {
        snapped.max(minimum)
    } else {
        desired.max(minimum)
    };
    let fade = fade.min(mix_end);
    ((mix_end - fade).max(0.0), fade)
}

/// Mean vocal activity over `start`..`end` on a track's own timeline (upstream
/// `vocalActivityBetween`). Null when the mask is absent or the window is empty.
fn vocal_activity_between(analysis: &Analysis, start: f64, end: f64) -> Option<f64> {
    let mask = &analysis.vocal_activity_mask;
    let curve = &analysis.energy_curve;
    if mask.is_empty() || mask.len() != curve.len() || end <= start {
        return None;
    }
    let mut sum = 0.0;
    let mut count = 0usize;
    for index in 0..mask.len() {
        let time = curve[index].time;
        if !time.is_finite() || time < start || time > end {
            continue;
        }
        let value = mask[index];
        if !value.is_finite() {
            continue;
        }
        sum += value;
        count += 1;
    }
    if count > 0 {
        Some(sum / count as f64)
    } else {
        None
    }
}

/// Both windows measurably singing at once (upstream `isVocalClash`).
fn is_vocal_clash(outgoing_activity: Option<f64>, incoming_activity: Option<f64>) -> bool {
    matches!((outgoing_activity, incoming_activity),
        (Some(out), Some(inc)) if out >= VOCAL_ACTIVE_THRESHOLD && inc >= VOCAL_ACTIVE_THRESHOLD)
}

/// Do both records sing through the handover itself?
///
/// The two windows are the outgoing's last two bars before its mix-out anchor
/// and the incoming's first two bars from its cue — the audio that will
/// actually be heard together. A record that ends on an instrumental outro
/// reads 0.28 here while its whole-track probability said 0.85, which is why
/// the whole-track number could not be the trigger.
fn handover_vocal_clash(
    outgoing: &Analysis,
    incoming: &Analysis,
    mix_out_anchor: f64,
    incoming_cue: f64,
) -> bool {
    let out_beats = beat_seconds_of(outgoing).max(0.25);
    let in_beats = beat_seconds_of(incoming).max(0.25);
    let out_end = if mix_out_anchor > 0.0 {
        mix_out_anchor
    } else if outgoing.content_end > 0.0 {
        outgoing.content_end
    } else {
        outgoing.duration
    };
    let in_start = incoming_cue.max(0.0);
    let out_tail = vocal_activity_between(outgoing, out_end - 8.0 * out_beats, out_end);
    let in_head = vocal_activity_between(incoming, in_start, in_start + 8.0 * in_beats);
    is_vocal_clash(out_tail, in_head)
}

/// The fraction of a planned overlap where **both** tracks sing at the same
/// instant (upstream `simultaneousVocalFraction`), mapping the outgoing
/// energy-curve samples onto the incoming timeline through `rate`.
fn simultaneous_vocal_fraction(
    outgoing: &Analysis,
    incoming: &Analysis,
    out_start: f64,
    out_end: f64,
    in_start: f64,
    rate: f64,
) -> Option<f64> {
    let out_mask = &outgoing.vocal_activity_mask;
    let out_curve = &outgoing.energy_curve;
    let in_mask = &incoming.vocal_activity_mask;
    let in_curve = &incoming.energy_curve;
    if out_mask.is_empty() || out_mask.len() != out_curve.len() {
        return None;
    }
    if in_mask.is_empty() || in_mask.len() != in_curve.len() {
        return None;
    }
    if out_end <= out_start {
        return None;
    }
    let step = if rate.is_finite() && rate > 0.0 {
        rate
    } else {
        1.0
    };
    let mut in_index = 0usize;
    let mut both = 0usize;
    let mut total = 0usize;
    for index in 0..out_mask.len() {
        let time = out_curve[index].time;
        if !time.is_finite() || time < out_start {
            continue;
        }
        if time > out_end {
            break;
        }
        total += 1;
        if out_mask[index] < VOCAL_ACTIVE_THRESHOLD {
            continue;
        }
        let target = in_start + (time - out_start) * step;
        while in_index + 1 < in_curve.len() && in_curve[in_index + 1].time <= target {
            in_index += 1;
        }
        if in_index < in_mask.len() && in_mask[in_index] >= VOCAL_ACTIVE_THRESHOLD {
            both += 1;
        }
    }
    if total > 0 {
        Some(both as f64 / total as f64)
    } else {
        None
    }
}

/// The render-time vocal separation amount, measured instant-by-instant over the
/// windows the plan actually blends (upstream `plannedVocalOverlap`).
fn planned_vocal_overlap(
    outgoing: &Analysis,
    incoming: &Analysis,
    transition_start: f64,
    transition_end: f64,
    incoming_cue_time: f64,
    incoming_playback_rate: f64,
) -> f64 {
    let outgoing_span = transition_end - transition_start;
    if outgoing_span <= 0.0 || !outgoing_span.is_finite() {
        return 0.0;
    }
    let rate = if incoming_playback_rate.is_finite() && incoming_playback_rate > 0.0 {
        incoming_playback_rate
    } else {
        1.0
    };
    simultaneous_vocal_fraction(
        outgoing,
        incoming,
        transition_start,
        transition_end,
        incoming_cue_time,
        rate,
    )
    .unwrap_or(0.0)
}

// ---- WSOLA beat-matched phrase-switch (upstream `planWsolaTransition`) ------

fn nearest_at_or_before(values: &[f64], target: f64) -> Option<f64> {
    values
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v >= 0.0 && *v <= target)
        .max_by(|a, b| a.total_cmp(b))
}

fn nearest_value(values: &[f64], target: f64, tolerance: f64) -> Option<f64> {
    values
        .iter()
        .copied()
        .filter(|v| v.is_finite() && (*v - target).abs() <= tolerance)
        .min_by(|a, b| (a - target).abs().total_cmp(&(b - target).abs()))
}

/// The earliest point the analysis claims the track makes sound (upstream
/// `audibleStartOf`).
fn audible_start_of(analysis: &Analysis) -> f64 {
    let mut best = f64::INFINITY;
    for value in [analysis.audible_start_time, analysis.first_beat] {
        if value.is_finite() && value >= 0.0 {
            best = best.min(value);
        }
    }
    if best.is_finite() {
        best
    } else {
        0.0
    }
}

/// One pass of the vocal-clash gate (upstream `clashOver`).
#[allow(clippy::too_many_arguments)]
fn clash_over(
    outgoing: &Analysis,
    incoming: &Analysis,
    overlap_end_target: f64,
    outgoing_beat_seconds: f64,
    incoming_beat_seconds: f64,
    incoming_drop_time: f64,
    audible_start: f64,
    stretch_ratio: f64,
    beats: usize,
) -> bool {
    let out_start = overlap_end_target - beats as f64 * outgoing_beat_seconds;
    let in_start = audible_start.max(incoming_drop_time - beats as f64 * incoming_beat_seconds);
    let out_vocal = vocal_activity_between(outgoing, out_start, overlap_end_target);
    let in_vocal = vocal_activity_between(incoming, in_start, incoming_drop_time);
    let simultaneous = simultaneous_vocal_fraction(
        outgoing,
        incoming,
        out_start,
        overlap_end_target,
        in_start,
        stretch_ratio,
    );
    if simultaneous.is_some_and(|s| s > VOCAL_CLASH_TOLERANCE) {
        return true;
    }
    if is_vocal_clash(out_vocal, in_vocal) {
        return true;
    }
    if beats > 8 && out_vocal.is_some_and(|v| v >= VOCAL_ACTIVE_THRESHOLD) {
        let deep_vocal = vocal_activity_between(
            outgoing,
            out_start,
            overlap_end_target - 8.0 * outgoing_beat_seconds,
        );
        if deep_vocal.is_some_and(|v| v >= VOCAL_ACTIVE_THRESHOLD) {
            return true;
        }
    }
    false
}

/// Plans one beat-matched transition (upstream `planWsolaTransition`). `None`
/// is a routing decision — the caller falls back to the adaptive overlap.
fn plan_wsola_transition(outgoing: &Analysis, incoming: &Analysis) -> Option<TransitionPlan> {
    if assess(outgoing, incoming) != Tier::Beatmatched {
        return None;
    }
    let outgoing_bpm = outgoing.bpm;
    if outgoing_bpm <= 0.0 || incoming.bpm <= 0.0 {
        return None;
    }
    let incoming_bpm = align_tempo_octave(outgoing_bpm, incoming.bpm);
    // Refuse the phrase-switch rather than warp past the transparent band —
    // the adaptive-overlap path will offer a filtered handoff instead.
    let stretch_ratio =
        transparent_playback_rate(outgoing_bpm, incoming.bpm, MAX_BEATMATCH_STRETCH)?;

    let outgoing_length = outgoing.duration;
    let incoming_length = incoming.duration;
    if outgoing_length <= 0.0 || incoming_length <= 0.0 {
        return None;
    }

    let incoming_beat_seconds = 60.0 / incoming_bpm;
    let outgoing_beat_seconds = 60.0 / outgoing_bpm;

    let long = long_blend_allowed(outgoing, incoming);
    let max_seconds = if long {
        LONG_BLEND_SECONDS
    } else {
        UPSTREAM_MAX_OVERLAP_SECONDS
    };
    let max_beats = if long {
        LONG_BLEND_BEATS
    } else {
        UPSTREAM_MAX_FADE_BEATS
    };
    // The drop, or the furthest handoff the overlap can reach. Never the cue.
    let incoming_drop_time = incoming_handoff(incoming, max_seconds);

    let content_end = if outgoing.content_end > 0.0 {
        outgoing.content_end
    } else {
        outgoing_length
    };
    let (mix_out_anchor, _) = resolve_mix_out_anchor(outgoing);
    let unshifted_overlap_end = outgoing_length.min(mix_out_anchor);
    let outgoing_arrangement_overlap = if (mix_out_anchor - content_end).abs() < 0.05 {
        (ARRANGEMENT_OVERLAP_BEATS * outgoing_beat_seconds).min(MAX_DISCARDED_MUSIC_SECONDS)
    } else {
        0.0
    };
    let overlap_end_target =
        MIN_CLEARANCE_SECONDS.max(unshifted_overlap_end - outgoing_arrangement_overlap);

    let audible_start = audible_start_of(incoming);
    let available_fade_beats =
        (incoming_drop_time - audible_start).max(0.0) / incoming_beat_seconds;
    let capped_by_overlap = ((max_seconds / incoming_beat_seconds).floor() as usize / 4) * 4;
    if capped_by_overlap < MIN_FADE_BEATS {
        return None;
    }
    let mut fade_beats = max_beats
        .min(capped_by_overlap)
        .min(((available_fade_beats / 4.0).floor() as usize) * 4);
    if fade_beats < MIN_FADE_BEATS {
        fade_beats = MIN_FADE_BEATS;
    }

    while fade_beats > MIN_FADE_BEATS
        && clash_over(
            outgoing,
            incoming,
            overlap_end_target,
            outgoing_beat_seconds,
            incoming_beat_seconds,
            incoming_drop_time,
            audible_start,
            stretch_ratio,
            fade_beats,
        )
    {
        fade_beats -= 4;
    }

    let coverable_beats =
        ((incoming_drop_time - audible_start).max(0.0) / incoming_beat_seconds).floor() as usize;
    let overlap_beats = fade_beats.min(coverable_beats);
    if overlap_beats < 1 {
        return None;
    }

    let outgoing_overlap_seconds = overlap_beats as f64 * outgoing_beat_seconds;
    let overlap_seconds = overlap_beats as f64 * incoming_beat_seconds;

    let requested_incoming_handoff =
        incoming_drop_time + ARRANGEMENT_OVERLAP_BEATS * incoming_beat_seconds;
    let max_incoming_handoff = incoming_length - MIN_CLEARANCE_SECONDS;
    if max_incoming_handoff < incoming_drop_time {
        return None;
    }
    let incoming_handoff_time = requested_incoming_handoff.min(max_incoming_handoff);
    let entry = incoming_entry(incoming);
    let incoming_cue_time = cue_for(entry, incoming_handoff_time, overlap_seconds, stretch_ratio);
    if incoming_cue_time < audible_start - 0.5 {
        return None;
    }

    let start_target = overlap_end_target - outgoing_overlap_seconds;
    let transition_start =
        nearest_at_or_before(&outgoing.downbeats, start_target).unwrap_or(start_target);
    if transition_start < MIN_CLEARANCE_SECONDS {
        return None;
    }
    let transition_end = transition_start + outgoing_overlap_seconds;
    if transition_end > outgoing_length + 0.05 {
        return None;
    }

    let incoming_resume_time = incoming_cue_time + overlap_seconds;
    if incoming_resume_time + MIN_CLEARANCE_SECONDS > incoming_length {
        return None;
    }

    let bass_swap_fraction = bass_swap_fraction_for(
        outgoing,
        incoming,
        transition_start,
        incoming_cue_time,
        outgoing_beat_seconds,
        incoming_beat_seconds,
        overlap_seconds,
        overlap_beats,
    );
    let vocal_overlap = planned_vocal_overlap(
        outgoing,
        incoming,
        transition_start,
        transition_end,
        incoming_cue_time,
        stretch_ratio,
    );

    // The whole-track vocal probability cannot answer "does this handover put
    // two singers together": it sits at 0.82–0.86 on all thirteen corpus
    // tracks, sung or not. The windows the handover actually covers do
    // discriminate — 0.28 through an instrumental outro, 0.91 through a sung
    // one — so the clash is read there.
    let (anchor, _) = resolve_mix_out_anchor(outgoing);
    let vocal_conflict = handover_vocal_clash(outgoing, incoming, anchor, entry);
    let style = TransitionStyle::DjBlend;
    let playback_rate = stretch_ratio;
    let fade_seconds = transition_end - transition_start;
    let shape = mix_shape(
        style,
        true,
        vocal_overlap,
        vocal_conflict,
        fade_seconds,
        outgoing_beat_seconds,
        bass_swap_fraction,
        playback_rate,
        outgoing_bpm,
    );
    Some(TransitionPlan {
        style,
        bass_swap: true,
        bass_swap_fraction,
        filter_sweep: 0.0,
        vocal_overlap,
        fade_seconds,
        outgoing_hold_seconds: 0.0,
        bridge_start_seconds: 0.0,
        bridge_seconds: 0.0,
        bridge_repeats: 0,
        effect_seconds: 0.0,
        effect_start_seconds: 0.0,
        transition_end_seconds: transition_end,
        cue_seconds: incoming_cue_time,
        playback_rate,
        bed_fraction: shape.bed_fraction,
        bed_gain_db: shape.bed_gain_db,
        dip_depth: shape.dip_depth,
        dip_width: shape.dip_width,
        post_glide_seconds: shape.post_glide_seconds,
        outgoing_duration_seconds: outgoing.duration,
        beat_seconds: 0.0,
        reason: String::new(),
})
}

/// The most ambitious move: run the incoming intro under the outgoing and close
/// on its drop, gated on harmonic compatibility (upstream `phraseSwitch`).
fn phrase_switch(outgoing: &Analysis, incoming: &Analysis) -> Option<TransitionPlan> {
    if !harmonically_compatible(&trusted_key(outgoing), &trusted_key(incoming)) {
        return None;
    }
    plan_wsola_transition(outgoing, incoming)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The cache is what stops a transport command from re-decoding both tracks.
    /// It must key on the file's *content identity*, not just its path: a
    /// streamed download is appended to under a stable name, and serving a
    /// half-downloaded file's analysis forever would misplace every cue.
    #[test]
    fn analysis_cache_hits_by_identity_and_misses_when_the_file_grows() {
        let dir = std::env::temp_dir().join("bitchord-plan-cache");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("growing.bin");
        std::fs::write(&path, vec![0u8; 1024]).unwrap();
        let key = path.to_string_lossy().to_string();

        let computed = AtomicUsize::new(0);
        let mut compute = || {
            computed.fetch_add(1, Ordering::SeqCst);
            Analysis::default()
        };

        let first = analysis_cache::get_or_compute(&key, false, 90.0, &mut compute);
        let second = analysis_cache::get_or_compute(&key, false, 90.0, &mut compute);
        assert_eq!(computed.load(Ordering::SeqCst), 1, "a repeat must be a hit");
        assert!(
            Arc::ptr_eq(&first, &second),
            "a hit returns the same analysis"
        );

        let _ = analysis_cache::get_or_compute(&key, false, 91.0, &mut compute);
        assert_eq!(
            computed.load(Ordering::SeqCst),
            2,
            "a newly learned duration must not reuse an unknown-duration analysis"
        );

        // The same path with more bytes is a different file as far as planning
        // is concerned.
        std::fs::write(&path, vec![0u8; 2048]).unwrap();
        let _ = analysis_cache::get_or_compute(&key, false, 90.0, &mut compute);
        assert_eq!(
            computed.load(Ordering::SeqCst),
            3,
            "a grown file must be analysed again, not served from the cache"
        );

        // The vocal-model switch is part of the identity too.
        let _ = analysis_cache::get_or_compute(&key, true, 90.0, &mut compute);
        assert_eq!(computed.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn disk_cache_hit_skips_compute_and_corrupt_entry_is_rewritten() {
        let dir = std::env::temp_dir().join(format!("bitchord-analysis-disk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        analysis_cache::set_test_disk_dir(dir.clone());
        let source = dir.join("source-audio");
        std::fs::write(&source, b"audio identity").unwrap();
        let path = source.to_string_lossy().to_string();
        let calls = AtomicUsize::new(0);
        let first = analysis_cache::get_or_compute(&path, false, 120.0, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Analysis { bpm: 123.0, ..Analysis::default() }
        });
        analysis_cache::clear();
        let second = analysis_cache::get_or_compute(&path, false, 120.0, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Analysis::default()
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1, "disk hit skips analysis/decode");
        assert_eq!(second.bpm, first.bpm);

        analysis_cache::clear();
        let after_model_change = analysis_cache::get_or_compute_for_identity(&path, false, 120.0, "changed-model".into(), || {
            calls.fetch_add(1, Ordering::SeqCst);
            Analysis { bpm: 125.0, ..Analysis::default() }
        });
        assert_eq!(calls.load(Ordering::SeqCst), 2, "model digest change must miss");
        assert_eq!(after_model_change.bpm, 125.0);

        let (size, modified) = analysis_cache::file_identity(&path);
        let key = (path.clone(), size, modified, 120.0f64.to_bits(), super::super::models::analysis_identity(), false);
        std::fs::write(dir.join(format!("{}.json", analysis_cache::disk_key(&key))), b"{").unwrap();
        analysis_cache::clear();
        let repaired = analysis_cache::get_or_compute(&path, false, 120.0, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Analysis { bpm: 124.0, ..Analysis::default() }
        });
        assert_eq!(calls.load(Ordering::SeqCst), 3, "corruption must miss and recompute");
        assert_eq!(repaired.bpm, 124.0);
        assert!(serde_json::from_slice::<Analysis>(&std::fs::read(dir.join(format!("{}.json", analysis_cache::disk_key(&key)))).unwrap()).is_ok());

        let degraded_source = dir.join("degraded-source");
        std::fs::write(&degraded_source, b"temporary short read").unwrap();
        let degraded_path = degraded_source.to_string_lossy().to_string();
        analysis_cache::clear();
        let degraded = analysis_cache::get_or_compute(&degraded_path, false, 120.0, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Analysis::default()
        });
        assert_eq!(degraded.bpm, 0.0);
        let (size, modified) = analysis_cache::file_identity(&degraded_path);
        let degraded_key = (degraded_path.clone(), size, modified, 120.0f64.to_bits(), super::super::models::analysis_identity(), false);
        assert!(!dir.join(format!("{}.json", analysis_cache::disk_key(&degraded_key))).exists(), "degraded results are memory-only");
        analysis_cache::clear();
        let retried = analysis_cache::get_or_compute(&degraded_path, false, 120.0, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Analysis { bpm: 127.0, ..Analysis::default() }
        });
        assert_eq!(calls.load(Ordering::SeqCst), 5, "a degraded result must be recomputed next time");
        assert_eq!(retried.bpm, 127.0);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A track whose music starts at `first_beat`, on a 120 BPM grid. The
    /// pickup is reported the way the real analysis reports it — as both the
    /// envelope's `audible_start` and the grid's `first_beat` — because the
    /// cue is derived from whichever of them is earliest.
    fn analysis(first_beat: f64, duration: f64, downbeats: Vec<f64>) -> Analysis {
        Analysis {
            first_beat,
            audible_start_time: first_beat,
            duration,
            downbeats,
            bpm: 120.0,
            beat_interval: 0.5,
            ..Analysis::default()
        }
    }

    /// An energy curve and a vocal mask at 0.5 s resolution, so the vocal
    /// windows a handover reads have something to read.
    fn with_vocal_curve(a: &mut Analysis, seconds: f64, mask: f64) {
        let points = (0..((seconds / 0.5) as usize))
            .map(|index| audio_analysis::EnergyPoint {
                time: index as f64 * 0.5,
                energy: 0.8,
            })
            .collect::<Vec<_>>();
        a.vocal_activity_mask = vec![mask; points.len()];
        a.energy_curve = points;
    }

    /// A curve at one energy, then `tail` seconds of it falling away to 0.05.
    fn with_falling_tail(a: &mut Analysis, seconds: f64, tail: f64) {
        let points = (0..((seconds / 0.5) as usize))
            .map(|index| {
                let time = index as f64 * 0.5;
                let energy = if time < seconds - tail {
                    0.8
                } else {
                    0.8 * (1.0 - (time - (seconds - tail)) / tail).max(0.06)
                };
                audio_analysis::EnergyPoint { time, energy }
            })
            .collect::<Vec<_>>();
        a.energy_curve = points;
        a.vocal_activity_mask = vec![0.5; a.energy_curve.len()];
    }

    fn flat_out_and_tempo_gap(seconds: f64, bpm: f64, incoming_bpm: f64) -> (Analysis, Analysis) {
        let mut out = analysis(0.0, seconds + 5.0, vec![0.0, 2.0, 4.0]);
        out.bpm = bpm;
        out.beat_interval = 60.0 / bpm;
        out.beat_confidence = 0.8;
        out.content_end = seconds;
        with_vocal_curve(&mut out, seconds + 5.0, 0.5);
        let mut inc = analysis(0.0, seconds, vec![0.0, 2.0, 4.0]);
        inc.bpm = incoming_bpm;
        inc.beat_interval = 60.0 / incoming_bpm;
        inc.beat_confidence = 0.8;
        with_vocal_curve(&mut inc, seconds, 0.5);
        (out, inc)
    }

    #[test]
    fn an_unmatched_pair_crosses_over_where_the_tail_falls() {
        // 8 % apart, the outgoing record's last 12 s fall away, and the
        // session's effect budget is spent: a clean equal-power crossfade
        // placed on the fall, at the record's own rate.
        let (mut out, inc) = flat_out_and_tempo_gap(200.0, 120.0, 130.0);
        with_falling_tail(&mut out, 205.0, 12.0);
        let plan = plan_from(&out, &inc, 8.0, false);
        assert_eq!(plan.style, TransitionStyle::DjFilter);
        assert!(plan.filter_sweep > 0.0, "a filter fade, not a bare one");
        assert_eq!(plan.playback_rate, 1.0, "an unmatched pair is not stretched");
        assert!(plan.effect_seconds == 0.0);
        assert!(!plan.bass_swap, "no bass swap on a clean crossfade");
        assert!(
            plan.fade_seconds >= 8.0 * 60.0 / 120.0 - 1e-6,
            "at least a phrase: {:.2}s",
            plan.fade_seconds
        );
        assert!((plan.transition_end_seconds - out.content_end).abs() < 1e-6);
        assert!(plan.reason.contains("falls away"), "{}", plan.reason);
    }

    #[test]
    fn an_unmatched_pair_with_effect_budget_gets_its_technique() {
        // The owner's call: the paced effect slot is taken on an unmatched
        // pair even where the tail falls away, and a falling tail is never
        // given a cut, which would hold the next record across a quiet gap.
        let (mut out, inc) = flat_out_and_tempo_gap(200.0, 120.0, 130.0);
        with_falling_tail(&mut out, 205.0, 12.0);
        let plan = plan_from(&out, &inc, 8.0, true);
        assert!(plan.style.is_effect(), "got {:?}: {}", plan.style, plan.reason);
        assert_ne!(plan.style, TransitionStyle::DropCut);
        assert!(plan.effect_seconds > 0.0);
        // Thrown where the record is still strong, not into the faded tail.
        assert!(
            plan.effect_start_seconds <= out.content_end - 3.0,
            "thrown at {:.1}s of {:.1}s",
            plan.effect_start_seconds,
            out.content_end
        );
        assert!(
            (plan.outgoing_hold_seconds - (plan.fade_seconds - plan.effect_seconds)).abs() < 1e-6,
            "the outgoing holds until its effect"
        );
    }

    #[test]
    fn an_effect_brings_the_next_record_in_before_the_throw() {
        // "A cloud, then the next song": the incoming has to be under the
        // outgoing, plainly audible, for two bars before the technique takes
        // the outgoing out, and the blend still ends where the analysis put it.
        let (out, inc) = flat_out_and_tempo_gap(200.0, 120.0, 130.0);
        let plan = plan_from(&out, &inc, 8.0, true);
        assert!(plan.style.is_effect(), "got {:?}: {}", plan.style, plan.reason);
        let beat = plan.beat_seconds;
        let intro = (EFFECT_INTRO_BEATS * beat).max(EFFECT_INTRO_MIN_SECONDS);
        if plan.style != TransitionStyle::DropCut {
            assert!(
                plan.outgoing_hold_seconds >= intro.min(AUTO_TRANSITION_MAX_SECONDS - plan.effect_seconds) - 1e-6,
                "intro {:.2}s, wanted {intro:.2}s: {}",
                plan.outgoing_hold_seconds,
                plan.reason
            );
            assert!(plan.bed_gain_db >= EFFECT_INTRO_KEY_CLASH_DB && plan.bed_gain_db < 0.0);
            // A 130/120 pair is a grid clash: the incoming's top is held back.
            assert_eq!(plan.filter_sweep, 1.0);
        }
        assert!(plan.transition_end_seconds <= out.content_end + 1e-6);
    }

    #[test]
    fn the_effect_memory_skips_its_own_pair_and_keeps_two_others() {
        note_technique(1, TransitionStyle::ReverbWash);
        note_technique(2, TransitionStyle::EchoOut);
        // A re-plan of pair 2 sees only pair 1 behind it.
        assert_eq!(recent_techniques(2), vec![TransitionStyle::ReverbWash]);
        // A new pair sees both, newest first.
        assert_eq!(recent_techniques(3), vec![TransitionStyle::EchoOut, TransitionStyle::ReverbWash]);
        // A re-plan replaces its own entry rather than adding one.
        note_technique(2, TransitionStyle::LoopRoll);
        assert_eq!(recent_techniques(3), vec![TransitionStyle::LoopRoll, TransitionStyle::ReverbWash]);
    }

    #[test]
    fn a_spent_effect_budget_keeps_the_blend() {
        // The same pair that earns a technique with the budget available gets
        // the analysed blend when the session's last four transitions already
        // used one, and says why.
        let (out, inc) = flat_out_and_tempo_gap(200.0, 120.0, 130.0);
        assert!(plan_from(&out, &inc, 8.0, true).style.is_effect());
        let paced = plan_from(&out, &inc, 8.0, false);
        assert!(!paced.style.is_effect(), "got {:?}", paced.style);
        assert!(paced.reason.contains("pacing"), "{}", paced.reason);
    }

    #[test]
    fn a_busy_handover_earns_the_technique() {
        // The same 8 % gap, but both records are still at full energy across
        // the join: there is nowhere to cross over, so the technique stands.
        let (out, inc) = flat_out_and_tempo_gap(200.0, 120.0, 130.0);
        let plan = plan_from(&out, &inc, 8.0, true);
        assert!(plan.style.is_effect(), "got {:?}: {}", plan.style, plan.reason);
        assert!(!plan.reason.is_empty());
    }

    #[test]
    fn a_key_clash_earns_an_effect_even_with_a_falling_tail() {
        // Pair 10 of the corpus: C minor against A♭ major. They are a semitone
        // distance no blend wants and neither is a Camelot neighbour, so the
        // effect stands even though the outgoing tail falls away.
        let (mut out, mut inc) = flat_out_and_tempo_gap(200.0, 144.0, 121.0);
        out.key = "C minor".into();
        out.key_confidence = 0.51;
        inc.key = "A♭ major".into();
        inc.key_confidence = 0.35;
        with_falling_tail(&mut out, 205.0, 12.0);
        assert!(keys_clash(&out.key, out.key_confidence, &inc.key, inc.key_confidence));
        let plan = plan_from(&out, &inc, 8.0, true);
        assert!(plan.style.is_effect(), "got {:?}: {}", plan.style, plan.reason);
    }

    #[test]
    fn camelot_neighbours_and_relative_keys_never_clash() {
        // C major and A minor: the same number on the wheel.
        assert!(!keys_clash("C major", 0.6, "A minor", 0.6));
        // A minor (8A) and E minor (9A): one step, same letter.
        assert!(!keys_clash("A minor", 0.6, "E minor", 0.6));
        // C major (8B) and G major (9B): one step the other way, and a fifth.
        assert!(!keys_clash("C major", 0.6, "G major", 0.6));
        // C major against F♯ major: six semitones and no wheel relationship.
        assert!(keys_clash("C major", 0.6, "F♯ major", 0.6));
        // One side under the trust gate is no key at all.
        assert!(!keys_clash("C major", 0.24, "F♯ major", 0.9));
        assert!(!keys_clash("C major", 0.9, "F♯ major", 0.10));
        // The Camelot numbers themselves.
        assert_eq!(camelot("A minor"), Some((8, true)));
        assert_eq!(camelot("C major"), Some((8, false)));
        assert_eq!(camelot("E minor"), Some((9, true)));
        assert_eq!(camelot("G major"), Some((9, false)));
        assert_eq!(camelot(""), None);
    }

    #[test]
    fn a_vocal_handover_ducks_the_blend_instead_of_earning_an_effect() {
        // 100 against 106 BPM is a 6 % gap — inside the filter band — and both
        // records sing through the handover, which used to fire the effect rule
        // through the whole-track vocal probability. The answer is a filtered,
        // ducked blend, not an EchoOut that throws the outgoing record away.
        let mut out = analysis(0.0, 205.0, vec![0.0, 2.0, 4.0]);
        out.bpm = 100.0;
        out.beat_interval = 0.6;
        out.beat_confidence = 0.8;
        out.content_end = 200.0;
        out.vocal_probability = 0.85;
        with_vocal_curve(&mut out, 205.0, 0.85);
        let mut inc = analysis(0.0, 200.0, vec![0.0, 2.0, 4.0]);
        inc.bpm = 106.0;
        inc.beat_interval = 60.0 / 106.0;
        inc.beat_confidence = 0.8;
        inc.vocal_probability = 0.85;
        with_vocal_curve(&mut inc, 200.0, 0.85);
        let plan = plan_from(&out, &inc, 8.0, true);
        assert!(!plan.style.is_effect(), "got {:?}: {}", plan.style, plan.reason);
        assert!(plan.vocal_overlap > 0.0, "the clash must still be measured");
        assert!(plan.dip_depth > 0.0, "the mids are ducked");
        assert!(
            plan.reason.contains("ducked"),
            "the reason names the duck: {}",
            plan.reason
        );
    }

    #[test]
    fn an_instrumental_outro_is_not_a_vocal_clash() {
        // The outgoing's whole-track probability is 0.85, but its last two bars
        // are instrumental: the handover windows say so and the blend is not
        // reported as a vocal clash.
        let mut out = analysis(0.0, 205.0, vec![0.0, 2.0, 4.0]);
        out.bpm = 100.0;
        out.beat_interval = 0.6;
        out.beat_confidence = 0.8;
        out.content_end = 200.0;
        out.vocal_probability = 0.85;
        with_vocal_curve(&mut out, 205.0, 0.85);
        let tail_from = ((200.0 - 8.0 * 0.6) / 0.5) as usize;
        for value in out.vocal_activity_mask.iter_mut().skip(tail_from) {
            *value = 0.15;
        }
        let mut inc = analysis(0.0, 200.0, vec![0.0, 2.0, 4.0]);
        inc.bpm = 106.0;
        inc.beat_interval = 60.0 / 106.0;
        inc.beat_confidence = 0.8;
        inc.vocal_probability = 0.85;
        with_vocal_curve(&mut inc, 200.0, 0.85);
        let plan = plan_from(&out, &inc, 8.0, true);
        assert!(!plan.style.is_effect(), "got {:?}: {}", plan.style, plan.reason);
        assert!(plan.reason.is_empty(), "no vocal clash to report: {}", plan.reason);
        assert_eq!(plan.vocal_overlap, 0.0);
    }

    /// The pairs an autoplay run is made of — different tempos, a confident
    /// grid — used to fall to a fade. They now get a named technique whose
    /// landing sits on a downbeat inside the outgoing record.
    #[test]
    fn mismatched_tempos_get_a_dj_technique_not_a_fade() {
        let grid = |bpm: f64| (0..200).map(|i| i as f64 * 4.0 * 60.0 / bpm).collect::<Vec<_>>();
        let mut out = analysis(0.0, 210.0, grid(124.0));
        out.bpm = 124.0;
        out.beat_interval = 60.0 / 124.0;
        out.beat_confidence = 0.8;
        out.content_end = 205.0;
        let mut inc = analysis(0.0, 190.0, grid(92.0));
        inc.bpm = 92.0;
        inc.beat_interval = 60.0 / 92.0;
        inc.beat_confidence = 0.8;
        let plan = plan_from(&out, &inc, 8.0, true);
        assert!(plan.style.is_effect(), "got {:?}: {}", plan.style, plan.reason);
        assert!(!plan.reason.is_empty());
        assert!(plan.transition_end_seconds <= 205.0 + 1e-6);
        assert!((plan.beat_seconds - 60.0 / 124.0).abs() < 1e-6);
        assert_eq!(plan.playback_rate, 1.0, "a technique never stretches");
        // A′2: the effect rides the analysed crossfade, not its own short window.
        assert!(
            plan.fade_seconds >= AUTO_MIN_SECONDS - 1e-6,
            "effect blend {:.2}s under the 4s floor",
            plan.fade_seconds
        );
        assert!(plan.effect_seconds > 0.0 && plan.effect_seconds <= plan.fade_seconds + 1e-6);
        assert!(
            (plan.effect_start_seconds + plan.effect_seconds - plan.transition_end_seconds).abs()
                < 1e-6,
            "effect window {}..{} does not end at the blend end {}",
            plan.effect_start_seconds,
            plan.effect_start_seconds + plan.effect_seconds,
            plan.transition_end_seconds
        );
    }

    #[test]
    fn effect_earned_only_for_a_real_clash() {
        assert!(!effect_earned(false, 0.0));
        // The 4–7 % DjFilter band is still a blend.
        assert!(!effect_earned(false, 0.049));
        assert!(!effect_earned(false, 0.069));
        // A key the stretch cannot reconcile, or a gap past the filter band,
        // earns an effect; a vocal overlap never does on its own — the blend
        // ducks it instead.
        assert!(effect_earned(true, 0.0));
        assert!(effect_earned(false, 0.071));
    }

    #[test]
    fn a_tempo_is_counted_in_the_octave_that_plans() {
        // The corpus's four out-of-band readings fold to the octave the DSP
        // estimator and the Music Understanding overlay agree on.
        assert!((fold_tempo(44.0) - 88.0).abs() < 1e-9);
        assert!((fold_tempo(196.0) - 98.0).abs() < 1e-9);
        assert!((fold_tempo(198.2) - 99.1).abs() < 1e-9);
        assert!((fold_tempo(207.7) - 103.85).abs() < 1e-9);
        // Readings already in the band are left exactly alone.
        for bpm in [70.0, 88.0, 93.4, 121.3, 127.0, 143.0, 174.9] {
            assert!((fold_tempo(bpm) - bpm).abs() < 1e-9, "{bpm}");
        }
        // A nonsense tempo is passed through rather than spun to nothing.
        assert_eq!(fold_tempo(0.0), 0.0);
        assert!(fold_tempo(f64::NAN).is_nan());
    }

    #[test]
    fn two_to_one_tempos_are_the_same_beat() {
        // A 198 BPM grid against a 99 BPM one is double time, not a clash: the
        // ratio folds by an octave and the gap that decides an effect is ~0.
        let ratio = normalized_tempo_ratio(198.2, 99.0);
        assert!((ratio - 1.0).abs() < 0.01, "ratio {ratio}");
        assert!(!effect_earned(false, (1.0 - ratio).abs()));
        assert!(!effect_earned(false, (1.0 - normalized_tempo_ratio(99.0, 198.2)).abs()));
        // …and the log says so instead of printing a raw 2:1 beside "clash".
        let summary = tempo_summary(198.2, 99.0);
        assert!(summary.contains("2×"), "{summary}");
        assert!(summary.contains("gap 0%"), "{summary}");
    }

    #[test]
    fn three_to_two_is_not_a_beatmatch() {
        // 3:2 is a polyrhythm, not the same beat: it stays a clash in both
        // directions, which is why the fold is a power of two and not a
        // harmonic ratio.
        for (out, inc) in [(120.0, 180.0), (180.0, 120.0)] {
            let gap = (1.0 - normalized_tempo_ratio(out, inc)).abs();
            assert!(gap > 0.07, "{out}→{inc} gap {gap}");
            assert!(effect_earned(false, gap));
        }
    }

    #[test]
    fn a_near_ratio_is_reported_without_an_octave_fold() {
        let summary = tempo_summary(127.0, 134.0);
        assert!(!summary.contains("×"), "{summary}");
        assert!(summary.contains("gap 6%"), "{summary}");
    }

    #[test]
    fn a_five_percent_gap_keeps_the_analysed_blend() {
        let grid = |bpm: f64| (0..200).map(|i| i as f64 * 4.0 * 60.0 / bpm).collect::<Vec<_>>();
        let mut out = analysis(0.0, 210.0, grid(127.0));
        out.bpm = 127.0;
        out.beat_interval = 60.0 / 127.0;
        out.beat_confidence = 0.9;
        out.content_end = 205.0;
        out.key = "C major".into();
        out.key_confidence = 0.9;
        let mut inc = analysis(0.0, 190.0, grid(121.0));
        inc.bpm = 121.0;
        inc.beat_interval = 60.0 / 121.0;
        inc.beat_confidence = 0.9;
        inc.key = "C major".into();
        inc.key_confidence = 0.9;
        let plan = plan_from(&out, &inc, 8.0, true);
        assert!(!plan.style.is_effect(), "a 5 % gap got {:?}: {}", plan.style, plan.reason);
        assert!(plan.fade_seconds >= AUTO_MIN_SECONDS - 1e-6);
    }

    /// A real beatmatch is still a blend: the techniques are for pairs that
    /// cannot share a grid, not a replacement for one that can.
    #[test]
    fn a_true_beatmatch_still_blends() {
        let grid = (0..200).map(|i| i as f64 * 2.0).collect::<Vec<_>>();
        let mut out = analysis(0.0, 210.0, grid.clone());
        out.beat_confidence = 0.9;
        let mut inc = analysis(0.0, 210.0, grid);
        inc.beat_confidence = 0.9;
        let plan = plan_from(&out, &inc, 8.0, true);
        assert!(!plan.style.is_effect(), "got {:?}", plan.style);
    }

    #[test]
    fn partial_whole_track_decode_is_not_published_as_a_silent_tail() {
        let rate = audio_analysis::ANALYSIS_RATE as u32;
        // 96% is above the broad coverage ratio but still leaves 0.4 s of a
        // short track undecoded; it must not be interpreted as terminal silence.
        let partial = vec![0.2f32; rate as usize * 96 / 10];
        let analyzed = analyze(
            "growing-source",
            true,
            &|_, _, _, _| Some((partial.clone(), rate, 0.0)),
            10.0,
        );

        assert_eq!(analyzed.duration, 10.0);
        assert_eq!(analyzed.content_end, 0.0);
        assert!(analyzed.analysis_source.is_empty());
    }

    #[test]
    fn supplied_stream_duration_beats_local_tag_probe_fallback() {
        let outgoing = "https://audio.example.test/outgoing.m4a";
        let incoming = "https://audio.example.test/incoming.m4a";
        let plan = plan_pair(
            outgoing,
            incoming,
            "spoken podcast episode",
            "",
            false,
            8.0,
            83.0,
            91.0,
            true,
            "test",
            true,
            |_, _, _, _| None,
            |_| 0.0,
        );

        // The caller knows the stream's duration even though a local tag probe
        // cannot open its URL. Keeping that duration anchors the safety fade
        // and lets non-blocked stream pairs run whole-track analysis.
        assert_eq!(plan.outgoing_duration_seconds, 83.0);
        assert_eq!(plan.transition_end_seconds, 83.0);
    }

    #[test]
    fn bass_swap_fraction_defaults_and_stays_bounded() {
        let out = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
        let inc = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
        // Non-positive overlap returns the fixed default.
        assert!(
            (bass_swap_fraction_for(&out, &inc, 0.0, 0.5, 0.5, 0.5, 0.0, 16) - 0.7).abs() < 1e-9
        );
        // A normal overlap stays bounded even with no low-energy structure.
        let fraction = bass_swap_fraction_for(&out, &inc, 0.0, 0.5, 0.5, 0.5, 6.0, 16);
        assert!((0.0..=1.0).contains(&fraction), "fraction {fraction}");
    }

    #[test]
    fn is_vocal_clash_needs_both_sides_above_threshold() {
        assert!(is_vocal_clash(Some(0.8), Some(0.9)));
        assert!(!is_vocal_clash(Some(0.8), Some(0.5)));
        assert!(!is_vocal_clash(None, Some(0.9)));
        assert!(!is_vocal_clash(Some(0.9), None));
    }

    #[test]
    fn vocal_activity_between_averages_the_window() {
        let mut a = analysis(0.0, 200.0, vec![]);
        a.energy_curve = vec![
            audio_analysis::EnergyPoint {
                time: 0.0,
                energy: 1.0,
            },
            audio_analysis::EnergyPoint {
                time: 1.0,
                energy: 1.0,
            },
            audio_analysis::EnergyPoint {
                time: 2.0,
                energy: 1.0,
            },
            audio_analysis::EnergyPoint {
                time: 3.0,
                energy: 1.0,
            },
        ];
        a.vocal_activity_mask = vec![0.0, 0.5, 1.0, 0.5];
        // The [0.5, 2.5] window covers times 1.0 and 2.0 → mean of 0.5 and 1.0.
        let mean = vocal_activity_between(&a, 0.5, 2.5).unwrap();
        assert!((mean - 0.75).abs() < 1e-9, "mean {mean}");
    }

    /// The model windows override only their own ranges: the head and the tail
    /// are merged independently, and the DSP heuristic stands between them.
    #[test]
    fn a_model_window_overrides_only_its_own_range() {
        let mut a = analysis(0.0, 10.0, vec![0.0, 2.0, 4.0]);
        with_energy(&mut a, 10.0, |_| 0.8);
        for value in a.vocal_activity_mask.iter_mut() {
            *value = 0.8; // the DSP heuristic this test stands in for
        }
        let head = vec![0.05f32; 50]; // ≈ 1.16 s at the model's hop
        merge_open_unmix_window(&mut a, &head, 0.0);
        let tail = vec![0.9f32; 50];
        merge_open_unmix_window(&mut a, &tail, 8.0);
        let at = |time: f64| a.vocal_activity_mask[(time / 0.1) as usize];
        assert!(at(0.5) < 0.1, "head {}", at(0.5));
        assert!((at(5.0) - 0.8).abs() < 1e-9, "middle {}", at(5.0));
        assert!(at(8.5) > 0.8, "tail {}", at(8.5));
    }

    #[test]
    fn phrase_switch_refuses_incompatible_keys() {
        let mut out = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
        out.key = "C major".into();
        out.key_confidence = 0.9;
        out.bpm = 120.0;
        out.beat_confidence = 0.8;
        let mut inc = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
        inc.key = "D♯ major".into(); // a minor third — not a compatible move
        inc.key_confidence = 0.9;
        inc.bpm = 120.0;
        inc.beat_confidence = 0.8;
        assert!(phrase_switch(&out, &inc).is_none());
    }

    #[test]
    fn transparent_playback_rate_refuses_apple_style_warps() {
        // 120 → 90 is a 25% stretch after octave alignment fails to help.
        assert!(transparent_playback_rate(120.0, 90.0, MAX_BEATMATCH_STRETCH).is_none());
        // 120 → 122 is ~1.67% — transparent.
        let rate = transparent_playback_rate(120.0, 122.0, MAX_BEATMATCH_STRETCH).unwrap();
        assert!((rate - 120.0 / 122.0).abs() < 1e-4, "rate {rate}");
        // Octave cousins: 60 against 120 aligns to unity.
        let octave = transparent_playback_rate(120.0, 60.0, MAX_BEATMATCH_STRETCH).unwrap();
        assert!((octave - 1.0).abs() < 1e-4, "octave {octave}");
    }

    #[test]
    fn dj_assisted_near_tempo_does_not_stretch() {
        // Same tempo neighbourhood, weak grids → DjAssisted. Must keep unity
        // rate and ride filters rather than warp (Apple's failure mode).
        let mut out = matched(0.5, 200.0);
        out.beat_confidence = 0.4; // below MIN_BEATMATCH_CONFIDENCE (0.55)
        let mut inc = matched(0.5, 200.0);
        inc.bpm = 126.0; // 5% fast — inside the old ±10% assisted band
        inc.beat_interval = 60.0 / 126.0;
        inc.beat_confidence = 0.4;
        assert_eq!(assess(&out, &inc), Tier::DjAssisted);
        let plan = plan_from(&out, &inc, 8.0, true);
        assert!(
            (plan.playback_rate - 1.0).abs() < 1e-9,
            "DjAssisted stretched to {}",
            plan.playback_rate
        );
        assert_eq!(plan.style, TransitionStyle::DjFilter);
        let smooth = super::super::smoothness::score_plan(&plan);
        assert!(!smooth.forced_stretch);
    }

    #[test]
    fn section_fade_cap_takes_the_shorter_section() {
        let mut out = matched(0.5, 200.0);
        out.outro_start = 170.0;
        out.content_end = 180.0; // 10 s outro
        let mut inc = matched(0.5, 200.0);
        inc.audible_start_time = 0.5;
        inc.mix_in_time = 6.5; // 6 s intro
        let cap = section_fade_cap(&out, &inc).expect("both sections known");
        assert!((cap - 6.0).abs() < 1e-9, "cap {cap}");
    }

    #[test]
    fn overlay_skips_half_tempo_when_beat_this_grid_is_trusted() {
        let mut analysis = matched(0.5, 200.0);
        analysis.bpm = 148.7;
        analysis.beat_interval = 60.0 / 148.7;
        analysis.beat_confidence = 0.75;
        analysis.downbeats = vec![0.0, 1.615, 3.23];
        analysis.analysis_source = "beat_this".into();
        analysis.key = "C minor".into();
        apply_overlay(
            &mut analysis,
            &AnalysisOverlay {
                bpm: Some(74.5),
                beat_confidence: Some(0.85),
                beat_interval: Some(60.0 / 74.5),
                beats: None,
                downbeats: Some(vec![0.0, 3.23]),
                key: Some("C minor".into()),
                key_confidence: Some(0.8),
                phrase_boundaries: Some(vec![8.0, 16.0]),
                vocal_probability: None,
                content_end: None,
                audible_start: None,
                outro_start: None,
                mix_in_time: None,
                source: "music_understanding".into(),
            },
        );
        assert!(
            (analysis.bpm - 148.7).abs() < 0.01,
            "kept Beat This tempo, got {}",
            analysis.bpm
        );
        assert_eq!(analysis.phrase_boundaries, vec![8.0, 16.0]);
        assert!(analysis.analysis_source.contains("music_understanding"));
    }

    #[test]
    fn harmonically_compatible_matches_upstream_semantics() {
        // A fifth and a second are compatible; a minor third is not.
        assert!(harmonically_compatible("C major", "G major"));
        assert!(harmonically_compatible("C major", "D major"));
        assert!(!harmonically_compatible("C major", "D♯ major"));
        // Differing modes are compatible only within a second.
        assert!(harmonically_compatible("C major", "C minor"));
        assert!(!harmonically_compatible("C major", "D minor"));
        assert!(!harmonically_compatible("C major", "G minor"));
        // Unknown keys are never compatible.
        assert!(!harmonically_compatible("", "C major"));
    }

    #[test]
    fn key_distance_matches_upstream_semantics() {
        assert_eq!(key_distance("C major", "C major"), Some(0));
        // A fifth is the closest relationship on the circle.
        assert_eq!(key_distance("C major", "G major"), Some(5));
        // Same pitch class but different mode charges one step.
        assert_eq!(key_distance("C major", "C minor"), Some(1));
        // An unknown key yields no distance.
        assert_eq!(key_distance("", "C major"), None);
        // A low-confidence key is not trusted.
        let mut a = analysis(0.0, 0.0, vec![]);
        a.key = "C major".into();
        a.key_confidence = 0.1;
        assert_eq!(trusted_key(&a), "");
    }

    #[test]
    fn blocked_text_matches_word_boundaries_only() {
        assert!(blocked_text("The Daily Podcast Episode 12"));
        assert!(blocked_text("Live at the Apollo (Concert)"));
        assert!(blocked_text("an audiobook performance"));
        // "alive" and "lively" are not "live"; "concerted" is not "concert".
        assert!(!blocked_text("Stay Alive"));
        assert!(!blocked_text("Lively Discussion"));
        assert!(!blocked_text("A Concerted Effort"));
        assert!(!blocked_text("Ordinary Song Title"));
    }

    #[test]
    fn interior_energy_cliff_wins_the_mix_out_anchor() {
        let mut out = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
        out.content_end = 180.0;
        out.mix_out_candidates.push(audio_analysis::MixCuePoint {
            time: 175.0,
            score: 0.95,
            kind: "energy_cliff".into(),
        });
        // The cliff is 5 s before content end (inside the 12 s budget) and ranks
        // above the implicit content_end, so it is the anchor — and it is
        // interior, which is what gates the gapless path off.
        let (anchor, _) = resolve_mix_out_anchor(&out);
        assert!((anchor - 175.0).abs() < 1e-9, "anchor {anchor}");
        assert!(anchor < 180.0 - 1.0);
    }

    #[test]
    fn mix_out_anchor_falls_back_to_content_end() {
        let mut out = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
        out.content_end = 180.0;
        // No mix-out candidates and no energy curve → content_end is the anchor
        // with no discarded-music charge.
        let (time, discarded) = resolve_mix_out_anchor(&out);
        assert!((time - 180.0).abs() < 1e-9, "time {time}");
        assert_eq!(discarded, 0.0);
    }

    #[test]
    fn short_track_gets_plain_crossfade_despite_strong_beat_evidence() {
        // Upstream MIN_SMART_DURATION_SECONDS=45: a sub-45s track must never get
        // a smart transition, even when its beat grid would otherwise qualify.
        let mut short = analysis(0.5, 30.0, vec![0.5, 1.0, 1.5, 2.0]);
        short.bpm = 120.0;
        short.beat_confidence = 0.8;
        let mut long = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
        long.bpm = 120.0;
        long.beat_confidence = 0.8;

        let plan = plan_from(&long, &short, 8.0, true);
        assert_eq!(plan.style, TransitionStyle::EqualPower);
        assert!(!plan.bass_swap);
    }

    #[test]
    fn fade_is_capped_by_incoming_duration() {
        // A 30 s incoming track must not fade longer than 40 % of itself.
        let mut short = analysis(0.5, 30.0, vec![0.5, 1.0, 1.5, 2.0]);
        short.bpm = 90.0;
        short.beat_confidence = 0.8;
        let mut long = analysis(0.5, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
        long.bpm = 90.0;
        long.beat_confidence = 0.8;
        // Without the cap, 16 beats at 90 BPM is ~10.7 s.
        let plan = plan_from(&long, &short, 8.0, true);
        assert!(
            plan.fade_seconds <= 30.0 * 0.4 + 1e-6,
            "fade {}",
            plan.fade_seconds
        );
    }

    #[test]
    fn incoming_cue_uses_head_downbeat_not_outro() {
        let cue = incoming_cue(&analysis(
            1.2,
            200.0,
            vec![1.2, 3.0, 5.0, 180.0, 184.0, 188.0, 192.0, 196.0, 198.0],
        ));
        assert!(
            (cue - 1.2).abs() < 1e-9,
            "cue {cue} should be the first mix-in downbeat, not the outro"
        );
    }

    /// A mix-in is the top of the next record, not its drop.
    ///
    /// Regression: the cue used to be the highest-ranked phase-1 candidate, and
    /// that ranking put `main_drop` — 32 beats in — above `pickup`. Worse, when
    /// the DSP tempo read failed, `beat_interval` was 0, which sent the drop
    /// candidate to `intro_end_time`: the first moment the track got *loud*,
    /// capped at 48 s. That is a mix-*out* question used as a mix-in question,
    /// and it is where "the next song starts a minute in" came from.
    #[test]
    fn a_mix_in_never_reaches_for_the_drop() {
        // 120 BPM, pickup at 2 s, and a grid running the length of the track.
        let downbeats: Vec<f64> = (0..40).map(|i| 2.0 + i as f64 * 2.0).collect();
        let cue = incoming_cue(&analysis(2.0, 240.0, downbeats));
        assert!(
            (cue - 2.0).abs() < 1e-9,
            "a beat-aligned pickup at 2 s should cue at 2 s, got {cue}"
        );

        // And with the analysis reporting its pickup through the candidate list,
        // which is where the real value comes from.
        let with_candidate_grid: Vec<f64> = (0..40).map(|i| 2.0 + i as f64 * 2.0).collect();
        let mut with_candidate = analysis(2.0, 240.0, with_candidate_grid);
        with_candidate.audible_start_time = 2.0;
        with_candidate.mix_in_candidates = vec![audio_analysis::MixCuePoint {
            time: 2.0,
            score: 0.8,
            kind: "pickup".into(),
        }];
        assert!((incoming_cue(&with_candidate) - 2.0).abs() < 1e-9);
    }

    /// No usable grid means the top of the file, and never a guess.
    ///
    /// This is the degraded path in its purest form: the analysis is empty, so
    /// there is nothing to align to and nothing to say how deep a mix-in should
    /// reach. A plain crossfade from 0 is right; a crossfade from 45 s is the
    /// bug.
    #[test]
    fn a_mix_in_without_a_beat_grid_starts_at_the_top() {
        // No tempo at all.
        let mut blind = analysis(0.0, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
        blind.bpm = 0.0;
        blind.beat_interval = 0.0;
        assert_eq!(
            incoming_cue(&blind),
            0.0,
            "no tempo must not become a deep cue"
        );

        // A tempo outside the analyser's own range means the read failed, and a
        // failed read must not turn into a 32-beat cue either.
        let mut absurd = analysis(0.0, 200.0, vec![0.5, 1.0, 1.5, 2.0]);
        absurd.bpm = 12.0;
        absurd.beat_interval = 5.0;
        assert_eq!(
            incoming_cue(&absurd),
            0.0,
            "an out-of-range tempo is not a grid"
        );

        // A tempo with no downbeats is no grid either.
        let mut no_downbeats = analysis(0.0, 200.0, vec![]);
        no_downbeats.audible_start_time = 1.0;
        assert_eq!(incoming_cue(&no_downbeats), 0.0);
    }

    /// The three bounds, each of which is the one that bites in its own case.
    #[test]
    fn a_mix_in_is_bounded_by_seconds_beats_and_the_track() {
        // Seconds: a pickup deep into the record is pulled back to eight.
        let deep: Vec<f64> = (0..40).map(|i| 30.0 + i as f64 * 0.5).collect();
        let mut slow = analysis(30.0, 240.0, deep);
        slow.audible_start_time = 30.0;
        let cue = incoming_cue(&slow);
        assert!(
            cue <= MAX_CUE_SECONDS + 1e-9,
            "cue {cue} exceeded the {MAX_CUE_SECONDS}s cap"
        );

        // Beats: 16 beats of a 40 BPM track is 24 s, so the beat cap is the one
        // that applies — and a failed read at 40 BPM must not become 32 beats.
        let slow_grid: Vec<f64> = (0..200).map(|i| 1.0 + i as f64 * 1.5).collect();
        let mut ballad = analysis(1.0, 600.0, slow_grid);
        ballad.bpm = 40.0;
        ballad.beat_interval = 1.5;
        ballad.audible_start_time = 40.0;
        let cue = incoming_cue(&ballad);
        assert!(
            cue <= MAX_CUE_BEATS * 1.5 + 1e-9,
            "cue {cue} exceeded {MAX_CUE_BEATS} beats"
        );

        // Fraction: short material gets a proportionally smaller entry, because
        // eight seconds is an eighth of a one-minute clip and nothing at all in
        // a four-minute song.
        let short: Vec<f64> = (0..200).map(|i| 0.2 + i as f64 * 0.5).collect();
        let mut brief = analysis(0.2, 60.0, short);
        brief.audible_start_time = 12.0;
        let cue = incoming_cue(&brief);
        assert!(
            cue <= 60.0 * MAX_CUE_FRACTION + 1e-9,
            "cue {cue} exceeded {MAX_CUE_FRACTION} of a one-minute track"
        );
    }

    /// The cue lands on a beat, because entering between beats is the one thing
    /// a listener hears as "edited" rather than "mixed".
    #[test]
    fn a_mix_in_lands_on_the_grid() {
        let downbeats: Vec<f64> = (0..40).map(|i| 0.25 + i as f64 * 0.5).collect();
        let mut track = analysis(0.25, 200.0, downbeats.clone());
        // A pickup that falls a little after a downbeat.
        track.audible_start_time = 1.4;
        let cue = incoming_cue(&track);
        assert!(
            downbeats.iter().any(|d| (d - cue).abs() < 1e-6),
            "cue {cue} is not on the grid"
        );
        assert!(
            cue >= 1.4 && cue <= 1.4 + 0.5 + 1e-6,
            "cue {cue} should be the first downbeat at or after the 1.4 s pickup"
        );
    }

    #[test]
    fn incoming_cue_does_not_index_into_sorted_outro_beats() {
        // The old fallback picked downbeats[8] after merging and sorting tail
        // times, which for a 3-minute track is ~duration − WINDOW.
        let cue = incoming_cue(&analysis(
            0.0,
            180.0,
            vec![
                150.0, 154.0, 158.0, 162.0, 166.0, 170.0, 174.0, 176.0, 178.0,
            ],
        ));
        assert_eq!(cue, 0.0, "outro-only grid must not become a mix-in cue");
    }

    #[test]
    fn incoming_cue_rejects_pickup_in_the_last_ten_seconds() {
        let cue = incoming_cue(&analysis(175.0, 180.0, vec![175.0, 177.0, 179.0]));
        assert_eq!(cue, 0.0);
    }

    fn matched(first_beat: f64, duration: f64) -> Analysis {
        let step = 0.5;
        let downbeats: Vec<f64> = (0..((duration / step) as usize))
            .map(|i| first_beat + i as f64 * step)
            .filter(|t| *t < duration)
            .collect();
        let mut track = analysis(first_beat, duration, downbeats);
        track.beat_confidence = 0.9;
        track.content_end = duration;
        track.key = "C major".into();
        track.key_confidence = 0.9;
        track
    }

    /// An eight-bar intro is a runway, so the phrase switch fires and the cue
    /// stays at the top of that intro.
    #[test]
    fn phrase_switch_fires_on_an_eight_bar_intro_and_starts_at_the_entry() {
        let out = matched(0.5, 200.0);
        let mut inc = matched(0.5, 200.0);
        // 32 beats of intro: the drop, not the cue.
        inc.mix_in_candidates = vec![audio_analysis::MixCuePoint {
            time: 0.5 + 32.0 * 0.5,
            score: 0.95,
            kind: "main_drop".into(),
        }];
        let plan = phrase_switch(&out, &inc).expect("an eight-bar intro should phrase-switch");
        assert_eq!(plan.style, TransitionStyle::DjBlend);
        let entry = incoming_entry(&inc);
        assert!(
            plan.cue_seconds <= entry + MAX_CUE_SECONDS + 1e-6,
            "cue {} started more than {MAX_CUE_SECONDS}s past the entry {entry}",
            plan.cue_seconds
        );
        assert!(
            plan.cue_seconds < 0.5 + 32.0 * 0.5 - 1.0,
            "cue {} is the drop, not the entry",
            plan.cue_seconds
        );
        assert!(
            plan.bed_fraction > 0.0,
            "a phrase switch is a bed, not a crossfade"
        );
    }

    #[test]
    fn a_cue_is_never_more_than_eight_seconds_past_the_entry() {
        assert!(
            (cue_for(1.0, 80.0, 8.0, 1.0) - 9.0).abs() < 1e-9,
            "80 - 8 = 72, which must clamp to entry + 8"
        );
        let out = matched(0.5, 240.0);
        let mut inc = matched(0.5, 240.0);
        inc.mix_in_candidates = vec![audio_analysis::MixCuePoint {
            time: 90.0,
            score: 0.95,
            kind: "main_drop".into(),
        }];
        let plan = plan_from(&out, &inc, 8.0, true);
        let entry = incoming_entry(&inc);
        assert!(
            plan.cue_seconds <= entry + MAX_CUE_SECONDS + 1e-6,
            "cue {} exceeded entry {entry} + {MAX_CUE_SECONDS}s",
            plan.cue_seconds
        );
    }

    #[test]
    fn a_plain_crossfade_ends_where_the_music_ends() {
        let mut out = analysis(0.5, 200.0, vec![]);
        out.bpm = 0.0;
        out.beat_interval = 0.0;
        out.content_end = 170.0;
        let inc = analysis(0.5, 200.0, vec![]);
        let plan = plan_from(&out, &inc, 8.0, true);
        // No tempo is no longer a fade: the record washes out instead, and the
        // wash still ends where the music does.
        assert_eq!(plan.style, TransitionStyle::ReverbWash, "{}", plan.reason);
        assert!(
            (plan.transition_end_seconds - 170.0).abs() < 1e-6,
            "wash ended at {}, not the content end",
            plan.transition_end_seconds
        );
        let fade = plain_fallback(8.0, &out);
        assert_eq!(fade.style, TransitionStyle::EqualPower);
        assert!((fade.transition_end_seconds - 170.0).abs() < 1e-6);
        assert_eq!(plan.bed_fraction, 0.0);
        assert_eq!(plan.dip_depth, 0.0);
    }

    /// A grid snap that lands on the mix-out used to leave `mix_end - start`
    /// at a few dozen milliseconds, and `.max(0.5)` turned every one of those
    /// into a half-second cut.
    #[test]
    fn a_snapped_start_cannot_shrink_the_blend_to_half_a_second() {
        let (start, fade) = realized_window(180.0, 179.95, 12.0, AUTO_MIN_SECONDS);
        assert!(
            (fade - 12.0).abs() < 1e-6,
            "a collapsed snap must restore the planned blend, got {fade}s"
        );
        assert!((start - 168.0).abs() < 1e-6, "start {start}");

        let (kept_start, kept_fade) = realized_window(180.0, 168.0, 12.0, AUTO_MIN_SECONDS);
        assert!(
            (kept_fade - 12.0).abs() < 1e-6,
            "a good snap stays, got {kept_fade}"
        );
        assert!((kept_start - 168.0).abs() < 1e-6);

        // The tail is the whole window: do not invent blend time the track
        // does not have.
        let (_, short) = realized_window(2.0, 1.9, 12.0, AUTO_MIN_SECONDS);
        assert!((short - 2.0).abs() < 1e-6, "short tail {short}");
    }

    #[test]
    fn a_clean_beatmatched_pair_blends_for_more_than_thirty_two_beats() {
        let out = matched(0.5, 240.0);
        let inc = matched(0.5, 240.0);
        let plan = phrase_switch(&out, &inc).expect("a clean pair should phrase-switch");
        let beats = plan.fade_seconds / 0.5;
        assert!(
            beats > 32.0,
            "fade {:.2}s is only {beats:.0} beats; a clean pair should run long",
            plan.fade_seconds
        );
    }

    #[test]
    fn a_vocal_clash_shrinks_the_long_blend() {
        let mut out = matched(0.5, 200.0);
        let mut inc = matched(0.5, 200.0);
        let curve: Vec<audio_analysis::EnergyPoint> = (0..400)
            .map(|i| audio_analysis::EnergyPoint {
                time: i as f64 * 0.5,
                energy: 1.0,
            })
            .collect();
        let mask = vec![1.0f64; curve.len()];
        out.energy_curve = curve.clone();
        inc.energy_curve = curve;
        out.vocal_activity_mask = mask.clone();
        inc.vocal_activity_mask = mask;
        out.vocal_probability = 0.9;
        inc.vocal_probability = 0.9;
        let plan =
            phrase_switch(&out, &inc).expect("a clash shrinks the blend, it does not refuse it");
        let beats = plan.fade_seconds / 0.5;
        assert!(
            beats <= 8.0,
            "a vocal clash left the blend at {beats:.0} beats"
        );
    }

    #[test]
    fn next_energy_dip_detects_local_valley_in_window() {
        let curve = vec![
            super::audio_analysis::EnergyPoint {
                time: 0.0,
                energy: 0.9,
            },
            super::audio_analysis::EnergyPoint {
                time: 0.5,
                energy: 0.8,
            },
            super::audio_analysis::EnergyPoint {
                time: 1.0,
                energy: 0.25,
            }, // dip
            super::audio_analysis::EnergyPoint {
                time: 1.5,
                energy: 0.75,
            },
            super::audio_analysis::EnergyPoint {
                time: 2.0,
                energy: 0.85,
            },
        ];
        // At position 0.0, window is [0.1, 2.0]. Local minimum at 1.0s.
        let dip = find_next_energy_dip(&curve, 0.0);
        assert_eq!(dip, Some(1.0));

        // When flat: no dip found
        let flat_curve = vec![
            super::audio_analysis::EnergyPoint {
                time: 0.0,
                energy: 0.9,
            },
            super::audio_analysis::EnergyPoint {
                time: 0.5,
                energy: 0.91,
            },
            super::audio_analysis::EnergyPoint {
                time: 1.0,
                energy: 0.92,
            },
            super::audio_analysis::EnergyPoint {
                time: 1.5,
                energy: 0.90,
            },
        ];
        assert_eq!(find_next_energy_dip(&flat_curve, 0.0), None);
    }
    /// A 100 ms energy curve, the resolution the E heuristics read.
    fn with_energy(a: &mut Analysis, seconds: f64, energy: impl Fn(f64) -> f64) {
        let points = (0..((seconds / 0.1) as usize))
            .map(|index| {
                let time = index as f64 * 0.1;
                super::audio_analysis::EnergyPoint {
                    time,
                    energy: energy(time),
                }
            })
            .collect::<Vec<_>>();
        a.vocal_activity_mask = vec![0.0; points.len()];
        a.energy_curve = points;
    }

    fn edge_plan(end: f64, fade: f64, cue: f64) -> TransitionPlan {
        TransitionPlan {
            style: TransitionStyle::EqualPower,
            fade_seconds: fade,
            transition_end_seconds: end,
            cue_seconds: cue,
            ..Default::default()
        }
    }

    #[test]
    fn a_final_chord_is_struck_at_unity_after_a_held_phrase() {
        let mut out = analysis(0.0, 60.0, vec![0.0, 2.0, 4.0]);
        out.content_end = 60.0;
        // A quiet break, then the final chord at 57 s.
        with_energy(&mut out, 60.0, |time| if time < 57.0 { 0.08 } else { 0.8 });
        let mut inc = analysis(0.0, 40.0, vec![0.0, 2.0, 4.0]);
        with_energy(&mut inc, 40.0, |_| 0.8);
        let mut plan = edge_plan(60.0, 8.0, 0.0);
        respect_music_edges(&out, &inc, &mut plan);
        let chord = final_chord_attack(&out).expect("the fixture ends on a chord");
        let start = plan.transition_end_seconds - plan.fade_seconds;
        // The blend is a phrase plus the decay, and the outgoing is still at
        // unity when the chord is struck, not already falling.
        assert!(plan.fade_seconds >= 4.0, "fade {:.2}s is under the phrase", plan.fade_seconds);
        assert!((plan.outgoing_hold_seconds - 4.0).abs() < 1e-6);
        assert!((start + plan.outgoing_hold_seconds - chord).abs() < 1e-6);
        assert!((plan.transition_end_seconds - 60.0).abs() < 1e-6);
        record_transition_edges(&out, &inc, &plan);
        let edges = last_transition_edges();
        assert!(edges.cold_ending, "{edges:?}");
        assert!(!edges.clipped_final_chord, "{edges:?}");
    }

    /// Pair 5 of the real corpus, pinned as a shape: 143 BPM (a 3.36 s phrase)
    /// whose final chord is 1.5 s before the content end. E used to leave a
    /// 1.5 s blend here; E-2 keeps the phrase in the overlap and holds the
    /// outgoing until the chord instead.
    #[test]
    fn pair_five_keeps_its_phrase_and_holds_the_final_chord() {
        let mut out = analysis(0.0, 215.0, vec![0.0, 2.0, 4.0]);
        out.bpm = 143.0;
        out.beat_interval = 0.42;
        out.content_end = 215.0;
        with_energy(&mut out, 215.0, |time| if time <= 213.5 { 0.8 } else { 0.1 });
        let mut inc = analysis(0.0, 200.0, vec![0.0, 2.0, 4.0]);
        with_energy(&mut inc, 200.0, |_| 0.8);
        let mut plan = edge_plan(215.0, 3.36, 0.0);
        respect_music_edges(&out, &inc, &mut plan);
        let start = plan.transition_end_seconds - plan.fade_seconds;
        assert!(
            plan.fade_seconds >= 3.36 - 1e-6,
            "pair 5's overlap fell to {:.2}s",
            plan.fade_seconds
        );
        assert!((plan.fade_seconds - 4.86).abs() < 1e-6, "{:.2}s", plan.fade_seconds);
        assert!((plan.outgoing_hold_seconds - 3.36).abs() < 1e-6);
        assert!((start + plan.outgoing_hold_seconds - 213.5).abs() < 1e-6);
        record_transition_edges(&out, &inc, &plan);
        let edges = last_transition_edges();
        assert!(edges.cold_ending, "{edges:?}");
        assert!(!edges.clipped_final_chord, "{edges:?}");
    }

    #[test]
    fn a_cold_ending_keeps_its_effect_inside_the_decay() {
        let mut out = analysis(0.0, 215.0, vec![0.0, 2.0, 4.0]);
        out.bpm = 143.0;
        out.beat_interval = 0.42;
        out.content_end = 215.0;
        with_energy(&mut out, 215.0, |time| if time <= 213.5 { 0.8 } else { 0.1 });
        let mut inc = analysis(0.0, 200.0, vec![0.0, 2.0, 4.0]);
        with_energy(&mut inc, 200.0, |_| 0.8);
        let mut plan = edge_plan(215.0, 6.0, 0.0);
        plan.style = TransitionStyle::EchoOut;
        plan.effect_seconds = 3.0;
        plan.effect_start_seconds = 209.0;
        respect_music_edges(&out, &inc, &mut plan);
        let chord = final_chord_attack(&out).expect("the fixture ends on a chord");
        // The effect dresses the ending, not the phrase that leads into it.
        assert!((plan.effect_seconds - 1.5).abs() < 1e-6, "{:.2}s", plan.effect_seconds);
        assert!(plan.effect_start_seconds >= chord - 1e-6);
        assert!(
            (plan.effect_start_seconds + plan.effect_seconds - 215.0).abs() < 1e-3,
            "the effect no longer ends with the record"
        );
    }

    #[test]
    fn a_fade_out_is_not_treated_as_a_final_chord() {
        let mut out = analysis(0.0, 60.0, vec![0.0, 2.0, 4.0]);
        out.content_end = 60.0;
        with_energy(&mut out, 60.0, |time| {
            if time < 50.0 {
                0.8
            } else {
                0.8 * (1.0 - (time - 50.0) / 10.0).max(0.02)
            }
        });
        assert!(final_chord_attack(&out).is_none());
    }

    #[test]
    fn a_cue_past_a_vocal_entry_is_pulled_back() {
        let mut out = analysis(0.0, 60.0, vec![0.0, 2.0, 4.0]);
        with_energy(&mut out, 60.0, |_| 0.8);
        let mut inc = analysis(0.0, 40.0, vec![0.0, 2.0, 4.0]);
        with_energy(&mut inc, 40.0, |_| 0.8);
        for (index, point) in inc.energy_curve.iter().enumerate() {
            if point.time >= 4.0 {
                inc.vocal_activity_mask[index] = 0.8;
            }
        }
        let mut plan = edge_plan(60.0, 8.0, 6.0);
        respect_music_edges(&out, &inc, &mut plan);
        assert!(
            (plan.cue_seconds - 4.0).abs() < 0.05,
            "cue {} did not move to the entry",
            plan.cue_seconds
        );
    }

    fn test_grid(bpm: f64, bars: Vec<f64>, beats: Vec<f64>) -> crate::analyzer::beat::Grid {
        crate::analyzer::beat::Grid {
            beats,
            downbeats: bars,
            bpm,
            beat_interval: 60.0 / bpm,
            first_beat: 0.0,
            beat_confidence: 0.8,
        }
    }

    #[test]
    fn a_complete_model_grid_keeps_its_bars() {
        let bars: Vec<f64> = (0..=20).map(|i| i as f64 * 2.0).collect();
        let mut out = analysis(0.0, 40.0, bars.clone());
        out.bpm = 120.0;
        out.beat_interval = 0.5;
        let grid = test_grid(120.0, bars.clone(), (0..=80).map(|i| i as f64 * 0.5).collect());
        let (chosen, phrase_beat, source) = reconcile_grid(&out, grid);
        assert!(source.starts_with("model"), "{source}");
        assert_eq!(phrase_beat, 0.0);
        assert_eq!(chosen.downbeats, bars);
    }

    #[test]
    fn a_sparse_model_grid_takes_the_pll_bars() {
        let mut out = analysis(0.0, 40.0, (0..=20).map(|i| i as f64 * 2.0).collect());
        out.bpm = 120.0;
        out.beat_interval = 0.5;
        let model = test_grid(
            120.0,
            vec![0.0, 10.0, 20.0],
            (0..=80).map(|i| i as f64 * 0.5).collect(),
        );
        let (chosen, phrase_beat, source) = reconcile_grid(&out, model);
        assert!(source.starts_with("PLL"), "{source}");
        assert_eq!(chosen.downbeats.len(), 21);
        assert!((phrase_beat - 0.5).abs() < 1e-6);
        // The model's tempo and confidence survive for the tier.
        assert_eq!(chosen.bpm, 120.0);
        assert_eq!(chosen.beat_confidence, 0.8);
    }

    #[test]
    fn an_off_phase_model_shifts_the_pll_bars_by_a_beat() {
        let mut out = analysis(0.0, 40.0, (0..=20).map(|i| i as f64 * 2.0).collect());
        out.bpm = 120.0;
        out.beat_interval = 0.5;
        let model = test_grid(
            120.0,
            vec![0.5, 10.5, 20.5],
            (0..=80).map(|i| i as f64 * 0.5 + 0.5).collect(),
        );
        let (chosen, _, source) = reconcile_grid(&out, model);
        assert!(source.contains("shifted"), "{source}");
        assert!((chosen.downbeats[0] - 0.5).abs() < 1e-6, "{:?}", chosen.downbeats);
    }

    #[test]
    fn a_double_time_model_keeps_its_tempo_but_not_its_half_bars() {
        let mut out = analysis(0.0, 100.0, (0..=40).map(|i| i as f64 * 2.45).collect());
        out.bpm = 98.0;
        out.beat_interval = 0.612;
        let model = test_grid(
            196.0,
            (0..=60).map(|i| i as f64 * 1.225).collect(),
            (0..=200).map(|i| i as f64 * 0.306).collect(),
        );
        let (chosen, phrase_beat, source) = reconcile_grid(&out, model);
        assert!(source.starts_with("PLL"), "{source}");
        assert_eq!(chosen.bpm, 196.0, "the tier tempo must not change");
        assert!((phrase_beat - 2.45 / 4.0).abs() < 1e-6, "{phrase_beat}");
        // The phrase is now a musical 8 beats, not half of one.
        assert!((8.0 * phrase_beat - 4.9).abs() < 0.1);
    }

    /// F-4: a clean crossfade starts on the one. The end is untouched; the
    /// start moves to the nearest downbeat, and an even (phrase) bar beats a
    /// nearer odd one.
    #[test]
    fn a_clean_crossfade_starts_on_a_phrase_bar() {
        let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
        out.content_end = 100.0;
        // Last full-energy point 95.5 -> a 4.5 s fall, start 95.5.
        with_energy(&mut out, 100.0, |time| if time <= 95.5 { 0.8 } else { 0.1 });
        let plan = clean_crossfade(&out, 100.0, 0.0).expect("a crossfade");
        let start = plan.transition_end_seconds - plan.fade_seconds;
        assert!((start - 96.0).abs() < 1e-6, "start {start}");
        assert!((plan.fade_seconds - 4.0).abs() < 1e-6, "fade {}", plan.fade_seconds);
        assert!(plan.reason.contains("bar-aligned"), "{}", plan.reason);
    }

    #[test]
    fn a_clean_crossfade_may_snap_to_an_odd_bar_when_that_is_all_there_is() {
        let mut out = analysis(0.0, 100.0, vec![
            0.0, 20.0, 40.0, 60.0, 80.0, 85.0, 90.0, 95.0, 99.0, 103.0,
        ]);
        out.content_end = 100.0;
        with_energy(&mut out, 100.0, |time| if time <= 95.5 { 0.8 } else { 0.1 });
        let plan = clean_crossfade(&out, 100.0, 0.0).expect("a crossfade");
        let start = plan.transition_end_seconds - plan.fade_seconds;
        // 95 is the only downbeat inside the one-bar radius.
        assert!((start - 95.0).abs() < 1e-6, "start {start}");
        assert!((plan.fade_seconds - 5.0).abs() < 1e-6);
    }

    /// F: a vocal-free, phrase-aligned unit after the blend start is replayed
    /// once, and the record's end moves by exactly that unit.
    #[test]
    fn a_vocal_free_phrase_gets_one_bridge_unit() {
        let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
        out.beat_confidence = 0.8;
        out.content_end = 100.0;
        with_energy(&mut out, 100.0, |time| {
            if time < 80.0 {
                0.8
            } else {
                0.8 * (1.0 - (time - 80.0) / 20.0).max(0.02)
            }
        });
        for value in out.vocal_activity_mask.iter_mut() {
            *value = 0.1;
        }
        let mut plan = edge_plan(100.0, 8.0, 0.0);
        plan.reason = "crossfade: test".into();
        apply_bridge(&out, &mut plan);
        assert!((plan.bridge_start_seconds - 96.0).abs() < 1e-6, "{:?}", plan);
        assert!((plan.bridge_seconds - 4.0).abs() < 1e-6, "{:?}", plan);
        assert_eq!(plan.bridge_repeats, 1);
        assert!((plan.transition_end_seconds - 104.0).abs() < 1e-6);
        assert!((plan.fade_seconds - 12.0).abs() < 1e-6);
        assert_eq!(plan.outgoing_hold_seconds, 0.0, "an ordinary blend has no hold");
        assert!(plan.reason.contains("bridge: 2 bars"), "{}", plan.reason);
    }

    /// A bridged fade may run past the ordinary 12 s ceiling, up to 16 s.
    #[test]
    fn a_bridge_may_take_the_fade_to_sixteen_seconds() {
        let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
        out.beat_confidence = 0.8;
        out.content_end = 100.0;
        with_energy(&mut out, 100.0, |time| {
            if time < 80.0 {
                0.8
            } else {
                0.8 * (1.0 - (time - 80.0) / 20.0).max(0.02)
            }
        });
        for value in out.vocal_activity_mask.iter_mut() {
            *value = 0.1;
        }
        // 10.5 s base + a 4 s unit = 14.5 s, which the old 12 s cap refused.
        let mut plan = edge_plan(100.0, 10.5, 0.0);
        plan.reason = "crossfade: test".into();
        apply_bridge(&out, &mut plan);
        assert!((plan.bridge_seconds - 4.0).abs() < 1e-6, "{}", plan.reason);
        assert!((plan.fade_seconds - 14.5).abs() < 1e-6, "{}", plan.reason);
        assert!(plan.fade_seconds <= 16.0);
    }

    /// A bridge on a cold ending sits before the chord and grows the hold by
    /// the unit, so the delayed chord is still struck at unity.
    #[test]
    fn a_cold_ending_bridges_before_its_chord() {
        let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
        out.beat_confidence = 0.8;
        out.content_end = 98.0;
        with_energy(&mut out, 98.0, |time| if time <= 96.0 { 0.8 } else { 0.1 });
        for value in out.vocal_activity_mask.iter_mut() {
            *value = 0.1;
        }
        let mut inc = analysis(0.0, 80.0, vec![0.0, 2.0, 4.0]);
        with_energy(&mut inc, 80.0, |_| 0.8);
        let mut plan = edge_plan(98.0, 8.0, 0.0);
        plan.reason = "crossfade: test".into();
        respect_music_edges(&out, &inc, &mut plan);
        apply_bridge(&out, &mut plan);
        let start = plan.transition_end_seconds - plan.fade_seconds;
        let chord = final_chord_attack(&out).expect("the fixture ends on a chord");
        assert!(plan.bridge_seconds > 0.0, "{}", plan.reason);
        assert!(plan.bridge_start_seconds + plan.bridge_seconds <= chord + 1e-6);
        // The chord is delayed by the unit and the hold grew with it.
        assert!(
            (start + plan.outgoing_hold_seconds - (chord + plan.bridge_seconds)).abs() < 1e-6,
            "the delayed chord is not the start of the fall: {start} + {} vs {}",
            plan.outgoing_hold_seconds,
            chord + plan.bridge_seconds
        );
    }

    #[test]
    fn a_sung_unit_gets_no_bridge() {
        let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
        out.beat_confidence = 0.8;
        out.content_end = 100.0;
        with_energy(&mut out, 100.0, |time| {
            if time < 80.0 {
                0.8
            } else {
                0.8 * (1.0 - (time - 80.0) / 20.0).max(0.02)
            }
        });
        for value in out.vocal_activity_mask.iter_mut() {
            *value = 0.8;
        }
        let mut plan = edge_plan(100.0, 8.0, 0.0);
        plan.reason = "crossfade: test".into();
        apply_bridge(&out, &mut plan);
        assert_eq!(plan.bridge_repeats, 0);
        assert!(plan.reason.contains("bridge: none (the unit has a voice)"), "{}", plan.reason);
    }

    #[test]
    fn an_irregular_bar_grid_gets_no_bridge() {
        let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
        out.beat_confidence = 0.8;
        out.content_end = 100.0;
        // The model's confidence is no longer the gate: its bars are.
        for (index, bar) in out.downbeats.iter_mut().enumerate() {
            *bar += (index % 3) as f64 * 0.2;
        }
        with_energy(&mut out, 100.0, |time| {
            if time < 80.0 {
                0.8
            } else {
                0.8 * (1.0 - (time - 80.0) / 20.0).max(0.02)
            }
        });
        for value in out.vocal_activity_mask.iter_mut() {
            *value = 0.1;
        }
        let mut plan = edge_plan(100.0, 8.0, 0.0);
        plan.reason = "crossfade: test".into();
        apply_bridge(&out, &mut plan);
        assert_eq!(plan.bridge_repeats, 0);
        assert!(
            plan.reason.contains("bridge: none"),
            "{}",
            plan.reason
        );
    }

    #[test]
    fn a_drifting_unit_is_skipped_for_an_aligned_one() {
        let mut out = analysis(0.0, 100.0, (0..=50).map(|i| i as f64 * 2.0).collect());
        out.beat_confidence = 0.8;
        out.content_end = 100.0;
        // The last unit's middle downbeat is 40 ms late; an earlier unit is
        // still on the grid.
        out.downbeats[49] = 98.04;
        with_energy(&mut out, 100.0, |time| {
            if time < 80.0 {
                0.8
            } else {
                0.8 * (1.0 - (time - 80.0) / 20.0).max(0.02)
            }
        });
        for value in out.vocal_activity_mask.iter_mut() {
            *value = 0.1;
        }
        let mut plan = edge_plan(100.0, 8.0, 0.0);
        plan.reason = "crossfade: test".into();
        apply_bridge(&out, &mut plan);
        assert!(plan.bridge_seconds > 0.0, "{}", plan.reason);
        assert!(plan.bridge_start_seconds <= 92.0 + 1e-6, "{:?}", plan);
    }

    #[test]
    fn a_cold_ending_ringing_past_the_plan_end_is_kept() {
        let mut out = analysis(0.0, 60.0, vec![0.0, 2.0, 4.0]);
        out.content_end = 50.0;
        with_energy(&mut out, 50.0, |time| if time < 44.0 { 0.08 } else { 0.8 });
        let mut inc = analysis(0.0, 40.0, vec![0.0, 2.0, 4.0]);
        with_energy(&mut inc, 40.0, |_| 0.8);
        let mut plan = edge_plan(49.0, 6.0, 0.0);
        respect_music_edges(&out, &inc, &mut plan);
        assert!((plan.transition_end_seconds - 50.0).abs() < 1e-6);
        let chord = final_chord_attack(&out).expect("the fixture ends on a chord");
        let start = plan.transition_end_seconds - plan.fade_seconds;
        assert!(plan.fade_seconds >= 4.0);
        assert!((start + plan.outgoing_hold_seconds - chord).abs() < 1e-6);
    }

}
