import Foundation

private actor SwapResultProbe {
    var current = true
    var result: Bool?
    var polls = 0
    func isCurrent() -> Bool { current }
    func outcome() -> Bool? { polls += 1; return result }
    func count() -> Int { polls }
    func finish(_ result: Bool) { self.result = result }
    func supersede() { current = false }
}

@main struct VerifySourcePriority {
    static func main() async {
        precondition(PlaybackPreparationPolicy.lookupBudget(remainingSeconds: 180, transitionSeconds: 8) == 12.5)
        precondition(PlaybackPreparationPolicy.lookupBudget(remainingSeconds: 15, transitionSeconds: 8) == 5)
        precondition(PlaybackPreparationPolicy.lookupBudget(remainingSeconds: 10, transitionSeconds: 30) == 0.8)
        precondition(PlaybackPreparationPolicy.lookupBudget(remainingSeconds: 0, transitionSeconds: 0) == 0.8)
        typealias Race = PlaybackSourceRace<String, String>
        let preferred = Race(preferPrimary: true)
        await preferred.finishFallback("youtube")
        await preferred.finishLookup("flac")
        guard case .preferred("flac") = await preferred.wait(timeoutNanoseconds: 1_000_000) else {
            fatalError("A ready YouTube fallback must not beat the requested lossless source")
        }

        let fastest = Race(preferPrimary: false)
        await fastest.finishFallback("youtube")
        await fastest.finishLookup("flac")
        guard case .fallback("youtube") = await fastest.wait(timeoutNanoseconds: 1_000_000) else {
            fatalError("Fast-start mode must retain the first usable answer")
        }

        let miss = Race(preferPrimary: true)
        await miss.finishFallback("youtube")
        await miss.finishLookup(nil)
        guard case .fallback("youtube") = await miss.wait(timeoutNanoseconds: 10_000_000_000) else {
            fatalError("A source miss must release the ready fallback immediately")
        }

        let late = Race(preferPrimary: true)
        await late.finishFallback("youtube")
        guard case .fallback("youtube") = await late.wait(timeoutNanoseconds: 1_000_000) else {
            fatalError("An unresponsive preferred source must not hold playback forever")
        }
        await late.finishLookup("too-late")
        guard case .fallback("youtube") = await late.wait(timeoutNanoseconds: 1_000_000) else {
            fatalError("A late source must not overwrite the selected result")
        }

        let absent = Race(preferPrimary: true)
        await absent.finishFallback(nil)
        await absent.finishLookup(nil)
        guard case .none = await absent.wait(timeoutNanoseconds: 1_000_000) else {
            fatalError("Two failed lookups must finish without waiting for the deadline")
        }

        let timeout = Race(preferPrimary: true)
        guard case .timedOut = await timeout.wait(timeoutNanoseconds: 1_000_000) else {
            fatalError("No ready source must report a deadline, rather than a fabricated success")
        }

        let cancelled = Race(preferPrimary: true)
        let pending = Task { await cancelled.wait(timeoutNanoseconds: 60_000_000_000) }
        pending.cancel()
        guard case .cancelled = await pending.value else {
            fatalError("Leaving a track must cancel its pending source selection")
        }

        let delayed = Race(preferPrimary: true)
        let waiting = Task { await delayed.wait(timeoutNanoseconds: 10_000_000_000) }
        await delayed.finishFallback("youtube")
        await Task.yield()
        await delayed.finishLookup("flac")
        guard case .preferred("flac") = await waiting.value else {
            fatalError("Lossless arriving after YouTube must still win within the selection window")
        }
        for result in [true, false] {
            let probe = SwapResultProbe()
            let submitted = Task {
                await PlaybackSwapConfirmation.wait(
                    isCurrent: { await probe.isCurrent() },
                    outcome: { await probe.outcome() }, pollNanoseconds: 1_000_000
                )
            }
            while await probe.count() == 0 { await Task.yield() }
            submitted.cancel() // Models a route change after native submission.
            await probe.finish(result)
            guard await submitted.value == result else {
                fatalError("Cancelling the lookup must still reconcile the submitted native result")
            }
        }
        let superseded = SwapResultProbe()
        let obsolete = Task {
            await PlaybackSwapConfirmation.wait(
                isCurrent: { await superseded.isCurrent() },
                outcome: { await superseded.outcome() }, pollNanoseconds: 1_000_000
            )
        }
        while await superseded.count() == 0 { await Task.yield() }
        await superseded.supersede()
        guard await obsolete.value == nil else { fatalError("An obsolete track must not update source metadata") }
        print("PASS source priority (8), submitted swap reconciliation (3), next-track timing (4 scenarios)")
    }
}
