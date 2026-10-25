import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// Apple Settings-style grouped form. Structure and copy follow upstream;
/// settings appear directly in one grouped form, with drill-downs for dedicated screens.
struct SettingsView: View {
    var embedded: Bool = false
    @Environment(\.dismiss) private var dismiss
    @Environment(PlaybackController.self) var controller
    @Environment(AuthController.self) var auth
    @Environment(AppModel.self) var appModel

    @State var crossfade = Int(PlatformSettings.shared.getInt(key: "crossfade_seconds", default: 0))
    @AppStorage("dolby_atmos") var dolbyAtmos = true
    #if os(macOS)
    @AppStorage("menu_bar_player") var menuBarPlayer = true
    #endif
    @State var spatial = PlatformSettings.shared.getBoolean(key: "spatial_audio", default: false)
    @State var automix = PlatformSettings.shared.getBoolean(key: "smart_fade_enabled", default: true)
    @State var automixSequence = PlatformSettings.shared.getBoolean(key: "automix_smart_sequence", default: true)
    @State var trimEdges = PlatformSettings.shared.getBoolean(key: "trim_edge_silence", default: false)
    @State var skipNonMusic = PlatformSettings.shared.getBoolean(key: "skip_non_music", default: false)
    @State var skipSilence = PlatformSettings.shared.getBoolean(key: "skip_silence", default: false)
    @State var wifiQuality = PlatformSettings.shared.getString(key: "audio_quality_wifi", default: "LOSSLESS")
    @State var cellQuality = PlatformSettings.shared.getString(key: "audio_quality_cellular", default: "LOSSLESS")
    @State var downloadQuality = PlatformSettings.shared.getString(key: "download_quality", default: "LOSSLESS")
    @State var wifiOnlyDownloads = PlatformSettings.shared.getBoolean(key: "wifi_only_downloads", default: true)
    @State var nerdStats = PlatformSettings.shared.getBoolean(key: "show_nerd_stats", default: false)
    @State var canvas = PlatformSettings.shared.getBoolean(key: "animated_canvas", default: true)
    @State var canvasCellular = PlatformSettings.shared.getBoolean(key: "canvas_over_cellular", default: false)
    @State var reduceBlur = PlatformSettings.shared.getBoolean(key: "reduce_dynamic_blur", default: false)
    @State var update = UpdateChecker.shared
    /// The release being shown, and nil when nothing is.
    @State var updateSheet: AppReleaseUpdate?
    @State var reduceAnimation = PlatformSettings.shared.getBoolean(key: "reduce_animation", default: false)
    @State var fullBleed = PlatformSettings.shared.getBoolean(key: "full_bleed_artwork", default: true)
    @AppStorage("lyrics_blur") var lyricsBlur = true
    @AppStorage("show_music_videos") var showMusicVideos = false
    @AppStorage("translation_language") var translationLanguage = ""
    @State var syncedLyrics = PlatformSettings.shared.getBoolean(key: "synced_lyrics", default: true)
    @State var convertVideo = PlatformSettings.shared.getBoolean(key: "convert_video_to_audio", default: true)
    @State var swipeNext = PlatformSettings.shared.getBoolean(key: "swipe_to_play_next", default: false)
    @State var dontRepeat = PlatformSettings.shared.getBoolean(key: "dont_repeat_suggestions", default: false)
    @State var hideVolume = PlatformSettings.shared.getBoolean(key: "hide_volume_bar", default: false)
    @State var hideSongStatus = PlatformSettings.shared.getBoolean(key: "hide_song_status", default: false)
    @State var outputPcm = PlaybackController.migrateOutputPcmMode()
    @State var matchSourceRate = PlatformSettings.shared.getBoolean(key: "match_source_sample_rate", default: true)
    @State var bitPerfect = PlatformSettings.shared.getBoolean(key: "bit_perfect_output", default: false)
    @State var preferUsbDac = PlatformSettings.shared.getBoolean(key: "prefer_usb_dac", default: false)
    @State var loudness = PlatformSettings.shared.getBoolean(key: "loudness_normalization", default: false)
    @State var loudnessModeSelection = PlatformSettings.shared.getString(key: "loudness_mode", default: "TRACK") == "ALBUM" ? "ALBUM" : "TRACK"
    @State var loudnessOffOnSpeaker = PlatformSettings.shared.getBoolean(key: "loudness_off_on_speaker", default: true)
    @State var automixPerf = PlatformSettings.shared.getString(key: "automix_performance", default: "BALANCED")
    @State var highPerf = PlatformSettings.shared.getBoolean(key: "high_performance_mode", default: false)
    @State var refreshRate: Int = Int(PlatformSettings.shared.getInt(key: "performance_refresh_rate", default: 60))
    @State var showPerfWarning = false
    @State var localFolderName: String? = SettingsView.storedLocalFolderName()
    @State var pickingFolder = false
    @State var filterNonMusic = PlatformSettings.shared.getBoolean(key: "filter_non_music_audio", default: true)
    @State var exportDownloads = PlatformSettings.shared.getBoolean(key: "export_downloads", default: false)
    @State var smartAlign = PlatformSettings.shared.getBoolean(key: "smart_version_alignment", default: true)
    @State var legacyMesh = PlatformSettings.shared.getBoolean(key: "legacy_mesh_gradient", default: false)
    @State var preferMusicOnly = PlatformSettings.shared.getBoolean(key: "prefer_music_only", default: false)
    @State var confirmImport = false
    @State var theme = PlatformSettings.shared.getString(key: "theme_mode", default: "dark")
    @State var lyricsSources = LyricsSourceNames.normalizeList(
        PlatformSettings.shared.getString(key: "lyrics_sources", default: LyricsSourceNames.defaultEnabled)
    )
    @State var lyricsSourceOrder = LyricsSourceNames.normalizeList(
        PlatformSettings.shared.getString(key: "lyrics_source_order", default: LyricsSourceNames.defaultEnabled)
    )
    @State var playbackSpeed = Double(PlatformSettings.shared.getFloat(key: "playback_speed", default: 1))
    @State var jiosaavn = PlatformSettings.shared.getBoolean(key: "jiosaavn_enabled", default: true)
    @State var stopBackground = PlatformSettings.shared.getBoolean(key: "stop_when_backgrounded", default: false)
    @State var stopMusicOnClose = PlatformSettings.shared.getBoolean(key: "stop_music_on_close", default: false)
    @State var hidePlayerStatusBar = PlatformSettings.shared.getBoolean(key: "hide_status_bar_in_player", default: false)
    @State var showQueueHistory = PlatformSettings.shared.getBoolean(key: "show_queue_history", default: false)
    @State var artistSignatures = PlatformSettings.shared.getBoolean(key: "artist_signatures_enabled", default: true)
    @State var artistSignaturesWikimediaUrl = PlatformSettings.shared.getString(key: "artist_signatures_wikimedia_url", default: "")
    @State var syllableSync = PlatformSettings.shared.getBoolean(key: "prioritize_syllable_sync", default: true)
    @State var language = PlatformSettings.shared.getString(key: "app_language", default: "")
    @State var spotifyCookie = PlatformSettings.shared.getString(key: "spotify_spdc_token", default: "")
    @State var replayGenres = PlatformSettings.shared.getBoolean(key: "replay_genres", default: true)
    @State var exportPresented = false
    @State private var importPresented = false
    @State var backupNote: String?
    @State var cacheLimitMB = SettingsView.cacheLimitMegabytes()
    @State var cacheUnlimited = SettingsView.cacheIsUnlimited()
    @AppStorage(SettingsDeepLink.requestKey) var requestedSettingsPage = ""
    /// Whether the Library's folder list offers the cached-songs browser. Off by
    /// default, as upstream's storage switch is: the cache is a storage detail,
    /// not a music library, until somebody asks to browse it.
    @AppStorage("show_cached_songs_in_library") var showCachedSongs = false
    @State var loginPresented = false
    @State var discordPresented = false
    /// The Automix models offer, raised when Automix is switched on without the
    /// beat model that makes it worth having.
    @State private var offerModelsSheet = false
    @State var songCacheNote: String?
    @State var imageCacheNote: String?
    /// The settings search box. Empty means "no filter", not "nothing" — clearing
    /// it must bring the whole screen back.
    @State private var search = ""
    #if os(macOS)
    @State private var settingsHistory = PathNavigator()
    @State private var selectedMacSettingsSection: SettingsSection? = .account
    #endif

    var metered: Bool { NetworkQuality.shared.metered }

    var body: some View {
        Group {
            if embedded {
                settingsForm
                    .navigationTitle("Settings")
                    .searchable(text: $search, prompt: "Search settings")
            } else {
                #if os(macOS)
                macSettingsSplitView
                    .navigationTitle("Settings")
                    // The titlebar's own search pill at the trailing edge.
                    .searchable(text: $search, placement: .toolbar, prompt: "Search")
                    .environment(\.inBrowseStack, true)
                .toolbar {
                    ToolbarItem(placement: .navigation) {
                        MacHistoryButtons(navigator: settingsHistory)
                    }
                }
                .background(UnifiedTitlebar())
                #else
                NavigationStack {
                    settingsForm
                        .navigationTitle("Settings")
                        .searchable(text: $search, prompt: "Search settings")
                        .toolbar {
                            ToolbarItem(placement: .confirmationAction) {
                                Button("Done") { dismiss() }
                            }
                        }
                }
                #endif
            }
        }
        #if os(macOS)
        .frame(minWidth: 560, idealWidth: 620, minHeight: 720)
        #endif
        .alert("Higher refresh rate?", isPresented: $showPerfWarning) {
            Button("Cancel", role: .cancel) {}
            Button("Turn On") { highPerf = true }
        } message: {
            Text("Holding a high refresh rate keeps the display working harder the whole time BitChord is open, and the battery drains faster for it. Turn it on anyway?")
        }
        .fileImporter(isPresented: $pickingFolder, allowedContentTypes: [.folder]) { result in
            if case .success(let url) = result {
                LocalLibrary.shared.scanPicked(url)
                localFolderName = SettingsView.storedLocalFolderName()
                    ?? url.lastPathComponent
            }
        }
        .sheet(item: $updateSheet) { release in
            UpdateSheet(release: release)
                .environment(update)
        }
        .onAppear {
            // The tuner holds a display link while high performance is on, so it
            // has to be (re)applied every time this screen appears — and a value
            // written by another build that is not a rate offered here snaps to
            // 60 rather than requesting a mode the picker cannot name.
            if !PerformanceTuner.supportedRates.contains(refreshRate) { refreshRate = 60 }
            PerformanceTuner.shared.apply(highPerformance: highPerf, refreshRateHz: refreshRate)
            localFolderName = SettingsView.storedLocalFolderName()
        }
        .preferredColorScheme(appModel.preferredScheme)
        .onChange(of: dolbyAtmos) { _, value in AppSettings.shared.setDolbyAtmos(value: value) }
        .onChange(of: loudnessOffOnSpeaker) { _, value in
            AppSettings.shared.setLoudnessOffOnSpeaker(value: value)
            controller.updateLoudnessMode(controller.loudnessMode)
        }
        .onChange(of: hidePlayerStatusBar) { _, value in AppSettings.shared.setHideStatusBarInPlayer(value: value) }
        .onChange(of: showQueueHistory) { _, value in AppSettings.shared.setShowQueueHistory(value: value) }
        .onChange(of: stopMusicOnClose) { _, value in AppSettings.shared.setStopMusicOnClose(value: value) }
        .onChange(of: lyricsBlur) { _, value in AppSettings.shared.setLyricsBlur(value: value) }
        .onChange(of: translationLanguage) { _, value in AppSettings.shared.setTranslationLanguage(value: value) }
        .sheet(isPresented: $loginPresented) { loginSheet }
        .sheet(isPresented: $discordPresented) { DiscordLoginView() }
        // The offer again, from inside Settings, when Automix is switched on with
        // no beat model to time it. Presented here rather than through the root
        // binding because on macOS this screen is its own window and has no root
        // view above it.
        .sheet(isPresented: $offerModelsSheet) { AutomixModelsSheet() }
        .modifier(SettingsPlaybackPersist(
            controller: controller,
            crossfade: $crossfade,
            spatial: $spatial,
            automix: $automix,
            automixSequence: $automixSequence,
            automixPerf: $automixPerf,
            skipSilence: $skipSilence,
            playbackSpeed: $playbackSpeed,
            onAutomixEnabled: {
                if appModel.mayAskForAutomixModels() { offerModelsSheet = true }
            }
        ))
        .modifier(SettingsQualityPersist(
            controller: controller,
            wifiQuality: $wifiQuality,
            cellQuality: $cellQuality,
            downloadQuality: $downloadQuality,
            wifiOnlyDownloads: $wifiOnlyDownloads,
            cacheLimitMB: $cacheLimitMB,
            outputPcm: $outputPcm,
            matchSourceRate: $matchSourceRate,
            bitPerfect: $bitPerfect,
            preferUsbDac: $preferUsbDac,
            loudness: $loudness,
            loudnessModeSelection: $loudnessModeSelection
        ))
        .modifier(SettingsExperiencePersist(
            controller: controller,
            appModel: appModel,
            nerdStats: $nerdStats,
            canvas: $canvas,
            canvasCellular: $canvasCellular,
            reduceBlur: $reduceBlur,
            reduceAnimation: $reduceAnimation,
            fullBleed: $fullBleed,
            syncedLyrics: $syncedLyrics,
            convertVideo: $convertVideo,
            swipeNext: $swipeNext,
            dontRepeat: $dontRepeat,
            hideVolume: $hideVolume,
            legacyMesh: $legacyMesh,
            preferMusicOnly: $preferMusicOnly,
            theme: $theme
        ))
        .modifier(SettingsAdvancedPersist(
            controller: controller,
            smartAlign: $smartAlign,
            hideSongStatus: $hideSongStatus,
            exportDownloads: $exportDownloads,
            filterNonMusic: $filterNonMusic,
            highPerf: $highPerf,
            refreshRate: $refreshRate,
            reduceAnimation: $reduceAnimation,
            reduceBlur: $reduceBlur
        ))
        .modifier(SettingsExtrasPersist(
            lyricsSources: $lyricsSources,
            lyricsSourceOrder: $lyricsSourceOrder,
            jiosaavn: $jiosaavn,
            stopBackground: $stopBackground,
            syllableSync: $syllableSync,
            language: $language,
            spotifyCookie: $spotifyCookie,
            replayGenres: $replayGenres,
            appModel: appModel
        ))
        // Asked before the picker opens rather than after a file is chosen: the
        // thing being confirmed is that this device's own data is about to be
        // replaced, and that is true whichever file gets picked.
        .alert("Replace this device's data?", isPresented: $confirmImport) {
            Button("Cancel", role: .cancel) {}
            Button("Choose File") { importPresented = true }
        } message: {
            Text("Importing a backup replaces the settings and listening history on this device. This cannot be undone.")
        }
        .fileExporter(
            isPresented: $exportPresented,
            document: SettingsBackupFile(text: BitChordBackup.exportText()),
            contentType: .json,
            defaultFilename: BitChordBackup.suggestedName()
        ) { result in
            if case .success = result { backupNote = "Settings exported" }
        }
        .fileImporter(isPresented: $importPresented, allowedContentTypes: [.json]) { result in
            guard case .success(let url) = result else { return }
            let access = url.startAccessingSecurityScopedResource()
            defer { if access { url.stopAccessingSecurityScopedResource() } }
            if let data = try? Data(contentsOf: url), let text = String(data: data, encoding: .utf8) {
                BitChordBackup.importText(text)
                backupNote = "Imported — reopen Settings to see them"
            }
        }
    }

    /// The sections of this screen, in order.
    ///
    /// Only the order and the titles live here now. What a section *contains* is a
    /// question its own rows answer (see `SettingsRows`): a keyword table kept
    /// beside the section could only describe the section as a whole, which is how
    /// typing "flac" came to mean "show every row in Audio Quality".
    enum SettingsSection: String, CaseIterable, Identifiable {
        case account, audioQuality, downloads, playback
        case appearance, performance, localMusic, storage, yourData, miscellaneous, advanced, about

        var id: String { rawValue }

        var title: String {
            switch self {
            case .account: return "Account & Integrations"
            case .audioQuality: return "Audio Quality"
            case .downloads: return "Downloads"
            case .playback: return "Playback"
            case .appearance: return "Appearance"
            case .performance: return "Performance"
            case .localMusic: return "Local Music"
            case .storage: return "Storage"
            case .yourData: return "Your Data"
            case .miscellaneous: return "Miscellaneous"
            case .advanced: return "Advanced Options"
            case .about: return "About"
            }
        }

        var systemImage: String {
            switch self {
            case .account: return "person.crop.circle"
            case .audioQuality: return "waveform"
            case .downloads: return "arrow.down.circle"
            case .playback: return "play.circle"
            case .appearance: return "paintbrush"
            case .performance: return "speedometer"
            case .localMusic: return "music.note.list"
            case .storage: return "externaldrive"
            case .yourData: return "externaldrive"
            case .miscellaneous: return "ellipsis.circle"
            case .advanced: return "gearshape.2"
            case .about: return "info.circle"
            }
        }
    }

    // MARK: - Rows

    /// One row that survived the search filter, with whatever it draws itself as.
    struct SettingsRowEntry: Identifiable {
        let id: Int
        let content: AnyView
    }

    /// A group's rows, filtered by the search field as they are added.
    ///
    /// Ported from upstream's `SettingsGroupScope.row` (`Ua/ui/screens/SettingsSheet.kt:2332`):
    /// each row declares the words somebody might type looking for it, and the
    /// group's own title counts for every row beneath it, so searching "playback"
    /// turns up the playback group entire rather than nothing at all. A group with
    /// no rows left draws nothing — header, footer and all — which is why there is
    /// no section-level filter any more: whether a group exists is a question only
    /// its rows can answer, and a keyword table kept beside the group answered it
    /// for the whole group at once ("flac" used to bring back all of Audio Quality).
    struct SettingsRows {
        let query: String
        let sectionTitle: String
        private(set) var entries: [SettingsRowEntry] = []

        /// Adds a row if the search field admits it.
        ///
        /// - Parameters:
        ///   - title: what the row is called; also its first search term.
        ///   - keywords: what the title does not say that the row's subtitle or its
        ///     synonyms do — the words somebody would actually type ("flac" for a
        ///     lossless quality row, "party" for Listen Together).
        mutating func row<V: View>(
            _ title: String,
            _ keywords: String...,
            @ViewBuilder content: () -> V
        ) {
            row(title, keywords: keywords, content: content)
        }

        /// The same, for a caller that already holds its keywords as a list.
        mutating func row<V: View>(
            _ title: String,
            keywords: [String],
            @ViewBuilder content: () -> V
        ) {
            let terms = ([sectionTitle, title] + keywords).joined(separator: Self.separator)
            // The matching policy stays in the shared layer (SettingsSearchBridge),
            // where the terms and the comparison cannot drift apart.
            guard SettingsSearchBridge.shared.matches(
                query: query,
                title: sectionTitle,
                termsCsv: terms
            ) else { return }
            entries.append(SettingsRowEntry(id: entries.count, content: AnyView(content())))
        }

        /// ASCII unit separator, as the bridge expects: the terms are phrases, and
        /// a separator that can appear inside one is a separator that will split one.
        private static let separator = "\u{1F}"
    }

    /// A section and the rows the search field left in it.
    private struct BuiltSection: Identifiable {
        let section: SettingsSection
        let rows: [SettingsRowEntry]
        var id: String { section.id }
    }

    /// Every section, in order, with only the rows the search field admits.
    ///
    /// Built for the whole screen at once rather than section by section, because
    /// the "nothing matches" message is a question about the screen: a section that
    /// lost every row is not drawn, so the form has to know whether that happened
    /// to all of them.
    private func builtSections() -> [BuiltSection] {
        SettingsSection.allCases.compactMap { section in
            let rows = rows(for: section)
            return rows.isEmpty ? nil : BuiltSection(section: section, rows: rows)
        }
    }

    func rows(for section: SettingsSection) -> [SettingsRowEntry] {
        var rows = SettingsRows(query: search, sectionTitle: section.title)
        switch section {
        case .account: buildAccountRows(&rows)
        case .audioQuality: buildAudioQualityRows(&rows)
        case .downloads: buildDownloadRows(&rows)
        case .playback: buildPlaybackRows(&rows)
        case .appearance: buildAppearanceRows(&rows)
        case .performance: buildPerformanceRows(&rows)
        case .localMusic: buildLocalMusicRows(&rows)
        case .storage: buildStorageRows(&rows)
        case .yourData: buildYourDataRows(&rows)
        case .miscellaneous: buildMiscellaneousRows(&rows)
        case .advanced: buildAdvancedRows(&rows)
        case .about: buildAboutRows(&rows)
        }
        return rows.entries
    }

    /// What a section says underneath itself, when it has anything to say.
    ///
    /// The footer travels with the group: one that lost every row takes its footer
    /// with it rather than leaving a paragraph about settings that are not there.
    @ViewBuilder
    private func footer(for section: SettingsSection) -> some View {
        switch section {
        case .audioQuality:
            Text("Match Source Sample Rate asks the route to switch clock families between tracks. Bit-perfect output is available on macOS when the DAC grants exclusive access and the lossless source format is supported. The Audio Pipeline readout shows what is actually in effect.")
        case .downloads:
            Text("Exported downloads live in the shared Music folder. Private ones stay inside BitChord's own storage.")
        case .playback:
            Text("Gapless stays on at 0s. Automix picks timing per track; crossfade sets the manual blend length.")
        case .performance:
            Text("High Performance Mode holds the display at the chosen refresh rate instead of letting the system vary it. Expect noticeably shorter battery life while it is on.")
        case .localMusic:
            Text("BitChord watches the folder you choose for music files. Filtering keeps everything that is not a song out of the list.")
        case .miscellaneous:
            Text("With \u{201C}Stop When Backgrounded\u{201D} on, playback pauses when you leave the app — on Mac, closing the last window counts too.")
        case .advanced:
            Text("A version switch resumes where you left off. With this off it restarts from the top, since no alignment is attempted.")
        case .about:
            Text("BitChord \(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.1.0")")
                .frame(maxWidth: .infinity)
                .multilineTextAlignment(.center)
        case .account, .appearance, .storage, .yourData:
            EmptyView()
        }
    }

    #if os(macOS)
    private var macSettingsSplitView: some View {
        let visibleSections = builtSections().map(\.section)
        return SettingsMacSplitView(
            sections: visibleSections,
            selection: $selectedMacSettingsSection,
            sidebarLabel: { section in
                NavigationLink(value: section) {
                    Label(section.title, systemImage: section.systemImage)
                }
            },
            detail: { section in
                NavigationStack(path: Bindable(settingsHistory).path) {
                    macSettingsSection(section)
                        .navigationTitle("Settings")
                        .navigationDestination(for: PushRoute.self) { route in
                            route.make().navigationBarBackButtonHidden(true)
                        }
                        .onAppear { consumeMacSettingsDeepLinkIfNeeded() }
                        .onChange(of: requestedSettingsPage) { _, _ in
                            consumeMacSettingsDeepLinkIfNeeded()
                        }
                }
            }
        )
        .onChange(of: search) { _, _ in
            let visibleSections = builtSections().map(\.section)
            if let selectedMacSettingsSection,
               visibleSections.contains(selectedMacSettingsSection) {
                return
            }
            selectedMacSettingsSection = visibleSections.first
        }
    }

    private func macSettingsSection(_ section: SettingsSection) -> some View {
        let rows = rows(for: section)
        return Form {
            Section {
                ForEach(rows) { row in row.content }
            } header: {
                Text(section.title)
            } footer: {
                if search.isEmpty { footer(for: section) }
            }
        }
        .formStyle(.grouped)
    }

    private func consumeMacSettingsDeepLinkIfNeeded() {
        guard !requestedSettingsPage.isEmpty,
              let rawValue = SettingsSectionName.canonical(requestedSettingsPage),
              let section = SettingsSection(rawValue: rawValue)
        else { return }
        requestedSettingsPage = ""
        search = ""
        selectedMacSettingsSection = section
    }
    #endif

    private var settingsForm: some View {
        let sections = builtSections()
        return ScrollViewReader { proxy in
            Form {
                ForEach(sections) { built in
                    Section {
                        ForEach(built.rows) { row in
                            row.content
                        }
                    } header: {
                        Text(built.section.title)
                    } footer: {
                        // Not while searching. A footer explains the rows that sat above
                        // it, and the search has filtered those: the Audio Quality note
                        // talks about sample-rate matching and bit-perfect output whether
                        // or not either row survived the query. Upstream draws no
                        // footers in search results for the same reason.
                        if search.isEmpty {
                            footer(for: built.section)
                        }
                    }
                    .id(built.section.id)
                }
                if !search.isEmpty && sections.isEmpty {
                    ContentUnavailableView.search(text: search)
                }
            }
            .formStyle(.grouped)
            // The sheets, the picker and the warning belong to the screen rather than
            // to one group: the group they used to hang off can be filtered away, and
            // what it raises must still be able to appear.
            .onAppear { consumeSettingsDeepLinkIfNeeded(using: proxy) }
            .onChange(of: requestedSettingsPage) { _, _ in consumeSettingsDeepLinkIfNeeded(using: proxy) }
        }
    }

    private func consumeSettingsDeepLinkIfNeeded(using proxy: ScrollViewProxy) {
        guard !requestedSettingsPage.isEmpty,
              let section = SettingsSectionName.canonical(requestedSettingsPage)
        else { return }
        requestedSettingsPage = ""
        search = ""
        Task { @MainActor in
            await Task.yield()
            withAnimation { proxy.scrollTo(section, anchor: .top) }
        }
    }

    // MARK: - Account

}

// MARK: - Account & integrations
