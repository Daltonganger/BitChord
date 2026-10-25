import Foundation
import BitChordShared
import os
#if os(iOS)
import UIKit
#if DEBUG
import AVFoundation
#endif
#else
import AppKit
#endif

/// UniFFI callback implementation — hops to the main queue, since views read
/// this state directly.
final class EngineCallbacks: EngineCallback, @unchecked Sendable {
    private weak var controller: PlaybackController?

    init(controller: PlaybackController) {
        self.controller = controller
    }

    func onStateChanged(state: PlaybackState) {
        Task { @MainActor in controller?.handleState(state) }
    }

    func onTrackEnded(reason: TrackEndReason, source: String) {
        Task { @MainActor in guard controller?.dolbyRenderer == nil else { return }; controller?.handleTrackEnded(reason, source: source) }
    }

    func onError(message: String) {
        PlaybackDebugLog.shared.record("engine error: \(message)")
        Task { @MainActor in guard controller?.dolbyRenderer == nil else { return }; controller?.handleError(message) }
    }

    func onHandoff(info: TrackInfoRec) {
        Task { @MainActor in guard controller?.dolbyRenderer == nil else { return }; controller?.handleHandoff(info) }
    }

    func onDurationChanged(seconds: Double) {
        Task { @MainActor in guard controller?.dolbyRenderer == nil else { return }; controller?.handleDuration(seconds) }
    }
}
