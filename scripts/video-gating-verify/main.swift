import Foundation

// The O9 picture gate, with fake surfaces and scene values.
//
// Two rules are easy to get wrong and both were: a surface leaving is not the
// scene going inactive, and a surface appearing on a track without a videoId
// must clear the previous one. This pins both, plus the switch and the last
// surface leaving.
//
// Run: scripts/check-video-gating.sh

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

func videoSide(kbpsPerMegapixel: Double, musicVideoType: String?) -> ResolvedVideoSide {
    let width = 1920
    let height = 1080
    let bitrate = Int((kbpsPerMegapixel * 1000 * Double(width * height) / 1_000_000).rounded())
    return ResolvedVideoSide(
        url: "https://example.invalid/video",
        width: width,
        height: height,
        itag: 137,
        mimeType: "video/mp4",
        codecs: "avc1.640028",
        bitrate: bitrate,
        headers: [:],
        availableVideoHeights: [1080],
        hasNParameter: false,
        clientName: "test",
        profileId: "test",
        musicVideoType: musicVideoType
    )
}

let lowMotionOMV = videoSide(kbpsPerMegapixel: 120, musicVideoType: "MUSIC_VIDEO_TYPE_OMV")
check("a low-bitrate OMV qualifies for search", lowMotionOMV.qualifiesAsSearchVideo(isOfficialMusicVideo: true),
      "\(Int(lowMotionOMV.kbpsPerMegapixel ?? 0)) kbps/MP")

let lowBitrateNonOMV = videoSide(kbpsPerMegapixel: 120, musicVideoType: nil)
check("a low-bitrate non-OMV does not qualify for search",
      !lowBitrateNonOMV.qualifiesAsSearchVideo(isOfficialMusicVideo: false),
      "\(Int(lowBitrateNonOMV.kbpsPerMegapixel ?? 0)) kbps/MP")

let unknownTypeOMV = videoSide(kbpsPerMegapixel: 120, musicVideoType: nil)
check("a low-bitrate side qualifies when the search row identifies its OMV type",
      unknownTypeOMV.qualifiesAsSearchVideo(isOfficialMusicVideo: true),
      "\(Int(unknownTypeOMV.kbpsPerMegapixel ?? 0)) kbps/MP")

let staticOMV = videoSide(kbpsPerMegapixel: 30, musicVideoType: "MUSIC_VIDEO_TYPE_OMV")
check("a static OMV qualifies for neither search nor counterpart",
      !staticOMV.qualifiesAsSearchVideo(isOfficialMusicVideo: true) && !staticOMV.qualifiesAsCounterpartVideo(),
      "\(Int(staticOMV.kbpsPerMegapixel ?? 0)) kbps/MP")

var gate = VideoSurfaceState()
gate.setEnabled(true)

gate.surfaceAppeared(videoId: "A")
gate.surfaceAppeared(videoId: "A")
check("two surfaces on a video track allow a request", gate.canRequest && gate.surfaces == 2)

gate.surfaceDisappeared()
check("closing one surface keeps the other's picture", gate.canRequest, "surfaces \(gate.surfaces)")
check("a surface leaving does not change the scene state", gate.active)

gate.setActive(false)
check("a backgrounded app stops requests", !gate.canRequest)
gate.surfaceDisappeared()
check("the last surface leaving does not re-activate the scene", !gate.active && !gate.canRequest)
gate.setActive(true)
check("returning to the foreground with no surface still stops", !gate.canRequest)

gate.surfaceAppeared(videoId: "A")
gate.surfaceAppeared(videoId: nil)
check("a surface appearing on a file clears the previous video id",
      gate.trackId == nil && !gate.canRequest, "track \(gate.trackId ?? "nil")")

gate.setTrack("B")
check("a video track set explicitly allows a request", gate.canRequest)
gate.setEnabled(false)
check("the switch off stops requests", !gate.canRequest)
gate.setEnabled(true)
check("the switch on allows a request again", gate.canRequest)

gate.setTrack("")
check("an empty video id is not a request", !gate.canRequest)

print(failures == 0 ? "\nall \(checks) checks passed" : "\n\(failures) of \(checks) checks FAILED")
exit(failures == 0 ? 0 : 1)
