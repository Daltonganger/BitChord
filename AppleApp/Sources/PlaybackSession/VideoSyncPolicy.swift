import Foundation

/// How the muted picture player follows the audio engine's clock.
///
/// Deliberately pure: no AVFoundation, no timer, no player. The controller reads
/// the engine and the picture once every 250 ms and asks this for a decision;
/// `scripts/check-video-sync.sh` feeds it fake clock values, including the
/// boundaries, so the sync contract is pinned without a device or a stream.
enum VideoSyncPolicy {
    /// Past this the picture is not "a little behind": seek it.
    static let seekDriftSeconds: Double = 0.15
    /// Inside this the difference is not worth a rate change.
    static let ignoreDriftSeconds: Double = 0.02
    /// How far the picture's rate may be pushed either way to catch up.
    static let maxRateNudge: Float = 0.1
    /// One seek per this long, so a slow seek cannot become a seek loop.
    static let seekCooldownSeconds: Double = 0.5
    /// How much of the remaining drift one second of nudged playback takes out.
    /// Below 1 so the picture approaches the clock rather than overshooting it.
    static let nudgeGain: Double = 0.8

    enum Action: Equatable {
        case none
        /// The transport is paused but the picture is still moving.
        case pause
        /// Run the picture at this rate (the engine's rate plus the nudge).
        case play(rate: Float)
        /// Jump the picture to this engine position.
        case seek(seconds: Double)
    }

    /// - Parameters:
    ///   - engineSeconds: the audio engine's position (the clock).
    ///   - videoSeconds: the picture player's current position.
    ///   - enginePlaying: whether the transport is playing.
    ///   - engineRate: the engine's playback rate (1.0 normally, 0.5–2.0 for
    ///     pitch-preserving speed).
    ///   - videoRate: the picture player's current rate, 0 when paused.
    ///   - secondsSinceSeek: since the last seek completed, for the cooldown.
    static func decide(
        engineSeconds: Double,
        videoSeconds: Double,
        enginePlaying: Bool,
        engineRate: Double,
        videoRate: Float,
        secondsSinceSeek: Double,
        seekLead: Double = 0
    ) -> Action {
        guard engineSeconds.isFinite, videoSeconds.isFinite else { return .none }
        let drift = engineSeconds - videoSeconds
        let canSeek = secondsSinceSeek >= seekCooldownSeconds

        if !enginePlaying {
            // Stop the picture first; the next tick can move it to a scrubbed
            // position once it is actually still.
            if videoRate != 0 { return .pause }
            if abs(drift) > seekDriftSeconds && canSeek {
                return .seek(seconds: max(0, engineSeconds))
            }
            return .none
        }

        if abs(drift) > seekDriftSeconds && canSeek {
            // A seek on a streamed picture takes a few hundred milliseconds,
            // and the engine keeps moving while it does. Landing where the
            // engine *was* leaves the picture behind by exactly that much,
            // which is past the threshold again: the picture then seeks
            // forever and shows one frame per seek. Aim where the engine will
            // be when the seek lands.
            let rate = engineRate.isFinite && engineRate > 0 ? engineRate : 1
            return .seek(seconds: max(0, engineSeconds + max(0, seekLead) * rate))
        }

        let base = engineRate.isFinite && engineRate > 0 ? Float(engineRate) : 1
        let rawNudge = abs(drift) <= ignoreDriftSeconds ? 0 : Float(drift * nudgeGain)
        let nudge = min(max(rawNudge, -maxRateNudge), maxRateNudge)
        let target = min(max(base + nudge, 0.1), 2.0)
        if videoRate == 0 || abs(videoRate - target) > 0.01 {
            return .play(rate: target)
        }
        return .none
    }
}
