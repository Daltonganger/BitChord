import SwiftUI
import BitChordShared

/// Menu-bar and keyboard commands for macOS.
///
/// The app previously had exactly one menu command — the `Settings` scene — and
/// no `.keyboardShortcut` anywhere. That is the most conspicuous gap on the Mac:
/// a music app is expected to be drivable from the keyboard, and every Mac user
/// tries space, arrow keys and ⌘F without being told otherwise.
///
/// The set is deliberately Music's, and the transport bindings are the ones
/// people already have in their fingers:
///
///  - Space — play-pause
///  - ← → — previous / next track
///  - ⌥⌘← ⌥⌘→ — seek back / forward ten seconds
///  - ⇧⌘S ⇧⌘R — shuffle / cycle repeat
///  - ⌘↑ ⌘↓ — volume
///  - ⌘F — search
///  - ⌘L — show lyrics
///  - ⌘U — show the queue
///  - ⇧⌘P — show the player
///  - ⌘1 … ⌘6 — Home, Explore, Library, Playlists, Downloads, History
///  - ⌘← ⌘→ — Home / Library, as they were before the Go menu existed
///
/// Left and right arrow are *track* navigation rather than seek, deliberately:
/// Music's behaviour depends on the Now Playing window having focus, and stealing
/// bare arrows from every text field in the app for a transport binding would be
/// worse than not having it. Bare arrows still work as scrub within the player's
/// focused controls; seeking has its own noisy binding instead.
struct PlaybackCommands: Commands {
    let controller: PlaybackController
    let appModel: AppModel
    let auth: AuthController
    let toast: ToastCenter

    var body: some Commands {
        // Replace the stock Undo/Redo group: this app has nothing to undo, and
        // the standard entries would sit there doing nothing. The rest of the
        // standard Edit menu is left alone — this app has text fields (search,
        // a cookie, playlist names), and Cut/Copy/Paste belong in them.
        CommandGroup(replacing: .undoRedo) {}

        // The player's panes live in View, beside the sidebar toggle, rather than
        // in Edit where they were: none of them edits text.
        CommandGroup(after: .sidebar) {
            Button("Show Player") {
                appModel.nowPlayingPresented = true
            }
            .keyboardShortcut("p", modifiers: [.command, .shift])

            Button("Show Queue") {
                appModel.nowPlayingPresented = true
                appModel.queueRevealRequested = true
            }
            .keyboardShortcut("u", modifiers: .command)

            Button("Show Lyrics") {
                appModel.nowPlayingPresented = true
                appModel.lyricsRevealRequested = true
            }
            .keyboardShortcut("l", modifiers: .command)

            Divider()

            Button("Search") {
                appModel.requestedTab = .search
                appModel.focusSearch = true
            }
            .keyboardShortcut("f", modifiers: .command)
        }

        CommandMenu("Controls") {
            Button(controller.isPlaying ? "Pause" : "Play") {
                controller.togglePlayPause()
            }
            .keyboardShortcut(.space, modifiers: [])
            .disabled(controller.current == nil)

            Button("Next Track") { controller.next() }
                .keyboardShortcut(.rightArrow, modifiers: [])
                .disabled(!controller.canPlayNext)

            Button("Previous Track") { controller.previous() }
                .keyboardShortcut(.leftArrow, modifiers: [])
                .disabled(!controller.canPlayPrevious)

            Divider()

            Button("Seek Forward 10 Seconds") {
                controller.seek(to: min(controller.position + 10, max(controller.duration, 0)))
            }
            .keyboardShortcut(.rightArrow, modifiers: [.command, .option])
            .disabled(controller.duration <= 0)

            Button("Seek Backward 10 Seconds") {
                controller.seek(to: max(controller.position - 10, 0))
            }
            .keyboardShortcut(.leftArrow, modifiers: [.command, .option])
            .disabled(controller.duration <= 0)

            Divider()

            // A check mark rather than a control, as Music does it: the menu
            // states whether shuffle is on, and picking it turns it over.
            Toggle("Shuffle", isOn: Binding(
                get: { controller.shuffleEnabled },
                set: { _ in controller.toggleShuffle() }
            ))
            .keyboardShortcut("s", modifiers: [.command, .shift])

            Button(repeatTitle) { controller.cycleRepeat() }
                .keyboardShortcut("r", modifiers: [.command, .shift])

            Divider()

            Button("Volume Up") { controller.volume = min(1, controller.volume + 0.05) }
                .keyboardShortcut(.upArrow, modifiers: .command)

            Button("Volume Down") { controller.volume = max(0, controller.volume - 0.05) }
                .keyboardShortcut(.downArrow, modifiers: .command)
        }

        // Everything that acts on the song that is playing, reachable from
        // wherever the listener is. The actions are the context menu's own
        // (`SongActionButtons`), through the one implementation in
        // `SongRatingActions`, so the two surfaces cannot disagree about what a
        // like or a download means.
        CommandMenu("Track") {
            if let entry = controller.current {
                Button("Play") { controller.play([entry], at: 0) }
                Button("Play Next") {
                    controller.playNext(entry)
                    toast.queueNotice("Playing next: \(entry.title)")
                }
                Button("Add to Queue") {
                    controller.addToQueue(entry)
                    toast.queueNotice("Added to queue: \(entry.title)")
                }
                Button("Start Radio") { controller.playRadio(entry) }

                Divider()

                if auth.signedIn, let videoId = entry.videoId {
                    Button(SongRatingActions.likeTitle(videoId)) {
                        Task { await SongRatingActions.toggleLike(videoId: videoId, controller: controller, toast: toast) }
                    }
                    Button(SongRatingActions.dislikeTitle(videoId)) {
                        Task { await SongRatingActions.toggleDislike(videoId: videoId, controller: controller, toast: toast) }
                    }
                    Button("Add to Playlist…") {
                        appModel.playlistPicker = PlaylistPickerRequest(videoId: videoId, title: entry.title)
                    }

                    Divider()
                }

                Button(downloadTitle(entry)) {
                    SongRatingActions.startDownload(entry, toast: toast)
                }
                .disabled(!canDownload(entry))

                Button("Show Downloads") { appModel.downloadManagerPresented = true }
            } else {
                Button("Nothing Playing") {}.disabled(true)
            }
        }

        CommandMenu("Go") {
            Button("Home") { appModel.requestedTab = .home }
                .keyboardShortcut("1", modifiers: .command)
            Button("Explore") { appModel.requestedTab = .explore }
                .keyboardShortcut("2", modifiers: .command)
            Button("Library") { appModel.requestedTab = .library }
                .keyboardShortcut("3", modifiers: .command)
            Button("Playlists") { appModel.requestedTab = .libraryPlaylists }
                .keyboardShortcut("4", modifiers: .command)
            Button("Downloads") { appModel.requestedTab = .libraryDownloads }
                .keyboardShortcut("5", modifiers: .command)
            Button("History") { appModel.requestedTab = .libraryHistory }
                .keyboardShortcut("6", modifiers: .command)

            Divider()

            // The library's other shelves, which the sidebar reaches with the
            // mouse and the keyboard now reaches the same way.
            Button("Songs") { appModel.requestedTab = .librarySongs }
            Button("Albums") { appModel.requestedTab = .libraryAlbums }
            Button("Artists") { appModel.requestedTab = .libraryArtists }
            Button("Subscriptions") { appModel.requestedTab = .librarySubscriptions }
            Button("Podcasts") { appModel.requestedTab = .libraryPodcasts }
            Button("On Device") { appModel.requestedTab = .libraryOnDevice }
            Button("WebDAV") { appModel.requestedTab = .libraryWebDav }

            Divider()

            Button("Replay") { appModel.openReplay() }
            Button("Listen Together") { appModel.listenTogetherPresented = true }
        }

        // File ▸ Import. The toolbar copies of these live in the Playlists page's
        // overflow, where the transport pushes them out of sight; the menu reaches
        // them from anywhere by switching there first.
        CommandGroup(after: .newItem) {
            Button("Import from Spotify…") {
                appModel.requestedTab = .libraryPlaylists
                appModel.spotifyImportPresented = true
            }
            .keyboardShortcut("i", modifiers: [.command, .shift])

            Button("Import Playlist File…") {
                appModel.requestedTab = .libraryPlaylists
                appModel.playlistFileImportPresented = true
            }
        }
    }

    /// What the repeat command says it will do next.
    ///
    /// The label names the *current* mode rather than the one a press would move
    /// to, because the menu is also the readout: somebody opening Controls wants
    /// to know whether repeat is on, not to be told what happens if they press it.
    private var repeatTitle: String {
        switch controller.repeatMode {
        case .off: "Repeat Off"
        case .all: "Repeat All"
        case .one: "Repeat One"
        }
    }

    private func downloadTitle(_ entry: QueueEntry) -> String {
        if DownloadStore.shared.isDownloaded(entry) { return "Downloaded" }
        if DownloadStore.shared.jobs.contains(where: { job in
            job.entry.source == entry.source && job.entry.id == entry.id
                && (job.status == .queued || job.status == .running)
        }) {
            return "Downloading…"
        }
        return "Download"
    }

    private func canDownload(_ entry: QueueEntry) -> Bool {
        downloadTitle(entry) == "Download"
    }
}
