import AppIntents
import Foundation

// MARK: - The app's objects, for code that has no scene

/// The objects an App Intent needs and the app's scene also owns.
///
/// Siri, Shortcuts and Spotlight perform an intent in the app's process but
/// outside the SwiftUI environment: there is no `@Environment(PlaybackController.self)`
/// to read and no `AppModel` to navigate with. They are created here, once, and
/// the `App` scene takes its `@State` values from the same holder — so there is
/// still exactly one of each. That matters for the player in particular: a second
/// `PlaybackController` boots a second output stream and fights the first for the
/// system's media slot, which is why `partySync` is not constructed inline either.
///
/// A `static let` exists as soon as anything asks for it, which covers an intent
/// performed during launch, before the first window has been made.
@MainActor
final class AppServices {
    static let shared = AppServices()

    let controller = PlaybackController()
    let appModel = AppModel()
    let auth = AuthController()

    private init() {}

    /// Wait for the app to finish coming up before an intent touches the network
    /// or the engine.
    ///
    /// The launch task installs the cipher and PoToken wiring that a stream
    /// resolution depends on, and the app reports itself ready when the first
    /// content lands (or after its own two-second fallback). An app that is
    /// already running — the usual case for "play" and "like" — returns at once.
    func waitUntilReady() async {
        await LaunchReadiness.shared.waitForContent()
    }
}

// MARK: - What an intent may say

/// The ways a BitChord intent can fail, in words Siri can say.
///
/// `LocalizedError` rather than a bare error carrying a message: Shortcuts speaks
/// the error's `localizedDescription`, and an ordinary struct reads "The operation
/// couldn't be completed. (BitChordIntentError error 1.)" — the same trap the
/// session-expiry matcher hit in I10a-1.
enum BitChordIntentError: LocalizedError {
    case emptyQuery
    case nothingPlaying
    case signedOut
    case noResults(String)
    case refused(String)

    var errorDescription: String? {
        switch self {
        case .emptyQuery:
            "What should BitChord look for?"
        case .nothingPlaying:
            "Nothing is playing in BitChord."
        case .signedOut:
            "Sign in to BitChord to like songs."
        case .noResults(let query):
            "BitChord couldn't find a song for “\(query)”."
        case .refused(let message):
            message
        }
    }
}

// MARK: - The verbs

/// What the intents do, on the main actor because the player and the navigation
/// model live there.
///
/// Kept apart from the intents themselves so an intent stays what Siri needs: a
/// title, a parameter and the sentence to say back.
@MainActor
enum AppIntentActions {
    /// Play or pause, and say which happened.
    static func togglePlayback() async throws -> String {
        let controller = AppServices.shared.controller
        guard let current = controller.current else { throw BitChordIntentError.nothingPlaying }
        // The state is read *before* the toggle: `togglePlayPause` starts (or
        // keeps) an asynchronous load, so what it leaves behind is not always the
        // outcome this call asked for, and reading it after named the wrong one.
        let wasPlaying = controller.isPlaying || controller.isBuffering
        controller.togglePlayPause()
        return wasPlaying ? "Paused \(current.title)" : "Playing \(current.title)"
    }

    /// Search and play the best match, with the rest of the page queued behind it
    /// so Next has somewhere to go.
    static func play(song: String) async throws -> String {
        let term = try cleaned(song)
        await AppServices.shared.waitUntilReady()
        let entries = try await tracks(matching: term)
        guard let first = entries.first else { throw BitChordIntentError.noResults(term) }
        AppServices.shared.controller.play(entries, at: 0, context: "Siri")
        return first.title
    }

    /// Open Search with the term already in the field — the same request shape a
    /// `bitchord://search` link makes, so both roads end in the same place.
    static func search(query: String) throws -> String {
        let term = try cleaned(query)
        let appModel = AppServices.shared.appModel
        appModel.pendingSearchQuery = term
        appModel.requestedTab = .search
        appModel.focusSearch = true
        return term
    }

    /// Like or unlike what is playing.
    static func likeCurrent() async throws -> String {
        let services = AppServices.shared
        guard services.auth.signedIn else { throw BitChordIntentError.signedOut }
        guard let current = services.controller.current, let videoId = current.videoId else {
            throw BitChordIntentError.nothingPlaying
        }
        if let failure = await services.controller.toggleLikeAndWait(videoId: videoId) {
            throw BitChordIntentError.refused(failure)
        }
        return services.controller.isLiked
            ? "Liked \(current.title)"
            : "Removed \(current.title) from Liked Music"
    }

    /// Add the best match to the end of the queue without disturbing what is
    /// playing. With an idle player this also starts it, which is what an empty
    /// queue under "add to queue" has to mean.
    static func addToQueue(song: String) async throws -> String {
        let term = try cleaned(song)
        await AppServices.shared.waitUntilReady()
        guard let entry = try await tracks(matching: term).first else {
            throw BitChordIntentError.noResults(term)
        }
        AppServices.shared.controller.addToQueue(entry)
        return entry.title
    }

    /// The playable rows for a term, deduped and capped at a queue's worth.
    ///
    /// A search page lists the same recording twice often enough — the promoted
    /// top result and the row below it — and a queue holding one song twice is a
    /// Next that appears to do nothing.
    private static func tracks(matching term: String) async throws -> [QueueEntry] {
        let hits = try await InnertubeSearch.shared.search(term, scope: "songs")
        var seen = Set<String>()
        return hits
            .filter { $0.isTrack && !$0.id.isEmpty && seen.insert($0.id).inserted }
            .prefix(10)
            .map { $0.asEntry() }
    }

    private static func cleaned(_ query: String) throws -> String {
        let term = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !term.isEmpty else { throw BitChordIntentError.emptyQuery }
        return term
    }
}

// MARK: - The intents

/// Play or pause what is already loaded. Background-capable: a listener who says
/// "pause" is not asking for a window.
struct PlayPauseIntent: AppIntent {
    static var title: LocalizedStringResource = "Play or Pause"
    static var description = IntentDescription("Play or pause what BitChord is playing.")
    static var openAppWhenRun = false

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let line = try await AppIntentActions.togglePlayback()
        return .result(dialog: "\(line)")
    }
}

/// Play a song by name. The app opens because the match is a guess — the player
/// and the queue are where a listener checks it — and because a cold launch's
/// stream wiring settles behind the first window.
struct PlaySongIntent: AppIntent {
    static var title: LocalizedStringResource = "Play a Song"
    static var description = IntentDescription("Search BitChord for a song and start playing it.")
    static var openAppWhenRun = true

    @Parameter(title: "Song", requestValueDialog: "Which song?")
    var song: String

    init() {}

    init(song: String) {
        self.song = song
    }

    static var parameterSummary: some ParameterSummary {
        Summary("Play \(\.$song)")
    }

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let title = try await AppIntentActions.play(song: song)
        return .result(dialog: "Playing \(title) in BitChord")
    }
}

/// Open Search with a term. The one intent that must open the app: its whole
/// result is the screen.
struct SearchIntent: AppIntent {
    static var title: LocalizedStringResource = "Search"
    static var description = IntentDescription("Open BitChord's search with a query.")
    static var openAppWhenRun = true

    @Parameter(title: "Search For", requestValueDialog: "What should BitChord search for?")
    var query: String

    init() {}

    init(query: String) {
        self.query = query
    }

    static var parameterSummary: some ParameterSummary {
        Summary("Search for \(\.$query)")
    }

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let term = try await AppIntentActions.search(query: query)
        return .result(dialog: "Searching BitChord for \(term)")
    }
}

/// Like or unlike the current track. Background-capable for the same reason as
/// pause — and because liking needs something playing, which means this app is
/// already alive in the audio session.
struct LikeCurrentSongIntent: AppIntent {
    static var title: LocalizedStringResource = "Like the Current Song"
    static var description = IntentDescription("Like or unlike what is playing in BitChord.")
    static var openAppWhenRun = false

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let line = try await AppIntentActions.likeCurrent()
        return .result(dialog: "\(line)")
    }
}

/// Add a song by name to the end of the queue.
struct AddToQueueIntent: AppIntent {
    static var title: LocalizedStringResource = "Add to the Queue"
    static var description = IntentDescription("Find a song and add it to the end of BitChord's queue.")
    static var openAppWhenRun = true

    @Parameter(title: "Song", requestValueDialog: "Which song?")
    var song: String

    init() {}

    init(song: String) {
        self.song = song
    }

    static var parameterSummary: some ParameterSummary {
        Summary("Add \(\.$song) to the queue")
    }

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let title = try await AppIntentActions.addToQueue(song: song)
        return .result(dialog: "Added \(title) to the queue")
    }
}

// MARK: - Siri and Spotlight phrases

/// The phrases Siri answers to, and the entry points Spotlight lists.
///
/// Shortcuts does not need this: every intent above is already a Shortcuts action
/// by existing. These phrases are what make them reachable by voice and from a
/// Spotlight search for the app.
struct BitChordShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: PlaySongIntent(),
            phrases: [
                "Play a song in \(.applicationName)",
                "Play some music in \(.applicationName)"
            ],
            shortTitle: "Play a Song",
            systemImageName: "play.fill"
        )
        AppShortcut(
            intent: PlayPauseIntent(),
            phrases: [
                "Pause \(.applicationName)",
                "Play or pause \(.applicationName)"
            ],
            shortTitle: "Play or Pause",
            systemImageName: "pause.fill"
        )
        AppShortcut(
            intent: SearchIntent(),
            phrases: ["Search in \(.applicationName)"],
            shortTitle: "Search",
            systemImageName: "magnifyingglass"
        )
        AppShortcut(
            intent: LikeCurrentSongIntent(),
            phrases: ["Like this song in \(.applicationName)"],
            shortTitle: "Like This Song",
            systemImageName: "heart.fill"
        )
        AppShortcut(
            intent: AddToQueueIntent(),
            phrases: ["Add a song to the queue in \(.applicationName)"],
            shortTitle: "Add to Queue",
            systemImageName: "text.badge.plus"
        )
    }
}
