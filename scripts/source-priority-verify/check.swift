import Foundation

@main struct VerifySourcePriority {
    static func main() async {
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
        print("PASS source priority: preferred / fast-start / miss / deadline / late result / failures / cancellation / asynchronous arrival")
    }
}
