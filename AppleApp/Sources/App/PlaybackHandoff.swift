import Foundation

/// Handoff of playback: what this device is playing is offered to the listener's
/// other devices, and picking it up there carries on from the same place.
///
/// The activity is refreshed when the answer can change — a new track, a
/// play/pause, and the app leaving the foreground — rather than continuously.
/// Handoff reads the activity when the other device picks it up, so a position
/// sampled at those moments is what "continue where you left off" needs; an
/// activity rewritten four times a second is work nobody ever sees.
@MainActor
final class PlaybackHandoff {
    static let shared = PlaybackHandoff()

    private var activity: NSUserActivity?

    private init() {}

    /// Publish the current track, or withdraw the offer when nothing can be
    /// handed over.
    func refresh(controller: PlaybackController) {
        guard let current = controller.current,
              let videoId = current.videoId,
              !videoId.isEmpty else {
            withdraw()
            return
        }
        let payload = PlaybackHandoffPayload(
            videoId: videoId,
            title: current.title,
            artist: current.artist,
            positionMs: Int((controller.position * 1000).rounded()),
            isPlaying: controller.isPlaying
        )
        // One activity for the lifetime of the offer: `becomeCurrent` on a fresh
        // object for every track would leave the previous ones current for a
        // moment, and the other device can see the activity that was current when
        // it asked.
        let activity = self.activity ?? NSUserActivity(activityType: PlaybackHandoffPayload.activityType)
        activity.title = current.title
        activity.isEligibleForHandoff = true
        activity.userInfo = payload.userInfo
        activity.becomeCurrent()
        self.activity = activity
    }

    /// Stop offering this device's playback — nothing is playing any more, so
    /// there is nothing for the other device to continue.
    func withdraw() {
        activity?.resignCurrent()
        activity = nil
    }

    /// Carry on from a payload that arrived from another device.
    ///
    /// The track plays as a one-song queue, because the receiving device has no
    /// queue to put it in. The playhead moves once the engine has the track:
    /// `seek` only moves a track the engine has loaded, so seeking immediately
    /// would be dropped without a word. The wait is bounded, and it stops the
    /// moment the listener replaces the track — a handoff that lands late must
    /// not pull the player back to what the other device was playing.
    func continuePlayback(payload: PlaybackHandoffPayload, controller: PlaybackController) {
        let entry = QueueEntry.youtube(
            videoId: payload.videoId,
            title: payload.title,
            artist: payload.artist
        )
        controller.play([entry], at: 0, context: "Handoff")
        if !payload.isPlaying {
            // A paused track must arrive paused. While the load is still in
            // flight this holds the pause until the first frame is ready.
            controller.togglePlayPause()
        }
        guard payload.positionMs > 0 else { return }
        Task { @MainActor in
            let deadline = Date().addingTimeInterval(15)
            while Date() < deadline {
                guard controller.current?.id == entry.id else { return }
                if controller.duration > 1 {
                    controller.seek(to: payload.positionSeconds)
                    return
                }
                try? await Task.sleep(for: .milliseconds(250))
            }
        }
    }
}
