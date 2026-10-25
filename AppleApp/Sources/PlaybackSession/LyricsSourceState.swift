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

/// One JSON array, newest first, capped — an array rather than a dict so the
/// cap evicts the oldest choice instead of an arbitrary one.
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
        replace(with: data)
    }

    var snapshot: Data? { defaults.data(forKey: key) }

    func replace(with data: Data) {
        // Reject corrupt/incompatible cloud payloads rather than poisoning the
        // local fallback. The decoder remains backward-compatible with the
        // existing local JSON representation.
        guard (try? JSONDecoder().decode([Entry].self, from: data)) != nil else { return }
        defaults.set(data, forKey: key)
    }

    private func entries() -> [Entry] {
        guard let data = defaults.data(forKey: key),
              let decoded = try? JSONDecoder().decode([Entry].self, from: data) else { return [] }
        return decoded
    }
}

/// iCloud KVS is the cross-device source when populated. Keep the local store
/// authoritative for writes first, and use it whenever cloud data is absent.
/// KVS and UserDefaults are thread-safe; the notification observer only
/// refreshes the persisted local snapshot, with no shared in-memory state.
final class UbiquitousLyricsChoiceStore: LyricsChoiceStore, @unchecked Sendable {
    private let local: DefaultsLyricsChoiceStore
    private let cloud: NSUbiquitousKeyValueStore
    private let key: String
    private var observer: NSObjectProtocol?

    init(
        defaults: UserDefaults = .standard,
        cloud: NSUbiquitousKeyValueStore = .default,
        key: String = "lyrics_provider_choices"
    ) {
        self.local = DefaultsLyricsChoiceStore(defaults: defaults, key: key)
        self.cloud = cloud
        self.key = key
        self.observer = nil
        self.observer = NotificationCenter.default.addObserver(
            forName: NSUbiquitousKeyValueStore.didChangeExternallyNotification,
            object: cloud,
            queue: nil
        ) { [weak self] notification in
            let changedKeys = notification.userInfo?[NSUbiquitousKeyValueStoreChangedKeysKey] as? [String]
            guard changedKeys == nil || changedKeys?.contains(key) == true else { return }
            self?.refreshLocalFromCloud()
        }
        cloud.synchronize()
        refreshLocalFromCloud()
    }

    deinit {
        if let observer { NotificationCenter.default.removeObserver(observer) }
    }

    func choice(for trackId: String) -> String? {
        if let data = cloud.object(forKey: key) as? Data {
            local.replace(with: data)
        }
        return local.choice(for: trackId)
    }

    func remember(_ source: String, for trackId: String) {
        local.remember(source, for: trackId)
        guard let data = local.snapshot else { return }
        cloud.set(data, forKey: key)
        cloud.synchronize()
    }

    private func refreshLocalFromCloud() {
        guard let data = cloud.object(forKey: key) as? Data else { return }
        local.replace(with: data)
    }
}
