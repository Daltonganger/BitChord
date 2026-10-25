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
        let minimumWindowSimilarity: Double

        init(segments: [Segment], confident: Bool, minimumWindowSimilarity: Double) {
            self.segments = segments
            self.confident = confident
            self.minimumWindowSimilarity = minimumWindowSimilarity
        }

        private enum CodingKeys: String, CodingKey {
            case segments, confident, minimumWindowSimilarity
        }

        init(from decoder: Decoder) throws {
            let values = try decoder.container(keyedBy: CodingKeys.self)
            segments = try values.decode([Segment].self, forKey: .segments)
            confident = try values.decode(Bool.self, forKey: .confident)
            // A pre-window map lacks this metric and must be remeasured before
            // being trusted by the video surface.
            minimumWindowSimilarity = try values.decodeIfPresent(Double.self, forKey: .minimumWindowSimilarity) ?? -1
        }

        func encode(to encoder: Encoder) throws {
            var values = encoder.container(keyedBy: CodingKeys.self)
            try values.encode(segments, forKey: .segments)
            try values.encode(confident, forKey: .confident)
            try values.encode(minimumWindowSimilarity, forKey: .minimumWindowSimilarity)
        }

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
        if let data = try? Data(contentsOf: file),
           let saved = try? JSONDecoder().decode(Map.self, from: data),
           saved.minimumWindowSimilarity >= 0 {
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
                try? FileManager.default.removeItem(at: songAudio.appendingPathExtension("part"))
                try? FileManager.default.removeItem(at: videoAudio.appendingPathExtension("part"))
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
                confident: rec.confident,
                minimumWindowSimilarity: rec.minimumWindowSimilarity
            )
            PlaybackDebugLog.shared.record(String(
                format: "video align: %@ %@ in %.1fs (similarity %.2f, baseline %.2f, window minimum %.2f, coverage %.2f): %@",
                videoId, rec.confident ? "aligned" : "NOT confident", Date().timeIntervalSince(started),
                rec.similarity, rec.baseline, rec.minimumWindowSimilarity, rec.coverage, map.summary
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

    /// googlevideo slows one open-ended request down to playback speed, so a
    /// whole song took minutes; bounded 1 MiB ranges arrive at line rate (the
    /// engine's `fetch_loop` does the same).
    private static let chunkBytes = 1 << 20

    private static func download(videoId: String, to destination: URL) async throws {
        if FileManager.default.fileExists(atPath: destination.path) { return }
        let stream = try await InnertubeStreamResolver.shared.resolve(videoId: videoId)
        guard let url = URL(string: stream.url) else { throw URLError(.badURL) }
        let partial = destination.appendingPathExtension("part")
        FileManager.default.createFile(atPath: partial.path, contents: nil)
        let handle = try FileHandle(forWritingTo: partial)
        defer { try? handle.close() }
        var offset = 0
        var total: Int?
        while total.map({ offset < $0 }) ?? true {
            try Task.checkCancellation()
            var request = URLRequest(url: url)
            for (name, value) in stream.headers { request.setValue(value, forHTTPHeaderField: name) }
            request.setValue("bytes=\(offset)-\(offset + chunkBytes - 1)", forHTTPHeaderField: "Range")
            let (data, response) = try await URLSession.shared.data(for: request)
            guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
            if http.statusCode == 416 { break }
            guard (200..<300).contains(http.statusCode), !data.isEmpty else { throw URLError(.badServerResponse) }
            try handle.write(contentsOf: data)
            offset += data.count
            if http.statusCode == 200 { break }
            if total == nil, let range = http.value(forHTTPHeaderField: "Content-Range"),
               let size = range.split(separator: "/").last.flatMap({ Int($0) }) {
                total = size
            }
            if total == nil, data.count < chunkBytes { break }
        }
        try handle.close()
        try? FileManager.default.removeItem(at: destination)
        try FileManager.default.moveItem(at: partial, to: destination)
    }
}
