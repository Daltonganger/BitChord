import Foundation

/// Bounds the materialized portion of a canonical detail-page list.
enum PlaybackQueueWindow {
    static let windowAhead = 25
    static let lowWater = 12

    /// Keep one prior row for Previous, the selected row, and no more than the
    /// configured number of upcoming rows.
    static func initialRange(count: Int, selectedIndex: Int) -> Range<Int> {
        guard count > 0 else { return 0..<0 }
        let selected = min(max(selectedIndex, 0), count - 1)
        let start = max(0, selected - 1)
        return start..<min(count, selected + windowAhead + 1)
    }

    static func appendCount(upcomingCount: Int, canonicalRemaining: Int) -> Int {
        guard upcomingCount <= lowWater, canonicalRemaining > 0 else { return 0 }
        return min(max(0, windowAhead - upcomingCount), canonicalRemaining)
    }

    static func shouldFetchContinuation(upcomingCount: Int, canonicalRemaining: Int, hasContinuation: Bool) -> Bool {
        upcomingCount <= lowWater && canonicalRemaining == 0 && hasContinuation
    }
}
