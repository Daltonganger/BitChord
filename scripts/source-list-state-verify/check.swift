import Foundation

@main struct VerifySourceListState {
    @MainActor static func main() throws {
        let state = SourceListState()
        func source(_ id: String, enabled: Bool = true, url: String = "https://example.invalid") -> ConfigDocument {
            ConfigDocument(id: id, kind: "ADDON", label: id, displayName: id, baseUrl: url,
                           enabled: enabled, isComplete: true, needsServer: true, canServeLossless: true, labels: [])
        }
        func document(_ configs: [ConfigDocument]) throws -> String {
            struct Listing: Encodable { let sources: [ConfigDocument] }
            return String(decoding: try JSONEncoder().encode(Listing(sources: configs)), as: UTF8.self)
        }
        func load(_ configs: [ConfigDocument]) throws {
            let revision = state.beginLoad()
            let payload = try document(configs)
            precondition(state.finishLoad(payload, revision: revision))
        }
        let a = source("a"), b = source("b"), c = source("c")
        try load([a, b, c])
        for bad in [nil, "broken", "{}", try document([a, a])] {
            let revision = state.beginLoad()
            precondition(!state.finishLoad(bad, revision: revision))
            precondition(state.configs == [a,b,c] && state.loadError != nil && !state.loading)
        }
        let staleLoad = state.beginLoad(), currentLoad = state.beginLoad()
        let empty = try document([]), full = try document([a,b,c])
        precondition(!state.finishLoad(empty, revision: staleLoad) && state.loading)
        precondition(state.finishLoad(full, revision: currentLoad))
        let probe = state.beginProbe(a)!
        precondition(state.beginProbe(a) == nil)
        state.finishProbe(probe, result: SourceHealthSnapshot(name: "ok", detail: "Ready"))
        precondition(state.health["a"] != nil && state.probing.isEmpty)
        let staleProbe = state.beginProbe(a)!
        let edited = source("a", url: "https://changed.invalid")
        try load([edited,b,c])
        state.finishProbe(staleProbe, result: SourceHealthSnapshot(name: "ok", detail: "Old"))
        precondition(state.health["a"] == nil && !state.probing.contains("a"))
        let newProbe = state.beginProbe(edited)!
        state.finishProbe(staleProbe, result: nil)
        precondition(state.probing.contains("a"), "An obsolete request cannot clear a newer request")
        state.finishProbe(newProbe, result: nil)
        precondition(state.beginChange("b") && !state.beginChange("c"))
        precondition(state.configs[1].enabled, "Changes must wait for acknowledgement")
        state.acceptEnabled("b", false)
        state.finishChange()
        precondition(state.beginProbe(state.configs[1]) == nil)
        state.acceptOrder(["c", "a", "b"])
        precondition(state.configs.map(\.id) == ["c", "a", "b"])
        let removedProbe = state.beginProbe(c)!
        state.acceptRemoved("c")
        state.finishProbe(removedProbe, result: SourceHealthSnapshot(name: "ok", detail: "Old"))
        precondition(state.health["c"] == nil && state.configs.map(\.id) == ["a", "b"])
        let cancel = state.beginLoad()
        state.cancelLoad(cancel)
        precondition(!state.loading)
        try load([])
        precondition(state.configs.isEmpty && state.hasLoaded && state.loadError == nil)
        print("PASS retained lists, stale loads/probes, edited/deleted source health, mutation acknowledgement, disabled probes, ordering and valid empty lists")
    }
}
