import Foundation

/// Queue snapshot so a cold start opens on the track that was current —
/// upstream `LastPlayed`. Restored idle: no stream resolve until Play.
enum LastPlayed {
    struct Snapshot {
        var tracks: [QueueEntry]
        var index: Int
        var position: Double
        var repeatMode: PlaybackController.RepeatMode
        var shuffleEnabled: Bool
        var volume: Double
        var contextID: String?
        var contextTitle: String?
        var contextEntries: [QueueEntry]
        var orderedContextEntries: [QueueEntry]
        var contextContinuation: String?
        var contextCursor: Int
        var contextBaseIndex: Int
    }

    private static let key = "bitchord_last_played"
    private static let backupKey = "bitchord_last_played_previous"
    private static let keepBehind = 10
    private static let maxTracks = 60

    static func save(
        tracks: [QueueEntry],
        index: Int,
        position: Double,
        repeatMode: PlaybackController.RepeatMode,
        shuffleEnabled: Bool,
        volume: Double,
        contextID: String? = nil,
        contextTitle: String? = nil,
        contextEntries: [QueueEntry] = [],
        orderedContextEntries: [QueueEntry] = [],
        contextContinuation: String? = nil,
        contextCursor: Int = 0,
        contextBaseIndex: Int = 0,
        defaults: UserDefaults = .standard
    ) {
        guard !tracks.isEmpty, tracks.indices.contains(index) else { return }
        let start = max(0, min(index - keepBehind, max(0, tracks.count - maxTracks)))
        let end = min(tracks.count, start + maxTracks)
        let window = Array(tracks[start..<end])
        let safeSeconds = position.isFinite ? min(max(0, position), Double(Int64.max / 4) / 1000) : 0
        let droppedContextRows = tracks.prefix(start).filter { $0.contextOrder != nil }.count
        let stored = Stored(
            tracks: window.map(StoredTrack.init),
            index: (index - start).clamped(to: 0...max(0, window.count - 1)),
            positionMs: Int64(safeSeconds * 1000),
            repeatMode: repeatMode.rawValue,
            shuffle: shuffleEnabled,
            volume: boundedVolume(volume),
            contextID: contextID,
            contextTitle: contextTitle,
            contextEntries: contextEntries.map(StoredTrack.init),
            orderedContextEntries: orderedContextEntries.map(StoredTrack.init),
            contextContinuation: contextContinuation,
            contextCursor: contextCursor,
            contextBaseIndex: max(0, contextBaseIndex) + droppedContextRows
        )
        if let data = try? JSONEncoder().encode(stored) {
            if let previous = defaults.data(forKey: key),
               let decoded = try? JSONDecoder().decode(Stored.self, from: previous), !decoded.tracks.isEmpty {
                defaults.set(previous, forKey: backupKey)
            }
            defaults.set(data, forKey: key)
        }
    }

    static func load(defaults: UserDefaults = .standard) -> Snapshot? {
        let stored = [key, backupKey].lazy.compactMap { name -> Stored? in
            guard let data = defaults.data(forKey: name),
                  let decoded = try? JSONDecoder().decode(Stored.self, from: data),
                  !decoded.tracks.isEmpty else { return nil }
            return decoded
        }.first
        guard let stored else { return nil }
        let tracks = stored.tracks.map(\.entry)
        return Snapshot(
            tracks: tracks,
            index: stored.index.clamped(to: 0...max(0, tracks.count - 1)),
            position: Double(max(0, stored.positionMs)) / 1000,
            repeatMode: PlaybackController.RepeatMode(rawValue: stored.repeatMode) ?? .off,
            shuffleEnabled: stored.shuffle,
            volume: boundedVolume(stored.volume),
            contextID: stored.contextID,
            contextTitle: stored.contextTitle,
            contextEntries: stored.contextEntries?.map(\.entry) ?? [],
            orderedContextEntries: stored.orderedContextEntries?.map(\.entry) ?? [],
            contextContinuation: stored.contextContinuation,
            contextCursor: stored.contextCursor ?? 0,
            contextBaseIndex: stored.contextBaseIndex ?? 0
        )
    }

    private static func boundedVolume(_ value: Double) -> Double {
        value.isFinite ? min(max(value, 0), 1) : 0.9
    }

    private struct Stored: Codable {
        var tracks: [StoredTrack]
        var index: Int
        var positionMs: Int64
        var repeatMode: Int
        var shuffle: Bool
        var volume: Double
        var contextID: String?
        var contextTitle: String?
        var contextEntries: [StoredTrack]?
        var orderedContextEntries: [StoredTrack]?
        var contextContinuation: String?
        var contextCursor: Int?
        var contextBaseIndex: Int?
    }

    private struct StoredTrack: Codable {
        var id: String
        var title: String
        var artist: String
        var source: String
        var thumbnailUrl: String?
        var durationText: String?
        var albumName: String?
        var isLocal: Bool
        var fromAutoplay: Bool
        var artistId: String?
        var albumId: String?
        var setVideoId: String?
        var contextOrder: Int?
        var isVideo: Bool?
        var isExplicit: Bool?

        init(_ entry: QueueEntry) {
            id = entry.id
            title = entry.title
            artist = entry.artist
            source = entry.source
            thumbnailUrl = entry.thumbnailUrl
            durationText = entry.durationText
            albumName = entry.albumName
            isLocal = entry.isLocal
            fromAutoplay = entry.fromAutoplay
            artistId = entry.artistId
            albumId = entry.albumId
            setVideoId = entry.setVideoId
            contextOrder = entry.contextOrder
            isVideo = entry.isVideo
            isExplicit = entry.isExplicit
        }

        var entry: QueueEntry {
            QueueEntry(
                id: id, title: title, artist: artist, source: source,
                thumbnailUrl: thumbnailUrl, durationText: durationText,
                albumName: albumName, artworkData: nil, isLocal: isLocal,
                fromAutoplay: fromAutoplay, artistId: artistId, albumId: albumId,
                setVideoId: setVideoId, contextOrder: contextOrder, isExplicit: isExplicit, isVideo: isVideo ?? false
            )
        }
    }
}

private extension Int {
    func clamped(to range: ClosedRange<Int>) -> Int { Swift.min(Swift.max(self, range.lowerBound), range.upperBound) }
}
