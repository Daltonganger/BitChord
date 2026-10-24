import Foundation

/// Handoff of playback: what this device is playing is offered to the listener's
/// other devices, and picking it up there carries on from the same place.
///
/// The activity is made current when the answer can change — a new track, a
/// play/pause, the app leaving the foreground — but the *position* is written by
/// the system's own save hook (`userActivityWillSave`), not at those moments. A
/// Handoff activity is read when the other device picks it up, and on the Mac the
/// scene never leaves `.active` while music plays, so a snapshot refreshed only at
/// track changes would offer the position the track had when it started. The
/// system asks for the data immediately before it sends the activity to another
/// device, which is the last moment a live playhead can be written and the only
/// one that matters.
@MainActor
final class PlaybackHandoff: NSObject, NSUserActivityDelegate {
    static let shared = PlaybackHandoff()

    /// The player the current offer describes. Weak: the offer belongs to the
    /// player and not the other way round, and a handoff must never be the reason
    /// a controller stays alive.
    private weak var controller: PlaybackController?
    private var activity: NSUserActivity?

    private override init() {
        super.init()
    }

    /// Publish the current track, or withdraw the offer when nothing can be
    /// handed over.
    func refresh(controller: PlaybackController) {
        self.controller = controller
        guard let payload = payload(from: controller) else {
            withdraw()
            return
        }
        // One activity for the lifetime of the offer: `becomeCurrent` on a fresh
        // object for every track would leave the previous ones current for a
        // moment, and the other device can see whichever was current when it
        // asked.
        let activity = self.activity ?? NSUserActivity(activityType: PlaybackHandoffPayload.activityType)
        if activity.delegate !== self {
            activity.delegate = self
        }
        activity.title = payload.title
        activity.isEligibleForHandoff = true
        activity.userInfo = payload.userInfo
        // This asks the system to call `userActivityWillSave` before it sends the
        // activity for continuation. Only a playing track has a position that
        // moves; a paused one keeps the payload written just above.
        activity.needsSave = payload.isPlaying
        activity.becomeCurrent()
        self.activity = activity
    }

    /// Stop offering this device's playback — nothing is playing any more, so
    /// there is nothing for the other device to continue.
    func withdraw() {
        activity?.resignCurrent()
        activity = nil
    }

    /// The system's save hook: called on the main thread just before the activity
    /// is sent for continuation on another device.
    ///
    /// `nonisolated` with `assumeIsolated` because `NSUserActivityDelegate` is not
    /// annotated with an actor and this class is: the callback is documented to
    /// arrive on the main thread, so the assumption is stated here once rather
    /// than the payload being refreshed on a timer nobody asked for.
    nonisolated func userActivityWillSave(_ userActivity: NSUserActivity) {
        MainActor.assumeIsolated {
            guard userActivity === activity,
                  let controller,
                  let payload = payload(from: controller) else { return }
            userActivity.userInfo = payload.userInfo
            userActivity.title = payload.title
        }
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

    /// What the offer says about the player right now, or nil when there is no
    /// track that could be continued on another device.
    private func payload(from controller: PlaybackController) -> PlaybackHandoffPayload? {
        guard let current = controller.current,
              let videoId = current.videoId,
              !videoId.isEmpty else { return nil }
        return PlaybackHandoffPayload(
            videoId: videoId,
            title: current.title,
            artist: current.artist,
            positionMs: Int((controller.position * 1000).rounded()),
            isPlaying: controller.isPlaying
        )
    }
}
