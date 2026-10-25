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

/// What the provider picker knows about one source for the current track —
/// the Swift mirror of the shared bridge's `LyricsBridge.LyricsProviderState`.
enum LyricsSourceState {
    case notFetched
    case fetching
    case found
    case notFound
    case blocked

    init(_ shared: LyricsBridge.LyricsProviderState) {
        switch shared {
        case .fetching: self = .fetching
        case .found: self = .found
        case .notFound: self = .notFound
        case .blocked: self = .blocked
        default: self = .notFetched
        }
    }
}

/// Local defaults: one JSON array, newest first, capped — an array rather
/// than a dict so the cap evicts the oldest choice instead of an
/// arbitrary one.
struct DefaultsLyricsChoiceStore: LyricsChoiceStore {
    let defaults: UserDefaults
    var key = "lyrics_provider_choices"
    var limit = 300

    private struct Entry: Codable {
        let track: String
        let source: String
    }

    func choice(for trackId: String) -> String? {
        entries().first { $0.track == trackId }?.source
    }

    func remember(_ source: String, for trackId: String) {
        var all = entries()
        all.removeAll { $0.track == trackId }
        all.insert(Entry(track: trackId, source: source), at: 0)
        if all.count > limit { all = Array(all.prefix(limit)) }
        guard let data = try? JSONEncoder().encode(all) else { return }
        defaults.set(data, forKey: key)
    }

    private func entries() -> [Entry] {
        guard let data = defaults.data(forKey: key),
              let decoded = try? JSONDecoder().decode([Entry].self, from: data) else { return [] }
        return decoded
    }
}
