import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
#if os(iOS)
import UIKit
#else
import AppKit
#endif

struct SettingsPlaybackPersist: ViewModifier {
    var controller: PlaybackController
    @Binding var crossfade: Int
    @Binding var spatial: Bool
    @Binding var automix: Bool
    @Binding var automixSequence: Bool
    @Binding var automixPerf: String
    @Binding var skipSilence: Bool
    @Binding var playbackSpeed: Double
    /// Raised when Automix goes on. The models are what make Automix's timing real,
    /// so switching it on with none installed is the one moment the question is
    /// worth asking again.
    var onAutomixEnabled: () -> Void = {}

    func body(content: Content) -> some View {
        content
            .onChange(of: crossfade) { _, value in
                AppSettings.shared.setCrossfadeSeconds(value: Swift.Int32(value))
                controller.updateCrossfade(seconds: value)
            }
            .onChange(of: spatial) { _, value in
                AppSettings.shared.setSpatialAudio(value: value)
                controller.updateSpatial(enabled: value)
            }
            .onChange(of: automix) { _, value in
                AppSettings.shared.setSmartFadeEnabled(value: value)
                controller.setAutomixEnabled(value)
                if value { onAutomixEnabled() }
            }
            .onChange(of: automixSequence) { _, value in
                PlatformSettings.shared.putBoolean(key: "automix_smart_sequence", value: value)
                controller.smartSequencingPreferenceChanged()
            }
            .onChange(of: automixPerf) { _, value in
                controller.setAutomixPerformanceMode(value)
            }
            .onChange(of: skipSilence) { _, value in
                AppSettings.shared.setSkipSilence(value: value)
                controller.updateSkipSilence(enabled: value)
            }
            .onChange(of: playbackSpeed) { _, value in
                AppSettings.shared.setPlaybackSpeed(value: Float(value))
                controller.updateSpeed(Float(value))
            }
    }
}

struct SettingsQualityPersist: ViewModifier {
    var controller: PlaybackController
    @Binding var wifiQuality: String
    @Binding var cellQuality: String
    @Binding var downloadQuality: String
    @Binding var wifiOnlyDownloads: Bool
    @Binding var cacheLimitMB: Int
    @Binding var outputPcm: String
    @Binding var matchSourceRate: Bool
    @Binding var bitPerfect: Bool
    @Binding var preferUsbDac: Bool
    @Binding var loudness: Bool
    @Binding var loudnessModeSelection: String

    func body(content: Content) -> some View {
        content
            .onChange(of: wifiQuality) { _, value in AppSettings.shared.setAudioQualityWifi(value: value) }
            .onChange(of: cellQuality) { _, value in AppSettings.shared.setAudioQualityCellular(value: value) }
            .onChange(of: downloadQuality) { _, value in AppSettings.shared.setDownloadQuality(value: value) }
            .onChange(of: wifiOnlyDownloads) { _, value in
                AppSettings.shared.setWifiOnlyDownloads(value: value)
                DownloadStore.shared.networkPolicyChanged()
            }
            .onChange(of: outputPcm) { _, value in controller.updateOutputPcmMode(value) }
            .onChange(of: matchSourceRate) { _, value in controller.updateMatchSourceSampleRate(value) }
            .onChange(of: bitPerfect) { _, value in controller.updateBitPerfectOutput(value) }
            .onChange(of: preferUsbDac) { _, value in controller.updatePreferUsbDac(value) }
            .onChange(of: loudness) { _, value in
                controller.updateLoudnessMode(value ? loudnessModeSelection : "OFF")
            }
            .onChange(of: loudnessModeSelection) { _, value in
                if loudness { controller.updateLoudnessMode(value) }
            }
            .onChange(of: cacheLimitMB) { _, value in
                AppSettings.shared.setAudioCacheLimitBytes(value: Swift.Int64(value) * 1024 * 1024)
                Task { await StreamFileCache.shared.trim() }
            }
    }
}

struct SettingsExperiencePersist: ViewModifier {
    var controller: PlaybackController
    var appModel: AppModel
    @Binding var nerdStats: Bool
    @Binding var canvas: Bool
    @Binding var canvasCellular: Bool
    @Binding var reduceBlur: Bool
    @Binding var reduceAnimation: Bool
    @Binding var fullBleed: Bool
    @Binding var syncedLyrics: Bool
    @Binding var convertVideo: Bool
    @Binding var swipeNext: Bool
    @Binding var dontRepeat: Bool
    @Binding var hideVolume: Bool
    @Binding var legacyMesh: Bool
    @Binding var preferMusicOnly: Bool
    @Binding var theme: String

    func body(content: Content) -> some View {
        content
            .onChange(of: nerdStats) { _, value in AppSettings.shared.setShowNerdStats(value: value) }
            .onChange(of: canvas) { _, value in
                AppSettings.shared.setAnimatedCanvas(value: value)
                controller.refreshCanvasLookup()
            }
            .onChange(of: canvasCellular) { _, value in
                AppSettings.shared.setCanvasOverCellular(value: value)
                controller.refreshCanvasLookup()
            }
            .onChange(of: reduceBlur) { _, value in AppSettings.shared.setReduceDynamicBlur(value: value) }
            .onChange(of: reduceAnimation) { _, value in AppSettings.shared.setReduceAnimation(value: value) }
            .onChange(of: fullBleed) { _, value in AppSettings.shared.setFullBleedArtwork(value: value) }
            .onChange(of: legacyMesh) { _, value in PlatformSettings.shared.putBoolean(key: "legacy_mesh_gradient", value: value) }
            .onChange(of: preferMusicOnly) { _, value in controller.updatePreferMusicOnly(value) }
            .onChange(of: syncedLyrics) { _, value in AppSettings.shared.setSyncedLyrics(value: value) }
            .onChange(of: convertVideo) { _, value in AppSettings.shared.setConvertVideoToAudio(value: value) }
            .onChange(of: swipeNext) { _, value in AppSettings.shared.setSwipeToPlayNext(value: value) }
            .onChange(of: dontRepeat) { _, value in AppSettings.shared.setDontRepeatSuggestions(value: value) }
            .onChange(of: hideVolume) { _, value in
                AppSettings.shared.setHideVolumeBar(value: value)
                controller.hideVolumeBar = value
            }
            .onChange(of: theme) { _, value in
                AppSettings.shared.setThemeMode(value: value)
                appModel.themeMode = value
            }
    }
}

struct SettingsExtrasPersist: ViewModifier {
    @Binding var lyricsSources: String
    @Binding var lyricsSourceOrder: String
    @Binding var stopBackground: Bool
    @Binding var syllableSync: Bool
    @Binding var language: String
    @Binding var spotifyCookie: String
    @Binding var replayGenres: Bool
    var appModel: AppModel

    func body(content: Content) -> some View {
        content
            .onChange(of: lyricsSources) { _, value in AppSettings.shared.setLyricsSources(value: value) }
            .onChange(of: lyricsSourceOrder) { _, value in AppSettings.shared.setLyricsSourceOrder(value: value) }
            .onChange(of: stopBackground) { _, value in AppSettings.shared.setStopWhenBackgrounded(value: value) }
            .onChange(of: syllableSync) { _, value in AppSettings.shared.setPrioritizeSyllableSync(value: value) }
            .onChange(of: language) { _, value in
                PlatformSettings.shared.putString(key: "app_language", value: value)
                appModel.appLanguage = value
            }
            .onChange(of: spotifyCookie) { _, value in AppSettings.shared.setSpotifySpdc(value: value) }
            .onChange(of: replayGenres) { _, value in AppSettings.shared.setReplayGenres(value: value) }
    }
}

/// Persists the settings that live outside the classic groups: the Advanced
/// Options section, the Local Music filter, the Downloads export switch, the
/// song-status caption, and the Performance section.
struct SettingsAdvancedPersist: ViewModifier {
    var controller: PlaybackController
    @Binding var smartAlign: Bool
    @Binding var hideSongStatus: Bool
    @Binding var exportDownloads: Bool
    @Binding var filterNonMusic: Bool
    @Binding var highPerf: Bool
    @Binding var refreshRate: Int
    @Binding var reduceAnimation: Bool
    @Binding var reduceBlur: Bool

    func body(content: Content) -> some View {
        content
            .onChange(of: smartAlign) { _, value in controller.setSmartVersionAlignment(value) }
            .onChange(of: hideSongStatus) { _, value in controller.updateHideSongStatus(value) }
            // The read side is `DownloadStore.folder`: on, downloads land in the
            // shared Music/BitChord folder where other music apps can see them;
            // off, they stay in BitChord's own storage.
            .onChange(of: exportDownloads) { _, value in PlatformSettings.shared.putBoolean(key: "export_downloads", value: value) }
            // The read side is the local-library scan, which drops rows that
            // fail `LocalMusicEligibility` while this is on.
            .onChange(of: filterNonMusic) { _, value in PlatformSettings.shared.putBoolean(key: "filter_non_music_audio", value: value) }
            .onChange(of: highPerf) { _, value in
                PlatformSettings.shared.putBoolean(key: "high_performance_mode", value: value)
                if value {
                    // Upstream's rule: a sustained high refresh rate and frozen
                    // gradients are opposites, so enabling this unfreezes both.
                    // Each write flows through its own persist above.
                    reduceAnimation = false
                    reduceBlur = false
                }
                PerformanceTuner.shared.apply(highPerformance: value, refreshRateHz: refreshRate)
            }
            .onChange(of: refreshRate) { _, value in
                PlatformSettings.shared.putInt(key: "performance_refresh_rate", value: Swift.Int32(value))
                PerformanceTuner.shared.apply(highPerformance: highPerf, refreshRateHz: value)
            }
    }
}
