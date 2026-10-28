import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
#if os(iOS)
import UIKit
#else
import AppKit
#endif

// Your data, miscellaneous and advanced options, About, and updates.
extension SettingsView {
    func buildYourDataRows(_ rows: inout SettingsRows) {
        rows.row("Replay", "top songs", "top artists", "stats", "year", "listening") {
            Button {
                appModel.replayPresented = true
            } label: {
                SettingsLine(
                    glyph: .replay,
                    title: "Replay",
                    subtitle: "Your top songs, artists, albums and genres"
                ) {
                    EmptyView()
                }
            }
            .buttonStyle(.plain)
        }
        rows.row("Work Out Genres", "genres", "last.fm", "lastfm", "replay", "chart", "artist") {
            SettingsToggleLine(
                glyph: .genres,
                title: "Work Out Genres",
                subtitle: replayGenres
                    ? "Asks Last.fm what an artist plays — their name is sent, nothing else"
                    : "Replay's genre chart is hidden while this is off",
                isOn: $replayGenres
            )
        }
        rows.row("Export Data", "export", "backup", "json", "data", "history", "reset", "erase", "privacy") {
            Button {
                exportPresented = true
            } label: {
                SettingsLine(
                    glyph: .export,
                    title: "Export Data",
                    subtitle: backupNote ?? "Settings and listening history, as one JSON file"
                ) {
                    EmptyView()
                }
            }
            .buttonStyle(.plain)
        }
        rows.row("Import Data", "import", "backup", "restore", "data", "reset", "erase") {
            Button {
                confirmImport = true
            } label: {
                SettingsLine(
                    glyph: .importData,
                    title: "Import Data",
                    subtitle: "Restores non-secret preferences"
                ) {
                    EmptyView()
                }
            }
            .buttonStyle(.plain)
        }
    }

    // MARK: - Miscellaneous

    func buildMiscellaneousRows(_ rows: inout SettingsRows) {
        rows.row("Play Next on Swipe", "swipe", "queue", "next", "gesture", "song") {
            SettingsToggleLine(
                glyph: .swipe,
                title: "Play Next on Swipe",
                subtitle: swipeNext
                    ? "Swiping a song plays it next"
                    : "Swiping a song adds it to the end of the queue when disabled",
                isOn: $swipeNext
            )
        }
        rows.row("Don’t Repeat Songs in Current Session", "repeat", "suggestions", "autoplay", "session", "duplicate") {
            SettingsToggleLine(
                glyph: .dontRepeat,
                title: "Don’t Repeat Songs in Current Session",
                subtitle: "AutoPlay won’t suggest a song already played or suggested this session",
                isOn: $dontRepeat
            )
        }
        rows.row("Hide Volume Bar", "volume", "bar", "slider", "player", "hide") {
            SettingsToggleLine(
                glyph: .hideVolume,
                title: "Hide Volume Bar",
                subtitle: "Removes the volume slider from the main player",
                isOn: $hideVolume
            )
        }
        rows.row("Hide Song Status", "song status", "playing from", "played by", "caption", "hide", "player") {
            SettingsToggleLine(
                glyph: .songStatus,
                title: "Hide Song Status",
                subtitle: "Hides the “Playing from” caption on the main player",
                isOn: $hideSongStatus
            )
        }
        #if os(iOS)
        rows.row("Hide Status Bar in Player", "status bar", "immersive", "fullscreen", "player") {
            SettingsToggleLine(
                glyph: .hideVolume,
                title: "Hide Status Bar in Player",
                subtitle: "Hides the iPhone status bar while Now Playing is open",
                isOn: $hidePlayerStatusBar
            )
        }
        #endif
        rows.row("Show Queue History", "queue", "history", "previously played", "player") {
            SettingsToggleLine(
                glyph: .dontRepeat,
                title: "Show Queue History",
                subtitle: "Shows tracks already played above Now Playing in the queue",
                isOn: $showQueueHistory
            )
        }
        rows.row("Stop When Backgrounded", "background", "stop", "pause", "close", "quit") {
            SettingsToggleLine(
                glyph: .video,
                title: "Stop When Backgrounded",
                subtitle: "Pauses playback when BitChord is hidden or deactivated on Mac; closing a window uses its own setting",
                isOn: $stopBackground
            )
        }
        #if os(macOS)
        rows.row("Stop Music on Close", "close", "window", "quit", "pause", "stop") {
            SettingsToggleLine(
                glyph: .video,
                title: "Stop Music on Close",
                subtitle: "Pauses playback when the main window closes",
                isOn: $stopMusicOnClose
            )
        }
        #endif
        rows.row("Listen together", "listen together", "party", "jam", "invite", "party code", "party server",
                 "in sync", "synchronise", "synchronize") {
            PushLink {
                ListenTogetherView()
            } label: {
                SettingsLine(
                    glyph: .listenTogether,
                    title: "Listen together",
                    subtitle: listenTogetherSubtitle,
                    badge: partyBadge
                ) {
                    EmptyView()
                }
            }
        }
        rows.row("Spotify Canvas", "spotify", "canvas", "sp_dc", "cookie", "motion art") {
            PushLink {
                SpotifyCanvasAuthView(cookie: $spotifyCookie)
            } label: {
                SettingsLine(
                    glyph: .canvas,
                    title: "Spotify Canvas",
                    subtitle: spotifyCookie.isEmpty ? "Optional sp_dc cookie for motion art" : "Cookie saved"
                ) {
                    EmptyView()
                }
            }
        }
        rows.row("App Language", "language", "locale", "translate", "english", "spanish", "french", "german", "portuguese", "hindi", "japanese", "korean", "chinese") {
            Picker(selection: $language) {
                Text("System").tag("")
                Text("English").tag("en")
                Text("Spanish").tag("es")
                Text("French").tag("fr")
                Text("German").tag("de")
                Text("Portuguese").tag("pt")
                Text("Hindi").tag("hi")
                Text("Japanese").tag("ja")
                Text("Korean").tag("ko")
                Text("Chinese").tag("zh-Hans")
            } label: {
                SettingsLine(glyph: .theme, title: "App Language", subtitle: "Follows system unless you pick one") {
                    EmptyView()
                }
            }
        }
    }

    // MARK: - Advanced options

    func buildAdvancedRows(_ rows: inout SettingsRows) {
        rows.row("Smart Version Alignment", "version", "alignment", "waveform", "sync", "switch", "audio",
                 "video", "intro", "skit", "codec", "bitrate") {
            SettingsToggleLine(
                glyph: .align,
                title: "Smart Version Alignment",
                subtitle: "Keeps your place when switching between versions of a track",
                isOn: $smartAlign
            )
        }
    }

    // MARK: - About

    func buildAboutRows(_ rows: inout SettingsRows) {
        rows.row("Engine", "native core", "version", "engine") {
            LabeledContent("Engine", value: "native-core v\(coreVersion())")
        }
        rows.row("Logic Core", "logic core", "kotlin", "version") {
            LabeledContent("Logic Core", value: GreetingKt.sharedGreeting())
        }
        rows.row("Version", "version", "build", "release", "credits", "licence", "license",
                 "acknowledgements", "privacy policy") {
            LabeledContent("Version", value: update.currentVersion)
        }
        buildUpdateRows(&rows)
        rows.row("GitHub", "github", "source", "code", "repository") {
            Link("GitHub", destination: URL(string: "https://github.com/bagumamartin/BitChord")!)
        }
        rows.row("Developer", "developer", "author", "github") {
            Link("Developer", destination: URL(string: "https://github.com/bagumamartin")!)
        }
        rows.row("Open-source licenses", "open source", "licenses", "license", "acknowledgements", "third party") {
            PushLink {
                LicenseAcknowledgementsView()
            } label: {
                SettingsLine(
                    glyph: .sources,
                    title: "Open-source licenses",
                    subtitle: "Third-party software, models and asset notices"
                ) {
                    EmptyView()
                }
            }
        }
        rows.row("Discord", "discord", "community", "chat") {
            Link("Discord", destination: URL(string: "https://discord.gg/pDdKfrdHY6")!)
        }
    }

    private func buildUpdateRows(_ rows: inout SettingsRows) {
        rows.row("Version & Updates", "update", "newer", "release", "version", "build", "install", "ipa", "dmg", "sparkle") {
            PushLink { UpdatesSettingsPage() } label: {
                SettingsLine(glyph: .update, title: "Version & Updates", subtitle: update.currentVersion) { EmptyView() }
            }
        }
    }

    var loginSheet: some View {
        NavigationStack {
            YtMusicLoginView(
                onCaptured: { session, done in
                    auth.accept(session) { accepted in
                        if accepted { loginPresented = false }
                        done(accepted)
                    }
                },
                onDismiss: { loginPresented = false }
            )
        }
        #if os(macOS)
        .frame(minWidth: 720, minHeight: 640)
        #endif
    }
}
