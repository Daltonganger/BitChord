import Foundation
import Observation

/// One configured source, as the shared registry describes it.
struct ConfigDocument: Codable, Identifiable, Hashable {
    let id: String
    let kind: String
    let label: String
    let displayName: String
    let baseUrl: String
    var enabled: Bool
    let isComplete: Bool
    let needsServer: Bool
    let canServeLossless: Bool
    let labels: [String]
    var refreshEnabled: Bool? = nil
    var refreshUrl: String? = nil
    var refreshUrlUnavailable: Bool? = nil

    /// A blank addon, for the "Add" row.
    static func newAddon() -> ConfigDocument {
        ConfigDocument(
            id: "", kind: "ADDON", label: "", displayName: "Addon",
            baseUrl: "", enabled: true, isComplete: false,
            needsServer: true, canServeLossless: true, labels: []
        )
    }
}

struct SourceHealthSnapshot {
    var name: String
    var detail: String
    var checkedAt: Date = Date()
}

/// View lifetime is shorter than registry lifetime. Keep the last valid list in
/// memory, including secrets, without writing an extra plaintext URL cache.
@Observable @MainActor final class SourceListState {
    static let shared = SourceListState()
    private(set) var configs: [ConfigDocument] = []
    private(set) var health: [String: SourceHealthSnapshot] = [:]
    private(set) var probing = Set<String>()
    private(set) var hasLoaded = false
    private(set) var loading = false
    private(set) var loadError: String?
    private(set) var changing: String?
    private var revision: UInt64 = 0
    private var probeRevision: UInt64 = 0
    private var probes: [String: Probe] = [:]

    struct Probe {
        let revision: UInt64
        let config: ConfigDocument
    }

    func beginLoad() -> UInt64 {
        revision &+= 1
        loading = true
        loadError = nil
        return revision
    }

    func cancelLoad(_ revision: UInt64) {
        if revision == self.revision { loading = false }
    }

    @discardableResult
    func finishLoad(_ document: String?, revision: UInt64) -> Bool {
        guard revision == self.revision else { return false }
        loading = false
        guard let document, let bytes = document.data(using: .utf8),
              let listing = try? JSONDecoder().decode(Listing.self, from: bytes),
              Set(listing.sources.map(\.id)).count == listing.sources.count,
              listing.sources.allSatisfy({ !$0.id.isEmpty }) else {
            loadError = configs.isEmpty ? "Sources could not be loaded. Try again." : "Sources could not be refreshed. Showing the last loaded list."
            return false
        }
        let previous = Dictionary(uniqueKeysWithValues: configs.map { ($0.id, $0) })
        configs = listing.sources
        hasLoaded = true
        for (id, probe) in Array(probes) where !configs.contains(probe.config) {
            probes[id] = nil
            probing.remove(id)
        }
        health = health.filter { id, _ in configs.contains { $0.id == id && previous[id] == $0 } }
        return true
    }

    func beginProbe(_ config: ConfigDocument) -> Probe? {
        guard config.enabled, config.isComplete, config.needsServer,
              configs.contains(config), !probing.contains(config.id) else { return nil }
        probeRevision &+= 1
        let probe = Probe(revision: probeRevision, config: config)
        probes[config.id] = probe
        probing.insert(config.id)
        return probe
    }

    func finishProbe(_ probe: Probe, result: SourceHealthSnapshot?) {
        let id = probe.config.id
        guard probes[id]?.revision == probe.revision else { return }
        probes[id] = nil
        probing.remove(id)
        guard configs.contains(probe.config), let result else { return }
        health[id] = result
    }

    func beginChange(_ id: String) -> Bool {
        guard changing == nil else { return false }
        changing = id
        return true
    }

    func finishChange() { changing = nil }

    func acceptEnabled(_ id: String, _ enabled: Bool) {
        guard let index = configs.firstIndex(where: { $0.id == id }) else { return }
        configs[index].enabled = enabled
        probes[id] = nil
        probing.remove(id)
        health[id] = nil
    }

    func acceptRemoved(_ id: String) {
        configs.removeAll { $0.id == id }
        health[id] = nil
        probes[id] = nil
        probing.remove(id)
    }

    func acceptOrder(_ ids: [String]) {
        let positions = Dictionary(uniqueKeysWithValues: ids.enumerated().map { ($1, $0) })
        let addons = configs.filter { $0.kind == "ADDON" }.sorted { positions[$0.id, default: Int.max] < positions[$1.id, default: Int.max] }
        configs = addons + configs.filter { $0.kind != "ADDON" }
    }

    private struct Listing: Decodable { let sources: [ConfigDocument] }
}
