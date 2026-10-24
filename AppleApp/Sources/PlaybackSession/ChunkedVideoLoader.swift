import AVFoundation
import Foundation
import UniformTypeIdentifiers

/// Feeds the picture player a googlevideo rendition in bounded byte ranges.
///
/// AVFoundation asks the CDN for open-ended ranges (`bytes=N-`), which
/// googlevideo slows to playback speed: a 1080p picture took about 20 s to
/// become ready, and once playing it ran short of frames and held one still
/// while its clock went on. Bounded 1 MiB ranges arrive at line rate (the
/// engine's `fetch_loop` and the aligner fetch the same way). The asset is
/// opened under a private scheme so every request comes here instead.
final class ChunkedVideoLoader: NSObject, AVAssetResourceLoaderDelegate, @unchecked Sendable {
    static let scheme = "bitchord-chunked"
    private static let chunkBytes: Int64 = 1 << 20

    let queue = DispatchQueue(label: "app.bitchord.video-loader")
    private let source: URL
    private let headers: [String: String]
    private let lock = NSLock()
    private var totalLength: Int64?
    /// Touched only on `queue`, where AVFoundation calls the delegate.
    private var tasks: [ObjectIdentifier: Task<Void, Never>] = [:]

    init(source: URL, headers: [String: String]) {
        self.source = source
        self.headers = headers
    }

    deinit {
        tasks.values.forEach { $0.cancel() }
    }

    /// The source's URL under the private scheme, for `AVURLAsset`.
    var assetURL: URL? {
        var components = URLComponents(url: source, resolvingAgainstBaseURL: false)
        components?.scheme = Self.scheme
        return components?.url
    }

    func resourceLoader(
        _ resourceLoader: AVAssetResourceLoader,
        shouldWaitForLoadingOfRequestedResource loadingRequest: AVAssetResourceLoadingRequest
    ) -> Bool {
        let id = ObjectIdentifier(loadingRequest)
        let request = UncheckedRequest(value: loadingRequest)
        tasks[id] = Task { [weak self] in
            await self?.serve(request.value)
            self?.queue.async { self?.tasks[id] = nil }
        }
        return true
    }

    func resourceLoader(_ resourceLoader: AVAssetResourceLoader, didCancel loadingRequest: AVAssetResourceLoadingRequest) {
        tasks.removeValue(forKey: ObjectIdentifier(loadingRequest))?.cancel()
    }

    private func serve(_ request: AVAssetResourceLoadingRequest) async {
        do {
            let total = try await length()
            if let info = request.contentInformationRequest {
                info.contentType = UTType.mpeg4Movie.identifier
                info.contentLength = total
                info.isByteRangeAccessSupported = true
            }
            if let data = request.dataRequest {
                var offset = data.currentOffset
                let end = data.requestsAllDataToEndOfResource
                    ? total
                    : min(total, data.requestedOffset + Int64(data.requestedLength))
                while offset < end {
                    try Task.checkCancellation()
                    let bytes = try await fetch(from: offset, through: min(end, offset + Self.chunkBytes) - 1)
                    guard !bytes.isEmpty else { break }
                    data.respond(with: bytes)
                    offset += Int64(bytes.count)
                }
            }
            if !Task.isCancelled { request.finishLoading() }
        } catch is CancellationError {
            // AVFoundation cancelled it; nothing to answer.
        } catch {
            if !Task.isCancelled { request.finishLoading(with: error) }
        }
    }

    /// The rendition's size, from the first range's `Content-Range` total.
    private func length() async throws -> Int64 {
        if let known = lock.withLock({ totalLength }) { return known }
        let (_, response) = try await get(range: "bytes=0-0")
        guard let range = response.value(forHTTPHeaderField: "Content-Range"),
              let total = range.split(separator: "/").last.flatMap({ Int64($0) }) else {
            throw URLError(.badServerResponse)
        }
        lock.withLock { totalLength = total }
        return total
    }

    private func fetch(from start: Int64, through end: Int64) async throws -> Data {
        try await get(range: "bytes=\(start)-\(end)").0
    }

    private func get(range: String) async throws -> (Data, HTTPURLResponse) {
        var request = URLRequest(url: source)
        for (name, value) in headers { request.setValue(value, forHTTPHeaderField: name) }
        request.setValue(range, forHTTPHeaderField: "Range")
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 206 || http.statusCode == 200 else {
            throw URLError(.badServerResponse)
        }
        return (data, http)
    }
}

/// AVFoundation's loading requests are not marked Sendable; each is answered
/// by exactly one task.
private struct UncheckedRequest: @unchecked Sendable {
    let value: AVAssetResourceLoadingRequest
}
