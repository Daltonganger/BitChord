import Foundation
import BitChordShared

/// A resolved YouTube Music stream: the URL the engine fetches plus the
/// headers the CDN demands and the quality description for display.
struct ResolvedYouTubeStream {
    let url: String
    let kbps: Int
    let mimeType: String
    let headers: [String: String]
    /// Player-response loudness figure for the engine's normalization stage
    /// (nil when the response carried none, or the payload predates it).
    let durationSeconds: Int?
    let loudnessDb: Double?
}

/// What the extraction stack could see of a track's video side.
///
/// This is the O9 spike's answer, not the playback path: `wantVideo` stays
/// false in `extract`, and only this method asks for video formats. A thrown
/// error from the bridge means the progressive stack would not deliver a
/// video-only stream at all; a successful one is the extractor's own fields.
struct ResolvedVideoProbe: Sendable {
    /// Signed and short-lived: report whether it exists, never print it.
    let videoUrl: String?
    let width: Int?
    let height: Int?
    let itag: Int?
    let mimeType: String?
    let codecs: String?
    let bitrate: Int?
    let availableVideoHeights: [Int]
    /// The extractor's readiness rule rejects a URL carrying YouTube's `n`
    /// throttle parameter; the probe reports it instead of hiding the reason.
    let hasNParameter: Bool
    let clientName: String
    let profileId: String
    let musicVideoType: String?
    /// Which ask produced this answer: the audio path's own direct profile
    /// (by override) or the automatic video selection.
    let selection: String

    var hasVideoURL: Bool { !(videoUrl ?? "").isEmpty }
    var aspectRatio: Double? {
        guard let width, let height, height > 0 else { return nil }
        return Double(width) / Double(height)
    }
    /// A square picture is an art track, not a music video (upstream's
    /// `MIN_VIDEO_ASPECT = 1.2`, engine.ts:47).
    var looksLikeMusicVideo: Bool { (aspectRatio ?? 0) >= 1.2 }
}

struct VideoProbeError: Error, LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

/// Swift bridge over the shared module's `StreamResolver` (via `PlayerBridge`):
/// resolves a videoId to a playable audio URL. The resolved payload crosses FFI
/// as JSON.
///
/// The resolution *policy* — which identities to ask, what to remember between
/// attempts, when to stand one down, and the signed-in retries — lives in the
/// shared module, mirroring upstream's own split. This is only the seam.
final class InnertubeStreamResolver: Sendable {
    static let shared = InnertubeStreamResolver()

    func resolve(videoId: String, maxKbps: Int = Int.max) async throws -> ResolvedYouTubeStream {
        let request = ResolutionRequest()
        let payload: ResolvedYouTubeStream = try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                request.install(continuation)
                guard !Task.isCancelled else { request.cancel(); return }
                let id = PlayerBridge.shared.resolveRequest(
                    videoId: videoId,
                    maxKbps: Swift.Int32(clamping: maxKbps == Int.max ? Int(Swift.Int32.max) : maxKbps),
                    callback: ResolveCallbackAdapter { json, message in
                        if let json {
                            do {
                                let decoded = try JSONDecoder().decode(StreamPayload.self, from: Data(json.utf8))
                                request.finish(.success(ResolvedYouTubeStream(url: decoded.url, kbps: decoded.kbps,
                                    mimeType: decoded.mimeType, headers: decoded.headers, durationSeconds: decoded.durationSeconds, loudnessDb: decoded.loudnessDb)))
                            } catch { request.finish(.failure(error)) }
                        } else {
                            let raw = message ?? "Stream resolution failed"
                            request.finish(.failure(StreamError(message: StreamError.say(raw), raw: raw)))
                        }
                    })
                request.setID(id)
            }
        } onCancel: { request.cancel() }

        // The shared resolver transforms both direct and ciphered URLs before
        // probing. Applying the n transform again corrupts a verified URL.
        return payload
    }

    /// Ask the shared extractor what the video side of a track is, without
    /// changing what playback resolves (the O9 spike).
    func probeVideo(videoId: String) async throws -> ResolvedVideoProbe {
        try await withCheckedThrowingContinuation { continuation in
            PlayerBridge.shared.probeVideo(
                videoId: videoId,
                callback: VideoProbeCallbackAdapter { json, message in
                    guard let json else {
                        continuation.resume(throwing: VideoProbeError(
                            message: message ?? "Video probe failed"
                        ))
                        return
                    }
                    do {
                        let payload = try JSONDecoder().decode(VideoProbePayload.self, from: Data(json.utf8))
                        continuation.resume(returning: ResolvedVideoProbe(
                            videoUrl: payload.videoUrl,
                            width: payload.videoWidth,
                            height: payload.videoHeight,
                            itag: payload.videoItag,
                            mimeType: payload.videoMimeType,
                            codecs: payload.videoCodecs,
                            bitrate: payload.videoBitrate,
                            availableVideoHeights: payload.availableVideoHeights ?? [],
                            hasNParameter: payload.videoUrlHasNParameter ?? false,
                            clientName: payload.clientName,
                            profileId: payload.profileId,
                            musicVideoType: payload.musicVideoType,
                            selection: payload.selection
                        ))
                    } catch {
                        continuation.resume(throwing: error)
                    }
                }
            )
        }
    }

    struct StreamError: Error, LocalizedError {
        /// The sentence a listener sees.
        let message: String
        /// What the shared module said, in full.
        ///
        /// Kept rather than discarded because it is the only place the per-client
        /// reasons exist and they are what a bug report needs. Never shown: it
        /// contains a video id and internal client names.
        let raw: String

        init(message: String, raw: String? = nil) {
            self.message = message
            self.raw = raw ?? message
        }

        var errorDescription: String? { message }

        /// What to show a listener, from whatever the shared module said.
        ///
        /// The shared module's message names the track and every client that refused
        /// it, which is the right thing for a log and the wrong thing to put in front
        /// of somebody: a video id and a list of internal client names is not a
        /// explanation. The two cases that are not "this one track is being awkward"
        /// get their own sentences, because they are the two a listener can do
        /// something about — and everything else gets one honest sentence rather than
        /// five words that name a subsystem.
        ///
        /// Same rule as the party screen's `describe`: the server's own words when it
        /// sent any, otherwise a statement of what failed and never the raw error.
        static func say(_ raw: String) -> String {
            let said = raw.lowercased()
            if said.contains("signed-in playback:") {
                return "Your signed-in session could not be restored. Check your connection or sign in again."
            }
            if said.contains("temporarily unavailable") {
                return "Playback is temporarily unavailable. Try again shortly."
            }
            if said.contains("sign in") || said.contains("not a bot") {
                return "YouTube is asking this device to sign in before it will serve anything. That is YouTube's gate rather than anything to do with the track."
            }
            if said.contains("needs to be reloaded") || said.contains("page needs") {
                return "YouTube's player refused this connection. Trying again in a moment usually clears it."
            }
            if said.contains("unavailable") || said.contains("not available") {
                return "That track isn’t available right now."
            }
            if said.contains("no signature timestamp") {
                return "BitChord could not read YouTube’s player, so it could not open a stream. Check the connection and try again."
            }
            return "Couldn’t open a stream for that track."
        }
    }
}

private struct StreamPayload: Codable {
    let url: String
    let kbps: Int
    let mimeType: String
    let headers: [String: String]
    /// Nil on payloads minted before the shared module parsed the figure.
    let durationSeconds: Int?
    let loudnessDb: Double?

    private enum CodingKeys: String, CodingKey { case url, kbps, mimeType, headers, loudnessDb, durationSeconds }
    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        url = try values.decode(String.self, forKey: .url)
        kbps = try values.decode(Int.self, forKey: .kbps)
        mimeType = try values.decode(String.self, forKey: .mimeType)
        // Kotlin omits empty default maps. Some upstream profiles correctly
        // need no CDN headers, so an omitted map is a valid empty map.
        headers = try values.decodeIfPresent([String: String].self, forKey: .headers) ?? [:]
        durationSeconds = try values.decodeIfPresent(Int.self, forKey: .durationSeconds)
        loudnessDb = try values.decodeIfPresent(Double.self, forKey: .loudnessDb)
    }
}

private final class ResolveCallbackAdapter: PlayerBridgeResolveCallback {
    private let onResult: (String?, String?) -> Void

    init(onResult: @escaping (String?, String?) -> Void) {
        self.onResult = onResult
    }

    func onResult(json: String?, message: String?) {
        onResult(json, message)
    }
}

private struct VideoProbePayload: Decodable {
    let videoUrl: String?
    let videoWidth: Int?
    let videoHeight: Int?
    let videoItag: Int?
    let videoMimeType: String?
    let videoCodecs: String?
    let videoBitrate: Int?
    /// Kotlin omits an empty default list.
    let availableVideoHeights: [Int]?
    let videoUrlHasNParameter: Bool?
    let clientName: String
    let profileId: String
    let musicVideoType: String?
    let selection: String
}

private final class VideoProbeCallbackAdapter: PlayerBridgeProbeCallback {
    private let onResult: (String?, String?) -> Void

    init(onResult: @escaping (String?, String?) -> Void) {
        self.onResult = onResult
    }

    func onResult(json: String?, message: String?) {
        onResult(json, message)
    }
}

/// Cancellation resumes Swift immediately and releases its shared Kotlin flight.
private final class ResolutionRequest: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<ResolvedYouTubeStream, Error>?
    private var id: Int64?
    private var cancelled = false
    private var completed = false
    func install(_ value: CheckedContinuation<ResolvedYouTubeStream, Error>) {
        lock.lock()
        if cancelled { lock.unlock(); value.resume(throwing: CancellationError()); return }
        continuation = value; lock.unlock()
    }
    func setID(_ value: Int64) {
        lock.lock(); id = value; let cancel = cancelled; lock.unlock()
        if cancel { PlayerBridge.shared.cancelResolve(requestId: value) }
    }
    func finish(_ result: Result<ResolvedYouTubeStream, Error>) {
        lock.lock()
        guard !completed, !cancelled, let value = continuation else { lock.unlock(); return }
        completed = true; continuation = nil; lock.unlock()
        value.resume(with: result)
    }
    func cancel() {
        lock.lock(); cancelled = true
        let value = continuation; continuation = nil
        let requestID = id; lock.unlock()
        value?.resume(throwing: CancellationError())
        if let requestID { PlayerBridge.shared.cancelResolve(requestId: requestID) }
    }
}
