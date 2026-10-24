import Foundation
import BitChordShared

// The O9 music-video spike, against YouTube.
//
// The audit's §O9 item 8 asks one question a fixture cannot answer: does the
// client the app actually gets served deliver *video-only* formats with ready
// URLs for real videos? The extractor carries video fields already, but the
// playback path has always asked with `wantVideo = false`, so nothing has ever
// looked. The risk is the n parameter: the extractor's own readiness rule
// rejects a URL that carries one (InnerTubeExtractor.hasReadyVideoUrl), and a
// SABR-only answer fails the progressive check entirely — both of which read
// as "no video" from outside unless the probe reports them.
//
// So: probe a real music video and an art track, print what the extractor got
// (URL presence, width×height, itag, heights, the n flag, client), and fail if
// the real video does not come back ready and 16:9-ish, or if the art track
// would pass for one. The signed URLs themselves are never printed.
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

func describe(_ probe: ResolvedVideoProbe) -> String {
    let picture = probe.width.flatMap { w in probe.height.map { h in "\(w)×\(h)" } } ?? "no picture size"
    let aspect = probe.aspectRatio.map { String(format: "%.2f", $0) } ?? "?"
    let itag = probe.itag.map(String.init) ?? "?"
    let codecs = probe.codecs ?? "?"
    let container = probe.mimeType ?? "?"
    let bitrate = probe.bitrate.map { "\($0 / 1000)kbps" } ?? "?"
    let heights = probe.availableVideoHeights.map(String.init).joined(separator: ",")
    let n = probe.hasNParameter ? "n=YES" : "n=no"
    let type = probe.musicVideoType ?? "type=?"
    return "\(probe.hasVideoURL ? "url present" : "no url") · \(picture) aspect \(aspect) · itag \(itag) · "
        + "\(container) \(codecs) \(bitrate) · heights [\(heights)] · \(n) · \(probe.clientName)/\(probe.profileId) · \(type) · \(probe.selection)"
}

print("video spike: the video side of a real music video and an art track")

for (index, id) in ids.enumerated() {
    let expectsMusicVideo = index == 0
    let label = expectsMusicVideo ? "a real music video (\(id))" : "an art track (\(id))"
    print("  \(label)")
    do {
        let began = Date()
        let probe = try await InnertubeStreamResolver.shared.probeVideo(videoId: id)
        let took = String(format: "%.1fs", Date().timeIntervalSince(began))
        print("    \(describe(probe)) — \(took)")

        if expectsMusicVideo {
            check("the music video yields a ready video URL",
                  probe.hasVideoURL && !probe.hasNParameter,
                  probe.hasVideoURL ? (probe.hasNParameter ? "URL carries n" : "URL ready") : "no URL")
            check("its picture is a music video, not an art track",
                  probe.looksLikeMusicVideo,
                  probe.aspectRatio.map { "aspect \(String(format: "%.2f", $0))" } ?? "no width/height")
        } else {
            // Either answer is a refusal of video: no URL at all, or a
            // picture that is square enough to be the sleeve.
            let qualifies = probe.hasVideoURL && probe.looksLikeMusicVideo
            check("the art track does not qualify as a music video", !qualifies,
                  qualifies ? "aspect \(String(format: "%.2f", probe.aspectRatio ?? 0))" :
                    (probe.hasVideoURL ? "aspect \(String(format: "%.2f", probe.aspectRatio ?? 0))" : "no video answer"))
        }
    } catch {
        // A real video failing here is the spike's headline finding: the
        // progressive client the app uses does not serve video. An art track
        // failing is a valid answer too — there is nothing to show.
        if expectsMusicVideo {
            check("the music video yields a ready video URL", false, error.localizedDescription)
        } else {
            check("the art track does not qualify as a music video", true,
                  "no video answer: \(error.localizedDescription)")
        }
    }
}

print(failures == 0 ? "\nall \(checks) checks passed" : "\n\(failures) of \(checks) checks FAILED")
exit(failures == 0 ? 0 : 1)
