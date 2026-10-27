import Foundation

/// A lossless preference gives configured sources a bounded head start over a ready fallback.
actor PlaybackSourceRace<Preferred: Sendable, Fallback: Sendable> {
    enum Winner: Sendable {
        case preferred(Preferred)
        case fallback(Fallback)
        case timedOut
        case none
        case cancelled
    }

    private let preferPrimary: Bool
    private var fallback: Fallback?
    private var lookupFinished = false
    private var fallbackFinished = false
    private var result: Winner?
    private var continuation: CheckedContinuation<Winner, Never>?
    private var timeoutTask: Task<Void, Never>?

    init(preferPrimary: Bool) { self.preferPrimary = preferPrimary }

    func wait(timeoutNanoseconds: UInt64) async -> Winner {
        await withTaskCancellationHandler {
            if Task.isCancelled { return .cancelled }
            return await withCheckedContinuation { continuation in
                if let result {
                    continuation.resume(returning: result)
                } else {
                    self.continuation = continuation
                    timeoutTask = Task {
                        do { try await Task.sleep(nanoseconds: timeoutNanoseconds) }
                        catch { return }
                        self.timeOut()
                    }
                }
            }
        } onCancel: {
            Task { await self.finish(.cancelled) }
        }
    }

    func finishLookup(_ candidate: Preferred?) {
        guard result == nil else { return }
        lookupFinished = true
        if let candidate { finish(.preferred(candidate)) }
        else if let fallback { finish(.fallback(fallback)) }
        else if fallbackFinished { finish(.none) }
    }

    func finishFallback(_ candidate: Fallback?) {
        guard result == nil else { return }
        fallbackFinished = true
        fallback = candidate
        if let candidate {
            if !preferPrimary || lookupFinished { finish(.fallback(candidate)) }
        } else if lookupFinished {
            finish(.none)
        }
    }

    private func timeOut() {
        if let fallback { finish(.fallback(fallback)) }
        else { finish(.timedOut) }
    }

    private func finish(_ winner: Winner) {
        guard result == nil else { return }
        result = winner
        timeoutTask?.cancel()
        timeoutTask = nil
        continuation?.resume(returning: winner)
        continuation = nil
    }
}
