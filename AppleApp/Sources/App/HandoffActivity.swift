import Foundation

/// What one device tells another when playback moves across with Handoff.
///
/// Foundation-only and separate from the activity that carries it, for two
/// reasons. The encoding is testable without an app (`scripts/check-handoff.sh`),
/// and the checks that decide whether a payload is trustworthy are exactly the
/// parts that break silently: a payload the other device cannot read is a
/// Handoff that does nothing, which is not the kind of failure anybody reports
/// as a bug.
struct PlaybackHandoffPayload: Codable, Equatable {
    /// The schema this build writes. A receiver only accepts what it knows, so a
    /// newer payload is ignored rather than decoded into guessed-at fields.
    static let currentVersion = 1

    /// The activity type. The same string is declared in the app's
    /// `NSUserActivityTypes`, which is what lets the system launch this app for
    /// an activity that arrives from the other device.
    static let activityType = "app.bitchord.playback"

    /// Where the payload rides inside `NSUserActivity.userInfo`.
    static let userInfoKey = "payload"

    var version: Int
    var videoId: String
    var title: String
    var artist: String
    /// Where the playhead was when the activity was last refreshed.
    var positionMs: Int
    var isPlaying: Bool

    var positionSeconds: Double {
        Double(max(0, positionMs)) / 1000
    }

    init(videoId: String, title: String, artist: String, positionMs: Int, isPlaying: Bool) {
        self.version = Self.currentVersion
        self.videoId = videoId
        self.title = title
        self.artist = artist
        self.positionMs = max(0, positionMs)
        self.isPlaying = isPlaying
    }

    /// The activity's `userInfo`: one JSON string rather than six loose keys.
    ///
    /// `userInfo` only accepts property-list types, and the ones it accepts are
    /// the ones a hand-written encoder and decoder disagree about — an `Int` that
    /// comes back as `NSNumber`, a `Bool` that arrives as a number. One key, one
    /// encoder, no disagreement.
    var userInfo: [AnyHashable: Any] {
        guard let data = try? JSONEncoder().encode(self),
              let json = String(data: data, encoding: .utf8) else { return [:] }
        return [Self.userInfoKey: json]
    }

    /// Reads a payload out of an activity's `userInfo`.
    ///
    /// Nil for anything that is not a payload this build understands: an empty
    /// `userInfo` from a different activity, a corrupted string, a version this
    /// build must not guess at, or a track with no id to play.
    init?(userInfo: [AnyHashable: Any]?) {
        guard let json = userInfo?[Self.userInfoKey] as? String,
              let data = json.data(using: .utf8),
              let payload = try? JSONDecoder().decode(Self.self, from: data),
              payload.version == Self.currentVersion,
              !payload.videoId.isEmpty
        else { return nil }
        self = payload
    }
}
