import Foundation
import BitChordShared

// The O9 video side, against YouTube.
//
// The spike proved that the extraction stack's own video selection is VP9/WebM
// for a music video, which AVPlayer cannot decode, and that the same direct
// profile the audio resolves through also answers with AVC/MP4 renditions. The
// build's rule (O9 decisions, 2026-10-24) is therefore: AVC in MP4 only, at most
// 1080p tall, highest bitrate among those, and nothing at all when there is no
// such rendition — the artwork stays rather than a WebM picture.
//
// So: resolve a real music video and an art track, print the rendition (URL
// presence, width×height, itag, heights, the n flag, client), read a real
// 64 KiB window off the music video's URL, and fail if the real video is not a
// served AVC/MP4 16:9-ish picture or if the art track would pass for one. The
// signed URLs themselves are never printed.
//
// Run: scripts/check-video-spike.sh

let ids = ProcessInfo.processInfo.environment["VIDEO_IDS"]?
    .split(separator: ",").map(String.init) ?? [
        // Shape of You (official music video) — 16:9.
        "JGwWNGJdvx8",
        // Shape of You's song entry, i.e. the art track YouTube Music serves
        // from its Songs shelf — a square picture.
        "xTvyyoF_LZY",
    ]

var failures = 0
var checks = 0

func check(_ label: String, _ ok: Bool, _ detail: String = "") {
    checks += 1
    if ok {
        print("  ok   \(label)\(detail.isEmpty ? "" : " — \(detail)")")
    } else {
        failures += 1
        print("  FAIL \(label)\(detail.isEmpty ? "" : " — \(detail)")")
    }
}

func describe(_ side: ResolvedVideoSide) -> String {
    let picture = side.width.flatMap { w in side.height.map { h in "\(w)×\(h)" } } ?? "no picture size"
    let aspect = side.aspectRatio.map { String(format: "%.2f", $0) } ?? "?"
    let itag = side.itag.map(String.init) ?? "?"
    let codecs = side.codecs ?? "?"
    let container = side.mimeType ?? "?"
    let bitrate = side.bitrate.map { "\($0 / 1000)kbps" } ?? "?"
    let heights = side.availableVideoHeights.map(String.init).joined(separator: ",")
    let n = side.hasNParameter ? "n=YES" : "n=no"
    let type = side.musicVideoType ?? "type=?"
    return "\(side.hasVideoURL ? "url present" : "no AVC/MP4 side") · \(picture) aspect \(aspect) · itag \(itag) · "
        + "\(container) \(codecs) \(bitrate) · heights [\(heights)] · \(n) · \(side.clientName)/\(side.profileId) · \(type)"
}

/// Read a real ranged window off the signed URL: a rendition nothing serves is
/// not a video side, however good its fields look.
func servedWindow(_ url: URL) async -> (status: Int?, type: String?, bytes: Int) {
    var request = URLRequest(url: url)
    request.setValue("bytes=0-65535", forHTTPHeaderField: "Range")
    request.timeoutInterval = 30
    do {
        let (data, response) = try await URLSession.shared.data(for: request)
        let http = response as? HTTPURLResponse
        return (http?.statusCode, http?.value(forHTTPHeaderField: "Content-Type"), data.count)
    } catch {
        return (nil, String(describing: error), 0)
    }
}

print("video side: a real music video and an art track, AVC/MP4 only")

for (index, id) in ids.enumerated() {
    let expectsMusicVideo = index == 0
    let label = expectsMusicVideo ? "a real music video (\(id))" : "an art track (\(id))"
    print("  \(label)")
    do {
        let began = Date()
        let side = try await InnertubeStreamResolver.shared.resolveVideo(videoId: id)
        let took = String(format: "%.1fs", Date().timeIntervalSince(began))
        print("    \(describe(side)) — \(took)")

        if expectsMusicVideo {
            check("the music video yields a ready AVC/MP4 video side",
                  side.hasVideoURL && !side.hasNParameter && side.isAVCInMP4,
                  side.hasVideoURL ? (side.isAVCInMP4 ? "AVC/MP4" : "not AVC/MP4") : "no AVC/MP4 side")
            check("its picture is a music video, not an art track",
                  side.looksLikeMusicVideo,
                  side.aspectRatio.map { "aspect \(String(format: "%.2f", $0))" } ?? "no width/height")
            if let string = side.url, let url = URL(string: string) {
                let window = await servedWindow(url)
                check("the rendition is served",
                      (window.status == 206 || window.status == 200) && window.bytes > 0
                        && (window.type ?? "").lowercased().contains("video/mp4"),
                      "\(window.status.map(String.init) ?? "no status") · \(window.type ?? "no type") · \(window.bytes)B")
            } else {
                check("the rendition is served", false, "no URL to read")
            }
        } else {
            // Either answer is a refusal of video: no video side at all, or a
            // picture that is square enough to be the sleeve.
            let qualifies = side.hasVideoURL && side.looksLikeMusicVideo
            check("the art track does not qualify as a music video", !qualifies,
                  qualifies ? "aspect \(String(format: "%.2f", side.aspectRatio ?? 0))" :
                    (side.hasVideoURL ? "aspect \(String(format: "%.2f", side.aspectRatio ?? 0))" : "no video side"))
        }
    } catch {
        // A real video failing here is the finding worth having: the platform
        // gets no picture. An art track failing is a valid answer too.
        if expectsMusicVideo {
            check("the music video yields a ready AVC/MP4 video side", false, error.localizedDescription)
        } else {
            check("the art track does not qualify as a music video", true,
                  "no video side: \(error.localizedDescription)")
        }
    }
}

print(failures == 0 ? "\nall \(checks) checks passed" : "\n\(failures) of \(checks) checks FAILED")
exit(failures == 0 ? 0 : 1)
