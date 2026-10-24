import SwiftUI

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
///  - Space / ⌘P — play-pause
///  - ← → — previous / next track
///  - ⌘← ⌘→ — previous / next tab
///  - ⌘↑ ⌘↓ — volume
///  - ⌘F — focus search
///  - ⌘L — toggle lyrics
///  - ⌘U — toggle the queue
///
/// Left and right arrow are *track* navigation rather than seek, deliberately:
/// Media's behaviour depends on the Now Playing window having focus, and
/// stealing bare arrows from every text field in the app for a transport binding
/// would be worse than not having it. Bare arrows still work as scrub within the
/// player's focused controls.
struct PlaybackCommands: Commands {
    let controller: PlaybackController
    let appModel: AppModel

    var body: some Commands {
        // Replace the stock Undo/Redo group: this app has nothing to undo, and
        // the standard entries would sit there doing nothing.
        CommandGroup(replacing: .undoRedo) {}

        CommandGroup(replacing: .textEditing) {
            // ⌘F focuses search, the way it does in Music and Safari. Placed here
            // rather than in a submenu so it works from anywhere in the app.
            Button("Search") {
                appModel.requestedTab = .search
                appModel.focusSearch = true
            }
            .keyboardShortcut("f", modifiers: .command)

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
        }

        CommandMenu("Controls") {
            Button(controller.isPlaying ? "Pause" : "Play") {
                controller.togglePlayPause()
            }
            .keyboardShortcut(.space, modifiers: [])

            Button("Next Track") { controller.next() }
                .keyboardShortcut(.rightArrow, modifiers: [])

            Button("Previous Track") { controller.previous() }
                .keyboardShortcut(.leftArrow, modifiers: [])

            Divider()

            Button("Volume Up") { controller.volume = min(1, controller.volume + 0.05) }
                .keyboardShortcut(.upArrow, modifiers: .command)

            Button("Volume Down") { controller.volume = max(0, controller.volume - 0.05) }
                .keyboardShortcut(.downArrow, modifiers: .command)

            Divider()

            Button("Go Home") { appModel.requestedTab = .home }
                .keyboardShortcut(.leftArrow, modifiers: .command)

            Button("Go to Library") { appModel.requestedTab = .libraryYouTube }
                .keyboardShortcut(.rightArrow, modifiers: .command)
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
}
