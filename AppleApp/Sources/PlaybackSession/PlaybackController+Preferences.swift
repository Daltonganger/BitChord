import Foundation
import BitChordShared
import os
#if os(iOS)
import UIKit
#if DEBUG
import AVFoundation
#endif
#else
import AppKit
#endif

// Sound and output preferences applied to the engine.
extension PlaybackController {
    func updateCrossfade(seconds: Int) {
        try? engine.setCrossfadeWindow(seconds: Double(seconds))
    }

    func updateSpatial(enabled: Bool) {
        applySpatialPreference(enabled: enabled)
    }

    func applySpatialPreference(enabled: Bool? = nil) {
        let requested = enabled ?? PlatformSettings.shared.getBoolean(key: "spatial_audio", default: false)
        let effective = requested && dolbyRenderer == nil && AudioRouteState.shared.permitsCustomSpatial
        try? engine.setSpatialEnabled(enabled: effective)
        if effective && soundMode == "ENHANCED" { headTracker.start(engine: engine) }
        else { headTracker.stop() }
    }

    func updateSpeed(_ speed: Float) {
        let sampledPosition = isPlaying ? playbackPosition : position
        let sampledAt = Date()
        dolbyRenderer?.rate = speed
        if isPlaying { dolbyRenderer?.play() }
        try? engine.setPlaybackSpeed(speed: speed)
        let rate = Double(speed)
        playbackRate = rate.isFinite ? min(max(rate, 0.5), 2.0) : 1.0
        position = sampledPosition
        positionSampledAt = sampledAt
        nowPlaying.updateRate(isPlaying ? Double(speed) : 0, position: sampledPosition)
    }

    func updateSkipSilence(enabled: Bool) {
        try? engine.setSkipSilence(enabled: enabled)
    }

    /// Inference threads for the Automix analysis tier: 1 / 2 / 4, upstream's
    /// own numbers for EFFICIENT / BALANCED / PERFORMANCE.
    var automixInferenceThreads: Int {
        switch automixPerformanceMode {
        case "EFFICIENT": return 1
        case "PERFORMANCE": return 4
        default: return 2
        }
    }

    func refreshPlaybackRegions() {
        for task in regionTasks.values { task.cancel() }; regionTasks = [:]; preparedRegionSources = []
        if let source = loadedSourcePath { prepareRegions(source: source, videoId: resolvedVideoIds[source] ?? DownloadStore.shared.provenance(for: source)) }
        syncEngineQueueNext()
    }

    func prepareRegions(source: String, videoId: String?) {
        guard !preparedRegionSources.contains(source), regionTasks[source] == nil else { return }
        let generation = playGeneration, revision = queueNextRevision
        let trim = automixEnabled || PlatformSettings.shared.getBoolean(key: "trim_edge_silence", default: false)
        let skip = automixEnabled || PlatformSettings.shared.getBoolean(key: "skip_non_music", default: false)
        let engine = self.engine
        regionTasks[source] = Task { [weak self] in
            for _ in 0..<300 {
                let regions = await PlaybackRegionStore.shared.regions(path: source, videoId: videoId, trimEdges: trim, skipSegments: skip)
                guard !Task.isCancelled, let self, self.playGeneration == generation,
                      self.loadedSourcePath == source || self.queueNextRevision == revision else { break }
                self.regionEdgesBySource[source] = (regions.audibleStartSeconds, regions.audibleEndSeconds)
                try? engine.setPlaybackRegions(source: source, regions: regions)
                let complete = !FileManager.default.fileExists(atPath: source + ".grow") || FileManager.default.fileExists(atPath: source + ".complete")
                if complete || !trim {
                    self.preparedRegionSources.insert(source); self.regionTasks[source] = nil
                    self.debugLog.record("playback regions start=\(regions.audibleStartSeconds) end=\(String(describing: regions.audibleEndSeconds)) excluded=\(regions.excluded.count)")
                    // The mixer updates waiting plans without discarding armed voices.
                    return
                }
                try? await Task.sleep(for: .seconds(1))
            }
            self?.regionTasks[source] = nil
        }
    }

    func scheduleLoudnessMeasurement() {
        loudnessMeasurementTask?.cancel()
        guard soundMode == "ENHANCED", loudnessMode != "OFF", let path = loadedSourcePath,
              !path.hasPrefix("http:"), !path.hasPrefix("https:") else { return }
        let engine = self.engine
        loudnessMeasurementTask = Task {
            do {
                // Partial downloads cannot yield a whole-recording loudness.
                for _ in 0..<300 {
                    try Task.checkCancellation()
                    if !FileManager.default.fileExists(atPath: path + ".grow") || FileManager.default.fileExists(atPath: path + ".complete") { break }
                    try await Task.sleep(for: .seconds(1))
                }
                try Task.checkCancellation()
                let cached = await StreamFileCache.shared.measuredLoudness(at: path)
                let measurement: LoudnessMeasurement
                if let cached { measurement = cached }
                else {
                    measurement = try await Task.detached(priority: .utility) { try engine.measureLoudness(source: path) }.value
                    await StreamFileCache.shared.storeMeasurement(measurement, at: path)
                }
                try Task.checkCancellation()
                // The native command discards this result if the source changed.
                try engine.setLoudnessMeasurement(source: path, measurement: measurement)
            } catch is CancellationError { }
            catch { NSLog("[BitChord] loudness measurement unavailable: \(error)") }
        }
    }

    nonisolated static func storedClarityTuning() -> ClarityTuning {
        let raw = PlatformSettings.shared.getString(key: "clarity_preset", default: "REFERENCE")
        let preset: ClarityPreset = raw == "SPEAKER" ? .speaker : raw == "HEADPHONE" ? .headphone : raw == "DAC" ? .dac : .reference
        let trims = PlatformSettings.shared.getString(key: "clarity_trims", default: "").split(separator: ",").compactMap { Float($0) }
        return ClarityTuning(preset: preset, wet: PlatformSettings.shared.getFloat(key: "clarity_wet", default: 1), trimsDb: trims.count == 8 ? trims : Array(repeating: 0, count: 8))
    }

    nonisolated static func storedLoudnessMode() -> LoudnessMode {
        let legacy = PlatformSettings.shared.getBoolean(key: "loudness_normalization", default: false)
        let raw = PlatformSettings.shared.getString(key: "loudness_mode", default: legacy ? "TRACK" : "OFF")
        return raw == "ALBUM" ? .album : raw == "TRACK" ? .track : .off
    }

    func updateSoundMode(_ value: String) {
        soundMode = value == "ENHANCED" ? "ENHANCED" : "TRANSPARENT"
        PlatformSettings.shared.putString(key: "sound_mode", value: soundMode)
        try? engine.setSoundMode(mode: soundMode == "ENHANCED" ? .enhanced : .transparent)
        applySpatialPreference()
        scheduleLoudnessMeasurement()
    }

    func updateClarity(preset: String? = nil, wet: Double? = nil) {
        if let preset { clarityPreset = preset; PlatformSettings.shared.putString(key: "clarity_preset", value: preset) }
        if let wet { clarityWet = min(max(wet, 0), 1); PlatformSettings.shared.putFloat(key: "clarity_wet", value: Float(clarityWet)) }
        try? engine.setClarityTuning(tuning: Self.storedClarityTuning())
    }

    func updateLoudnessMode(_ value: String) {
        loudnessMode = ["TRACK", "ALBUM"].contains(value) ? value : "OFF"
        loudnessNormalization = loudnessMode != "OFF"
        PlatformSettings.shared.putString(key: "loudness_mode", value: loudnessMode)
        PlatformSettings.shared.putBoolean(key: "loudness_normalization", value: loudnessNormalization)
        try? engine.setLoudnessMode(mode: Self.storedLoudnessMode())
        scheduleLoudnessMeasurement()
    }

    func updateOutputPcmMode(_ mode: String) {
        let valid = Self.migrateOutputPcmMode(mode)
        outputPcmMode = valid
        // PlatformSettings directly: the typed shared setter only exists in
        // source until the shared framework is rebuilt (see AppSettings.kt).
        PlatformSettings.shared.putString(key: "output_pcm_mode", value: valid)
        try? engine.setOutputPcmMode(mode: valid)
    }

    func updateBitPerfectOutput(_ enabled: Bool) {
        bitPerfectOutput = enabled
        PlatformSettings.shared.putBoolean(key: "bit_perfect_output", value: enabled)
        try? engine.setBitPerfectEnabled(enabled: enabled)
        if enabled, isPlaying, current != nil {
            restartCurrent(keepingPosition: true)
        }
    }

    func updateMatchSourceSampleRate(_ enabled: Bool) {
        matchSourceSampleRate = enabled
        PlatformSettings.shared.putBoolean(key: "match_source_sample_rate", value: enabled)
        if isPlaying, current != nil {
            restartCurrent(keepingPosition: true)
        }
    }

    /// The stored PCM mode with the removed rung migrated away.
    ///
    /// Upstream's enum never had PCM_24 (Media3's sink has no packed-24 path);
    /// the three-way picker did. Anything stored from then maps to the
    /// lossless path rather than the lossy one.
    nonisolated static func migrateOutputPcmMode(_ mode: String? = nil) -> String {
        #if os(iOS)
        // iOS CoreAudio RemoteIO operates exclusively with 32-bit float streams.
        // Normalize any persisted legacy value so all layers agree.
        let currentStored = PlatformSettings.shared.getString(key: "output_pcm_mode", default: "FLOAT_32")
        if currentStored != "FLOAT_32" {
            PlatformSettings.shared.putString(key: "output_pcm_mode", value: "FLOAT_32")
        }
        return "FLOAT_32"
        #else
        let stored = mode ?? PlatformSettings.shared.getString(key: "output_pcm_mode", default: "FLOAT_32")
        let migrated: String
        switch stored {
        case "PCM_16": migrated = "PCM_16"
        case "FLOAT_32", "PCM_24": migrated = "FLOAT_32"
        default: migrated = "FLOAT_32"
        }
        // Keep the supported PCM16 selection; only the removed PCM24 rung is
        // migrated to float so the setting, engine and pipeline agree.
        if migrated != stored, mode == nil {
            PlatformSettings.shared.putString(key: "output_pcm_mode", value: migrated)
        }
        return migrated
        #endif
    }

    func updatePreferUsbDac(_ enabled: Bool) {
        preferUsbDac = enabled
        PlatformSettings.shared.putBoolean(key: "prefer_usb_dac", value: enabled)
        try? engine.setPreferUsbDac(enabled: enabled)
    }

    func updateLoudnessNormalization(_ enabled: Bool) {
        updateLoudnessMode(enabled ? "TRACK" : "OFF")
    }

    func setAutomixPerformanceMode(_ mode: String) {
        let valid = mode == "EFFICIENT" || mode == "PERFORMANCE" ? mode : "BALANCED"
        automixPerformanceMode = valid
        PlatformSettings.shared.putString(key: "automix_performance", value: valid)
        engine.setAutomixPerformance(mode: valid)
    }

    func setAutomixPhraseBars(_ bars: Int) {
        let valid = bars == 8 || bars == 16 ? bars : 0
        automixPhraseBars = valid
        PlatformSettings.shared.putInt(key: "automix_phrase_bars", value: Int32(valid))
        engine.setAutomixPhraseBars(bars: UInt32(valid))
    }

    func setSmartVersionAlignment(_ enabled: Bool) {
        smartVersionAlignment = enabled
        PlatformSettings.shared.putBoolean(key: "smart_version_alignment", value: enabled)
    }

    func updateHideSongStatus(_ hidden: Bool) {
        hideSongStatus = hidden
        PlatformSettings.shared.putBoolean(key: "hide_song_status", value: hidden)
    }

    func updatePreferMusicOnly(_ enabled: Bool) {
        preferMusicOnly = enabled
        PlatformSettings.shared.putBoolean(key: "prefer_music_only", value: enabled)
    }

    func updateScrobblePrimaryArtist(_ mode: String) {
        let valid = mode == "album" ? "album" : "track"
        scrobblePrimaryArtist = valid
        PlatformSettings.shared.putString(key: "scrobble_primary_artist", value: valid)
    }

    /// The artist a scrobble credits for `entry`.
    ///
    /// "track" sends the full credit as the catalogue gave it. "album" sends
    /// the lead name alone — upstream `PrimaryArtist`, which splits on ",",
    /// "&" and "＆" and takes the first part, so "A, B & C" scrobbles as "A".
    func scrobbleArtist(for entry: QueueEntry) -> String {
        guard scrobblePrimaryArtist == "album" else { return entry.artist }
        let commaFirst = entry.artist.components(separatedBy: ",").first ?? entry.artist
        let ampSplit = commaFirst.components(separatedBy: " & ").first ?? commaFirst
        let fullWidthSplit = ampSplit.components(separatedBy: " ＆ ").first ?? ampSplit
        let lead = fullWidthSplit.trimmingCharacters(in: .whitespaces)
        return lead.isEmpty ? entry.artist : lead
    }

    /// Renders the equaliser settings into the ten-slot curve and hands it to
    /// the engine (upstream `PlaybackService.applyEqualizer`). The make-up
    /// preamp is computed inside the engine, never here.
    func applyEqualizer() {
        let tuning = Self.currentEqTuning()
        try? engine.setEqTuning(
            enabled: tuning.enabled,
            gainsDb: tuning.gains,
            qs: tuning.qs,
            balance: tuning.balance
        )
    }

    /// The equaliser settings as the engine's ten-slot tuning. Reads only
    /// `PlatformSettings`, so it is safe from the engine-startup task.
    nonisolated static func currentEqTuning() -> (enabled: Bool, gains: [Float], qs: [Float], balance: Float) {
        let enabled = PlatformSettings.shared.getBoolean(key: "equalizer_enabled", default: false)
        let dynamic = PlatformSettings.shared.getString(key: "equalizer_mode", default: "Dynamic") == "Dynamic"
        let bands = EqualizerTuning.loadBands()
        let toneX = Int(PlatformSettings.shared.getInt(key: "equalizer_tone_x", default: 0))
        let toneY = Int(PlatformSettings.shared.getInt(key: "equalizer_tone_y", default: 0))
        let focused = PlatformSettings.shared.getBoolean(key: "equalizer_focused", default: false)
        let balance = PlatformSettings.shared.getFloat(key: "equalizer_balance", default: 0)
        let curve = dynamic
            ? EqualizerTuning.toneCurve(x: toneX, y: toneY, focused: focused)
            : EqualizerTuning.manualCurve(bands)
        return (
            enabled: enabled,
            gains: curve.gains.map { Float($0) },
            qs: curve.qs.map { Float($0) },
            balance: balance
        )
    }
}
