import Foundation

/// Leave time to arm the transition even when quality lookups are still running.
enum PlaybackPreparationPolicy {
    static func lookupBudget(remainingSeconds: TimeInterval, transitionSeconds: TimeInterval) -> TimeInterval {
        min(12.5, max(0.8, remainingSeconds - max(0, transitionSeconds) - 2))
    }
}
