import Foundation

struct ArtistSignature: Equatable, Sendable {
    let artistName: String
    let imageURL: URL
    let isSVG: Bool
}

enum ArtistSignatureLookupResult: Equatable, Sendable {
    case found(ArtistSignature)
    case notFound
    case failed
}

/// Finds a Commons signature file by artist name. A configured URL may point at
/// a Wikimedia-compatible API endpoint; blank uses Commons' public API.
actor ArtistSignatureRepository {
    static let shared = ArtistSignatureRepository()
    static let defaultEndpoint = "https://commons.wikimedia.org/w/api.php"
    private static let cacheLimit = 64

    private var cache: [String: ArtistSignatureLookupResult] = [:]
    private var recency: [String] = []

    func signature(for artistName: String, endpointOverride: String) async -> ArtistSignatureLookupResult {
        let name = artistName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, let url = Self.requestURL(artistName: name, endpointOverride: endpointOverride) else { return .failed }
        let key = "\(url.absoluteString)|\(name.lowercased())"
        if let cached = cache[key] {
            recency.removeAll { $0 == key }
            recency.append(key)
            return cached
        }

        var request = URLRequest(url: url)
        request.setValue("BitChord/1.0 (artist signature lookup)", forHTTPHeaderField: "User-Agent")
        guard case let (data, response)? = try? await URLSession.shared.data(for: request),
              (response as? HTTPURLResponse)?.statusCode == 200 else { return .failed }
        let result = Self.parseResponse(data, requestedArtist: name)
        guard result != .failed else { return .failed }
        cache[key] = result
        recency.append(key)
        if recency.count > Self.cacheLimit {
            cache.removeValue(forKey: recency.removeFirst())
        }
        return result
    }

    static func requestURL(artistName: String, endpointOverride: String) -> URL? {
        let raw = endpointOverride.trimmingCharacters(in: .whitespacesAndNewlines)
        guard var components = URLComponents(string: raw.isEmpty ? defaultEndpoint : raw),
              let scheme = components.scheme?.lowercased(), ["http", "https"].contains(scheme),
              components.host != nil else { return nil }
        var query = components.queryItems ?? []
        let values = [
            "action": "query",
            "generator": "search",
            "gsrsearch": "\(artistName) signature OR autograph",
            "gsrnamespace": "6",
            "gsrlimit": "10",
            "prop": "imageinfo",
            "iiprop": "url|mime",
            "iiurlwidth": "1600",
            "format": "json",
            "origin": "*",
        ]
        for (name, value) in values {
            query.removeAll { $0.name == name }
            query.append(URLQueryItem(name: name, value: value))
        }
        components.queryItems = query
        let unreserved = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")
        components.percentEncodedQuery = query.map { item in
            let encodedName = item.name.addingPercentEncoding(withAllowedCharacters: unreserved) ?? ""
            let encodedValue = (item.value ?? "").addingPercentEncoding(withAllowedCharacters: unreserved) ?? ""
            return "\(encodedName)=\(encodedValue)"
        }.joined(separator: "&")
        return components.url
    }

    static func parseResponse(_ data: Data, requestedArtist: String) -> ArtistSignatureLookupResult {
        guard let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let query = root["query"] as? [String: Any] else { return .failed }
        guard let pages = query["pages"] as? [String: [String: Any]] else { return .notFound }
        let artist = normalize(requestedArtist)
        let candidates: [(score: Int, signature: ArtistSignature)] = pages.values.compactMap { page in
            guard let title = page["title"] as? String,
                  title.lowercased().hasSuffix(".svg") || title.lowercased().hasSuffix(".png") || title.lowercased().hasSuffix(".jpg") || title.lowercased().hasSuffix(".jpeg"),
                  let imageInfo = (page["imageinfo"] as? [[String: Any]])?.first,
                  let rawURL = imageInfo["thumburl"] as? String,
                  let imageURL = URL(string: rawURL),
                  ["http", "https"].contains(imageURL.scheme?.lowercased() ?? "") else { return nil }
            let searchableTitle = normalize(title)
            guard searchableTitle.contains(artist), searchableTitle.contains("signature") || searchableTitle.contains("autograph") else { return nil }
            let mime = (imageInfo["mime"] as? String)?.lowercased() ?? ""
            let svg = mime == "image/svg+xml" || title.lowercased().hasSuffix(".svg")
            let score = (svg ? 100 : 0) + (searchableTitle.contains("signature") ? 10 : 0)
            return (score, ArtistSignature(artistName: requestedArtist, imageURL: imageURL, isSVG: svg))
        }
        guard let best = candidates.max(by: { $0.score < $1.score })?.signature else { return .notFound }
        return .found(best)
    }

    private static func normalize(_ value: String) -> String {
        value.lowercased()
            .replacingOccurrences(of: "_", with: " ")
            .replacingOccurrences(of: "-", with: " ")
            .filter { $0.isLetter || $0.isNumber || $0.isWhitespace }
            .split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }
}
