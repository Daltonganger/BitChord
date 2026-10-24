import Foundation

/// Queue transformations share one boundary: played rows stay put, manual rows
/// keep their slots, and recommendations remain a separate tail.
enum PlaybackQueuePolicy {
    enum HeroState { case inactive, playing, paused }

    static let autoplayRefillThreshold = 3
    /// How many recommendations the tail is topped up to.
    static let autoplayTarget = 8
    /// Upcoming rows nothing automatic may move: the listener can see them.
    static let committedRows = 2

    static func shouldRefillAutoplay(upcomingCount: Int) -> Bool {
        upcomingCount < autoplayRefillThreshold
    }

    static func heroState(
        origin: String?, listID: String, hasQueue: Bool, stopped: Bool, playingOrBuffering: Bool
    ) -> HeroState {
        guard origin == listID, hasQueue, !stopped else { return .inactive }
        return playingOrBuffering ? .playing : .paused
    }

    static func listIndices(_ queue: [QueueEntry], after index: Int) -> [Int] {
        queue.indices.filter { $0 > index && queue[$0].contextOrder != nil && !queue[$0].fromAutoplay }
    }

    static func autoplayIndices(_ queue: [QueueEntry], after index: Int) -> [Int] {
        queue.indices.filter { $0 > index && queue[$0].fromAutoplay }
    }

    /// A Shuffle hero picks a seed without losing the earlier list tracks. Row
    /// selections keep their selected index and the existing played prefix.
    static func startingWith(_ queue: [QueueEntry], seed: Int) -> [QueueEntry] {
        guard queue.indices.contains(seed) else { return queue }
        return [queue[seed]] + queue.enumerated().filter { $0.offset != seed }.map(\.element)
    }

    static func orderList(
        _ queue: [QueueEntry], after index: Int, automix: Bool, shuffle: Bool, scores: [Double]
    ) -> [QueueEntry] {
        let positions = listIndices(queue, after: index)
        let ordered: [Int]
        if automix {
            ordered = ranked(positions, scores: scores)
        } else if shuffle {
            ordered = positions.shuffled()
        } else {
            ordered = positions.sorted { (queue[$0].contextOrder ?? 0) < (queue[$1].contextOrder ?? 0) }
        }
        return replacing(queue, at: positions, with: ordered.map { queue[$0] })
    }

    static func ranked(_ positions: [Int], scores: [Double]) -> [Int] {
        positions.sorted {
            let left = scores.indices.contains($0) ? scores[$0] : 0
            let right = scores.indices.contains($1) ? scores[$1] : 0
            return left == right ? $0 < $1 : left > right
        }
    }

    static func replacing(_ queue: [QueueEntry], at positions: [Int], with entries: [QueueEntry]) -> [QueueEntry] {
        var result = queue
        for (position, entry) in zip(positions, entries) { result[position] = entry }
        return result
    }

    /// Room left in the recommendation tail before it reaches [autoplayTarget].
    static func fillRoom(_ queue: [QueueEntry], after index: Int) -> Int {
        autoplayTarget - autoplayIndices(queue, after: index).count
    }

    /// A fill's only effect on the queue: new entries appended. Nothing already
    /// queued is removed, moved or replaced, and nothing is queued twice.
    static func appendingFill(_ queue: [QueueEntry], extras: [QueueEntry]) -> [QueueEntry] {
        var known = Set(queue.map(\.id))
        return queue + extras.filter { known.insert($0.id).inserted }
    }

    static func score(transitionFit: Double?, affinity: Double, playedAt: Double?, now: Double) -> Double {
        let recent = (playedAt ?? 0) > now - 14 * 24 * 60 * 60 * 1000
        let novelty = playedAt == nil ? 1.0 : (recent ? 0.0 : 0.65)
        return (transitionFit ?? 0.5) * 0.60 + affinity * 0.25 + novelty * 0.15 - (recent ? 0.25 : 0)
    }
}

/// Derived sorting does not change this context. A user queue edit, selection,
/// repeat policy or account change invalidates a recommendation response.
struct AutoplayRefreshState {
    struct Context: Equatable {
        let source: String
        let index: Int
        let playbackGeneration: UInt64
        let queueEditRevision: UInt64
        let sessionGeneration: Int64
    }
    struct Request: Equatable {
        let context: Context
        let serial: UInt64
    }
    private var latest: Request?
    private var serial: UInt64 = 0

    mutating func begin(_ context: Context, force: Bool = false) -> Request? {
        guard force || latest?.context != context else { return nil }
        serial &+= 1
        let request = Request(context: context, serial: serial)
        latest = request
        return request
    }

    func accepts(_ request: Request, current: Context) -> Bool {
        latest == request && request.context == current
    }

    /// A reply that landed while the context moved — usually the first song of a
    /// session, whose load is still settling when the recommendations arrive —
    /// is asked for again, but only while it is still the newest request and at
    /// most `maxRetries` times. Without this, a session started from one song
    /// never filled: nothing else asks until the queue is already empty.
    static let maxRetries = 3
    private var retries = 0

    mutating func shouldRetry(_ request: Request) -> Bool {
        guard latest == request, retries < Self.maxRetries else { return false }
        retries += 1
        return true
    }

    /// A fill landed, so the next miss starts a fresh retry budget.
    mutating func noteFilled() {
        retries = 0
    }

    mutating func invalidate() {
        serial &+= 1
        latest = nil
        retries = 0
    }
}
