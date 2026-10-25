import Foundation
import BitChordShared

private struct ProbeSong {
    enum Expectation { case officialVideo, lyricOnly, artTrack, noVideo, either }
    let title: String
    let artist: String
    let expectation: Expectation
}

private struct SearchHit: Decodable {
    let kind: String
    let videoId: String?
    let title: String
    let durationText: String?
}

private struct Candidate: Decodable {
    let videoId: String
    let title: String
    let durationText: String?
    let type: String?
    let source: String?
}

private final class SearchAdapter: SearchBridgeSearchCallback {
    let finish: (String?, String?) -> Void
    init(_ finish: @escaping (String?, String?) -> Void) { self.finish = finish }
    func onResult(json: String?, continuation: String?, message: String?) { finish(json, message) }
}

private final class VideoAdapter: VideoVersionBridgeVideoVersionCallback {
    let finish: (String?, String?) -> Void
    init(_ finish: @escaping (String?, String?) -> Void) { self.finish = finish }
    func onResult(json: String?, message: String?) { finish(json, message) }
}

private func titleCore(_ raw: String) -> String {
    let withoutBrackets = raw.replacingOccurrences(
        of: #"\s*[\(\[].*?[\)\]]"#,
        with: " ",
        options: .regularExpression
    )
    let withoutTrailingAside = withoutBrackets.replacingOccurrences(
        of: #"\s+[-–—|]\s+.*$"#,
        with: " ",
        options: .regularExpression
    )
    let folded = withoutTrailingAside.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: .current)
    return folded.unicodeScalars.filter { CharacterSet.alphanumerics.contains($0) }
        .map { String($0) }.joined()
}

private func searchSong(_ item: ProbeSong) async throws -> SearchHit {
    try await withCheckedThrowingContinuation { continuation in
        SearchBridge.shared.search(
            query: "\(item.title) \(item.artist)",
            scope: "SONGS",
            callback: SearchAdapter { json, message in
                guard let json,
                      let hits = try? JSONDecoder().decode([SearchHit].self, from: Data(json.utf8)),
                      let hit = hits.first(where: {
                          $0.kind == "track" && $0.videoId != nil && titleCore($0.title) == titleCore(item.title)
                      }) else {
                    continuation.resume(throwing: NSError(
                        domain: "VideoVersionsVerify", code: 1,
                        userInfo: [NSLocalizedDescriptionKey: message ?? "song row not found"]
                    ))
                    return
                }
                continuation.resume(returning: hit)
            }
        )
    }
}

private func findVersions(_ song: SearchHit, artist: String) async throws -> [Candidate] {
    guard let videoId = song.videoId else { return [] }
    return try await withCheckedThrowingContinuation { continuation in
        VideoVersionBridge.shared.find(
            videoId: videoId,
            title: song.title,
            artist: artist,
            durationText: song.durationText,
            callback: VideoAdapter { json, message in
                guard let json,
                      let candidates = try? JSONDecoder().decode([Candidate].self, from: Data(json.utf8)) else {
                    continuation.resume(throwing: NSError(
                        domain: "VideoVersionsVerify", code: 2,
                        userInfo: [NSLocalizedDescriptionKey: message ?? "Candidate response was invalid"]
                    ))
                    return
                }
                continuation.resume(returning: candidates)
            }
        )
    }
}

// Lyric-only results and an art track are negative cases. Keep the list fixed
// so results are comparable across verifier runs.
private let songs: [ProbeSong] = [
    .init(title: "Shape of You", artist: "Ed Sheeran", expectation: .officialVideo),
    .init(title: "Bad Romance", artist: "Lady Gaga", expectation: .officialVideo),
    .init(title: "Thriller", artist: "Michael Jackson", expectation: .officialVideo),
    .init(title: "Rolling in the Deep", artist: "Adele", expectation: .officialVideo),
    .init(title: "Uptown Funk", artist: "Mark Ronson Bruno Mars", expectation: .officialVideo),
    .init(title: "Gangnam Style", artist: "PSY", expectation: .officialVideo),
    .init(title: "Despacito", artist: "Luis Fonsi Daddy Yankee", expectation: .officialVideo),
    .init(title: "Blank Space", artist: "Taylor Swift", expectation: .officialVideo),
    .init(title: "Hello", artist: "Adele", expectation: .officialVideo),
    .init(title: "Chandelier", artist: "Sia", expectation: .officialVideo),
    .init(title: "Baby", artist: "Justin Bieber", expectation: .officialVideo),
    .init(title: "Take On Me", artist: "a-ha", expectation: .officialVideo),
    // Signed-out search currently lists only the live Wembley OMV; studio search
    // should not claim it as the studio recording.
    .init(title: "Bohemian Rhapsody", artist: "Queen", expectation: .either),
    .init(title: "Blinding Lights", artist: "The Weeknd", expectation: .officialVideo),
    .init(title: "Bad Guy", artist: "Billie Eilish", expectation: .officialVideo),
    // Shayne Ward has no OMV in the signed-out catalogue. Never substitute
    // Jay Sean's same-title video.
    .init(title: "Down", artist: "Shayne Ward", expectation: .noVideo),
    .init(title: "The Night We Met", artist: "Lord Huron", expectation: .officialVideo),
    .init(title: "Space Song", artist: "Beach House", expectation: .either),
    .init(title: "Ribs", artist: "Lorde", expectation: .either),
    .init(title: "Gymnopédie No. 1", artist: "Erik Satie", expectation: .artTrack),
]

guard CommandLine.arguments.count > 1 else {
    fputs("usage: video-versions-verify <YouTubeSolver directory>\n", stderr)
    exit(2)
}
CipherUnlockWiring.install(player: YouTubePlayerJs(solverDirectory: URL(fileURLWithPath: CommandLine.arguments[1])))
await MainActor.run { ApplePoTokenWiring.install() }

var failures = 0
for item in songs {
    do {
        let song = try await searchSong(item)
        let candidates = try await findVersions(song, artist: item.artist)
        var chosen: Candidate?
        var chosenMap: VideoVersionAlignment.Map?
        var chosenIsQualified = false
        var aligned = false
        var unqualifiedFallback: (candidate: Candidate, map: VideoVersionAlignment.Map?)?
        for candidate in candidates.prefix(3) {
            let side: ResolvedVideoSide?
            do {
                side = try await InnertubeStreamResolver.shared.resolveVideo(videoId: candidate.videoId)
            } catch {
                print("  candidate \(candidate.videoId) resolve failed: \(error.localizedDescription)")
                side = nil
            }

            let map: VideoVersionAlignment.Map?
            if let songId = song.videoId {
                map = await VideoVersionAlignment.shared.map(songId: songId, videoId: candidate.videoId)
            } else {
                map = nil
            }
            guard let side else {
                if unqualifiedFallback == nil { unqualifiedFallback = (candidate, map) }
                continue
            }
            let qualifies = candidate.source == "counterpart"
                ? side.qualifiesAsCounterpartVideo()
                : side.qualifiesAsSearchVideo(isOfficialMusicVideo: candidate.type == "MUSIC_VIDEO_TYPE_OMV")
            guard qualifies else { continue }

            if chosen == nil {
                chosen = candidate
                chosenMap = map
                chosenIsQualified = true
            }
            if let map, map.confident {
                chosen = candidate
                chosenMap = map
                chosenIsQualified = true
                aligned = true
                break
            }
        }
        if chosen == nil, let unqualifiedFallback {
            chosen = unqualifiedFallback.candidate
            chosenMap = unqualifiedFallback.map
        }
        var alignment = "map=none confident=no min-window=?"
        if let map = chosenMap {
            alignment = "map=\(map.summary) confident=\(map.confident ? "yes" : "no") min-window=\(String(format: "%.3f", map.minimumWindowSimilarity))"
        }
        let candidateText: String
        if let chosen {
            candidateText = "\(chosen.videoId) type=\(chosen.type ?? "?") source=\(chosen.source ?? "search") title=\(chosen.title)"
        } else {
            candidateText = "none type=- source=none title=-"
        }
        let selectionStatus = chosen == nil ? "no candidate" : !chosenIsQualified ? "unqualified" : aligned ? "aligned" : "own audio"
        print("\(item.title) — \(item.artist) [song=\(song.title), duration=\(song.durationText ?? "?")] → \(candidateText) · \(selectionStatus) · \(alignment)")

        switch item.expectation {
        case .officialVideo:
            if chosen?.type != "MUSIC_VIDEO_TYPE_OMV" { failures += 1 }
        case .lyricOnly, .artTrack:
            if chosen != nil { failures += 1 }
        case .noVideo:
            if chosen != nil { failures += 1 }
        case .either:
            break
        }
    } catch {
        // Keep diagnostics to metadata only; transport errors can contain
        // signed request details and are left for the local debug log.
        print("\(item.title) — \(item.artist) → none type=- source=none title=- · map=none confident=no min-window=? · ERROR")
        failures += 1
    }
}

print(failures == 0 ? "all 20 video-version cases passed" : "\(failures) of 20 video-version cases failed")
exit(failures == 0 ? 0 : 1)
