import Foundation
import BitChordShared

/// Addon catalogue search for the search screen's Addons source.
///
/// One query is answered by every enabled addon in parallel (upstream's
/// `runAddonSearch`): the bridge waits on each independently, drops a slow one
/// after 12 s, and interleaves the rows round-robin so no single catalogue
/// buries the others. Rows carry `sourceQuality` for the quality badge.
final class SourceSearch: Sendable {
    static let shared = SourceSearch()

    func addons(_ query: String) async throws -> [SearchHitDTO] {
        try await withCheckedThrowingContinuation { continuation in
            SourceBridge.shared.searchAddons(
                query: query,
                callback: AddonRowsAdapter { json, message in
                    if let json {
                        do {
                            let hits = try JSONDecoder()
                                .decode([SearchHitDTO].self, from: Data(json.utf8))
                            continuation.resume(returning: hits)
                        } catch {
                            continuation.resume(throwing: error)
                        }
                    } else {
                        continuation.resume(throwing: AddonSearchError(message: message ?? "addon search failed"))
                    }
                }
            )
        }
    }

    /// The stream behind one addon search row, by its `src:` key — the path a
    /// row takes when it is tapped. Nil means the sources declined, not that the
    /// request failed; the caller falls back to skipping the row.
    func stream(key: String, title: String, artist: String, durationSec: Int?, diagnostic: @escaping @Sendable (String) -> Void = { _ in }) async -> SourceSubstitution.Stream? {
        await withCheckedContinuation { continuation in
            SourceResolverBridge.shared.resolveAddonTrack(
                key: key,
                title: title,
                artist: artist,
                durationSec: durationSec.map { KotlinInt(value: Int32($0)) },
                callback: AddonStreamAdapter { json, message in
                    if let message { diagnostic(message) }
                    continuation.resume(returning: json.flatMap(SourceSubstitution.decode))
                }
            )
        }
    }

    /// A bridge failure. `LocalizedError` so `localizedDescription` carries the
    /// bridge's message rather than the generic "couldn't be completed" text.
    struct AddonSearchError: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
}

private final class AddonRowsAdapter: SourceBridgeRowCallback {
    private let onResult: (String?, String?) -> Void
    init(onResult: @escaping (String?, String?) -> Void) { self.onResult = onResult }
    func onResult(json: String?, message: String?) { onResult(json, message) }
}

private final class AddonStreamAdapter: SourceResolverBridgeStreamCallback {
    private let onResult: (String?, String?) -> Void
    init(onResult: @escaping (String?, String?) -> Void) { self.onResult = onResult }
    func onResult(json: String?, message: String?) { onResult(json, message) }
}
