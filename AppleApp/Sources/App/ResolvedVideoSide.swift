import Foundation

/// The track's AVC/MP4 video side: one rendition the Apple picture player can
/// decode, or nil fields when the track has no usable video stream.
struct ResolvedVideoSide: Sendable {
    /// Signed and short-lived: report whether it exists, never print it.
    let url: String?
    let width: Int?
    let height: Int?
    let itag: Int?
    let mimeType: String?
    let codecs: String?
    let bitrate: Int?
    let headers: [String: String]
    let availableVideoHeights: [Int]
    /// The extractor's readiness rule rejects a URL carrying YouTube's `n`
    /// throttle parameter; the side reports it instead of hiding the reason.
    let hasNParameter: Bool
    let clientName: String
    let profileId: String
    let musicVideoType: String?

    var hasVideoURL: Bool { !(url ?? "").isEmpty }
    var aspectRatio: Double? {
        guard let width, let height, height > 0 else { return nil }
        return Double(width) / Double(height)
    }
    /// A square picture is an art track, not a music video (upstream's
    /// `MIN_VIDEO_ASPECT = 1.2`, engine.ts:47).
    var looksLikeMusicVideo: Bool { (aspectRatio ?? 0) >= 1.2 }
    /// AVC bitrate helps identify a still upload, but low-motion official videos
    /// can also be cheap: The Night We Met is about 120 kbps/MP, while an actual
    /// still upload measured 328 kbps/MP. Search OMVs use the much lower static
    /// threshold; non-OMV search results keep the more conservative gate.
    var isStillImage: Bool {
        guard let kbpsPerMegapixel else { return false }
        return kbpsPerMegapixel < 700
    }
    /// Counterparts are authoritative, except for a genuinely static picture.
    /// A still AVC image costs a few tens of kbps/MP; do not reject low-motion
    /// music videos in the hundreds of kbps/MP.
    var isTrulyStatic: Bool {
        guard let kbpsPerMegapixel else { return false }
        return kbpsPerMegapixel < 50
    }
    var kbpsPerMegapixel: Double? {
        guard let bitrate, let width, let height, width > 0, height > 0 else { return nil }
        return Double(bitrate) / 1000 / (Double(width * height) / 1_000_000)
    }
    var isAuthoritativeMusicVideo: Bool {
        musicVideoType == "MUSIC_VIDEO_TYPE_OMV"
    }
    func qualifiesAsCounterpartVideo() -> Bool {
        hasVideoURL && isAVCInMP4 && !isTrulyStatic
    }
    func qualifiesAsSearchVideo(isOfficialMusicVideo: Bool) -> Bool {
        let hasMovingPicture = (isOfficialMusicVideo || isAuthoritativeMusicVideo) ? !isTrulyStatic : !isStillImage
        return hasVideoURL && isAVCInMP4 && looksLikeMusicVideo && hasMovingPicture
    }
    /// AVC in MP4 is the only container/codec pair AVFoundation decodes here.
    var isAVCInMP4: Bool {
        let mime = mimeType?.lowercased() ?? ""
        return mime.contains("video/mp4") && !mime.contains("mp4a")
            && (codecs?.lowercased().contains("avc1") ?? false)
    }
}
