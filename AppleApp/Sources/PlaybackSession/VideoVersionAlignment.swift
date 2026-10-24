import Foundation

/// Where a song's moments fall in its music video, for the video toggle.
///
/// A music video is rarely the song's cut: "Money" opens 3.7 s earlier and
/// stops for an 11.6 s interlude at 1:23. One clock offset would put the
/// picture eleven seconds off for the second half. The engine's
/// `align_versions` maps the whole song, segment by segment; this fetches the
/// video's audio, runs it once per pair and keeps the answer, so the picture
/// can follow the song's own audio through an intro and past an interlude.
actor VideoVersionAlignment {
    static let shared = VideoVersionAlignment()

    struct Map: Codable, Sendable, Equatable {
        struct Segment: Codable, Sendable, Equatable {
            let songStart: Double
            let songEnd: Double
            let offset: Double
        }
        let segments: [Segment]
        let confident: Bool

        /// Video time for a song time. Before the first segment and after the
        /// last, the nearest segment's offset holds.
        func videoTime(at song: Double) -> Double {
            if let segment = segments.first(where: { song >= $0.songStart && song < $0.songEnd }) {
                return song + segment.offset
            }
            let nearest = song < (segments.first?.songStart ?? 0) ? segments.first : segments.last
            return song + (nearest?.offset ?? 0)
        }

        var summary: String {
            segments.map { String(format: "%.0f–%.0fs %+.2fs", $0.songStart, $0.songEnd, $0.offset) }
                .joined(separator: ", ")
        }
    }

    private var maps: [String: Map] = [:]
    private var inFlight: [String: Task<Map?, Never>] = [:]

    private let directory: URL = {
        let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
        let url = caches.appendingPathComponent("BitChord/VersionAlignment", isDirectory: true)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }()

    /// The map for this pair, measuring it if no earlier play did. Nil when the
    /// song never finished downloading or the video's audio could not be had.
    func map(songId: String, videoId: String) async -> Map? {
        let key = "\(songId)_\(videoId)"
        if let known = maps[key] { return known }
        let file = directory.appendingPathComponent("\(key).json")
        if let data = try? Data(contentsOf: file), let saved = try? JSONDecoder().decode(Map.self, from: data) {
            maps[key] = saved
            return saved
        }
        if let running = inFlight[key] { return await running.value }
        let task = Task<Map?, Never> { [directory] in
            // Both recordings are fetched here, side by side and at full
            // speed: waiting for playback to finish downloading the song kept
            // the picture off screen for most of the track.
            let songAudio = directory.appendingPathComponent("\(songId).song.audio")
            let videoAudio = directory.appendingPathComponent("\(videoId).audio")
            defer {
                // The recordings are only inputs: the map is what is kept.
                try? FileManager.default.removeItem(at: songAudio)
                try? FileManager.default.removeItem(at: videoAudio)
            }
            let fetchStarted = Date()
            do {
                async let song: Void = Self.download(videoId: songId, to: songAudio)
                async let video: Void = Self.download(videoId: videoId, to: videoAudio)
                _ = try await (song, video)
            } catch {
                PlaybackDebugLog.shared.record("video align: the audio for \(videoId) failed: \(error)", about: songId)
                return nil
            }
            let songPath = songAudio.path
            PlaybackDebugLog.shared.record(String(
                format: "video align: both recordings fetched in %.1fs", Date().timeIntervalSince(fetchStarted)
            ), about: songId)
            let started = Date()
            let rec = await Task.detached(priority: .utility) {
                alignVersions(songPath: songPath, otherPath: videoAudio.path)
            }.value
            guard let rec else {
                PlaybackDebugLog.shared.record("video align: \(videoId) could not be decoded", about: songId)
                return nil
            }
            let map = Map(
                segments: rec.segments.map { .init(songStart: $0.songStart, songEnd: $0.songEnd, offset: $0.offset) },
                confident: rec.confident
            )
            PlaybackDebugLog.shared.record(String(
                format: "video align: %@ %@ in %.1fs (similarity %.2f, baseline %.2f, coverage %.2f): %@",
                videoId, rec.confident ? "aligned" : "NOT confident", Date().timeIntervalSince(started),
                rec.similarity, rec.baseline, rec.coverage, map.summary
            ), about: songId)
            if let data = try? JSONEncoder().encode(map) { try? data.write(to: file, options: .atomic) }
            return map
        }
        inFlight[key] = task
        let map = await task.value
        inFlight[key] = nil
        if let map { maps[key] = map }
        return map
    }

    private static func download(videoId: String, to destination: URL) async throws {
        if FileManager.default.fileExists(atPath: destination.path) { return }
        let stream = try await InnertubeStreamResolver.shared.resolve(videoId: videoId)
        guard let url = URL(string: stream.url) else { throw URLError(.badURL) }
        var request = URLRequest(url: url)
        for (name, value) in stream.headers { request.setValue(value, forHTTPHeaderField: name) }
        let (temporary, response) = try await URLSession.shared.download(for: request)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            try? FileManager.default.removeItem(at: temporary)
            throw URLError(.badServerResponse)
        }
        try? FileManager.default.removeItem(at: destination)
        try FileManager.default.moveItem(at: temporary, to: destination)
    }
}
