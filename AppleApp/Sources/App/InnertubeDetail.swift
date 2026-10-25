import Foundation
import BitChordShared

/// The offline stand-in for a collection saved to this device: the download
/// store's entries under the title the download recorded, so a page opened
/// with no network shows the songs the listener already has (upstream's
/// `downloadedCopyOf`).
extension DetailPageModel {
    init(
        browseId: String,
        title: String,
        subtitle: String,
        thumbnailUrl: String?,
        songs: [SongPayload],
        sections: [FeedShelf],
        description: String?,
        subscriberCountText: String?,
        monthlyListenerCount: String?,
        subscription: ArtistSubscriptionPayload?,
        continuation: String?,
        suggestedSongs: [SongPayload],
        libraryPlaylistId: String?,
        librarySaved: Bool?,
        playlistOwned: Bool?,
        type: String?,
        url: String?
    ) {
        self.browseId = browseId
        self.title = title
        self.subtitle = subtitle
        self.thumbnailUrl = thumbnailUrl
        self.songs = songs
        self.sections = sections
        self.description = description
        self.subscriberCountText = subscriberCountText
        self.monthlyListenerCount = monthlyListenerCount
        self.subscription = subscription
        self.continuation = continuation
        self.suggestedSongs = suggestedSongs
        self.libraryPlaylistId = libraryPlaylistId
        self.librarySaved = librarySaved
        self.playlistOwned = playlistOwned
        self.type = type
        self.url = url
    }

    @MainActor
    static func downloadedCopy(browseId: String, from store: DownloadStore) -> DetailPageModel? {
        let entries = store.savedEntries(for: browseId)
        guard !entries.isEmpty else { return nil }
        let songs = entries.map { entry in
            DetailPageModel.SongPayload(
                videoId: entry.videoId ?? entry.id,
                title: entry.title,
                artist: entry.artist,
                thumbnailUrl: entry.thumbnailUrl,
                durationText: entry.durationText,
                albumName: entry.albumName,
                isVideo: entry.isVideo,
                artistId: entry.artistId,
                albumId: entry.albumId,
                setVideoId: entry.setVideoId
            )
        }
        return DetailPageModel(
            browseId: browseId,
            title: store.savedTitle(for: browseId) ?? browseId,
            subtitle: "Downloaded",
            thumbnailUrl: entries.compactMap(\.thumbnailUrl).first,
            songs: songs,
            sections: [],
            description: nil,
            subscriberCountText: nil,
            monthlyListenerCount: nil,
            subscription: nil,
            continuation: nil,
            suggestedSongs: [],
            libraryPlaylistId: nil,
            librarySaved: nil,
            playlistOwned: nil,
            type: "PLAYLIST",
            url: nil
        )
    }
}

struct DetailPageModel: Codable {
    let browseId: String
    let title: String
    let subtitle: String
    let thumbnailUrl: String?
    var songs: [SongPayload]
    let sections: [FeedShelf]
    let description: String?
    let subscriberCountText: String?
    let monthlyListenerCount: String?
    let subscription: ArtistSubscriptionPayload?
    var continuation: String?
    var suggestedSongs: [SongPayload]
    let libraryPlaylistId: String?
    let librarySaved: Bool?
    let playlistOwned: Bool?
    let type: String?
    let url: String?

    enum CodingKeys: String, CodingKey {
        case browseId, title, subtitle, thumbnailUrl, songs, sections, description, subscriberCountText, monthlyListenerCount, subscription, continuation, type, suggestedSongs, libraryPlaylistId, librarySaved, playlistOwned, url
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        browseId = try c.decodeIfPresent(String.self, forKey: .browseId) ?? ""
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        subtitle = try c.decodeIfPresent(String.self, forKey: .subtitle) ?? ""
        thumbnailUrl = try c.decodeIfPresent(String.self, forKey: .thumbnailUrl)
        songs = try c.decodeIfPresent([SongPayload].self, forKey: .songs) ?? []
        sections = try c.decodeIfPresent([FeedShelf].self, forKey: .sections) ?? []
        description = try c.decodeIfPresent(String.self, forKey: .description)
        subscriberCountText = try c.decodeIfPresent(String.self, forKey: .subscriberCountText)
        monthlyListenerCount = try c.decodeIfPresent(String.self, forKey: .monthlyListenerCount)
        subscription = try c.decodeIfPresent(ArtistSubscriptionPayload.self, forKey: .subscription)
        continuation = try c.decodeIfPresent(String.self, forKey: .continuation)
        type = try c.decodeIfPresent(String.self, forKey: .type)
        suggestedSongs = try c.decodeIfPresent([SongPayload].self, forKey: .suggestedSongs) ?? []
        libraryPlaylistId = try c.decodeIfPresent(String.self, forKey: .libraryPlaylistId)
        librarySaved = try c.decodeIfPresent(Bool.self, forKey: .librarySaved)
        playlistOwned = try c.decodeIfPresent(Bool.self, forKey: .playlistOwned)
        url = try c.decodeIfPresent(String.self, forKey: .url)
    }

    struct SongPayload: Codable {
        let videoId: String
        let title: String
        let artist: String
        let thumbnailUrl: String?
        let durationText: String?
        let albumName: String?
        let isVideo: Bool
        let artistId: String?
        let albumId: String?
        let setVideoId: String?

        enum CodingKeys: String, CodingKey {
            case videoId, title, artist, thumbnailUrl, durationText, albumName, isVideo, artistId, albumId, setVideoId
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            videoId = try c.decode(String.self, forKey: .videoId)
            title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
            artist = try c.decodeIfPresent(String.self, forKey: .artist) ?? "Unknown artist"
            thumbnailUrl = try c.decodeIfPresent(String.self, forKey: .thumbnailUrl)
            durationText = try c.decodeIfPresent(String.self, forKey: .durationText)
            albumName = try c.decodeIfPresent(String.self, forKey: .albumName)
            isVideo = try c.decodeIfPresent(Bool.self, forKey: .isVideo) ?? false
            artistId = try c.decodeIfPresent(String.self, forKey: .artistId)
            albumId = try c.decodeIfPresent(String.self, forKey: .albumId)
            setVideoId = try c.decodeIfPresent(String.self, forKey: .setVideoId)
        }

        init(
            videoId: String,
            title: String,
            artist: String,
            thumbnailUrl: String?,
            durationText: String?,
            albumName: String?,
            isVideo: Bool,
            artistId: String?,
            albumId: String?,
            setVideoId: String?
        ) {
            self.videoId = videoId
            self.title = title
            self.artist = artist
            self.thumbnailUrl = thumbnailUrl
            self.durationText = durationText
            self.albumName = albumName
            self.isVideo = isVideo
            self.artistId = artistId
            self.albumId = albumId
            self.setVideoId = setVideoId
        }

        func asEntry(fallbackArt: String? = nil) -> QueueEntry {
            QueueEntry.youtube(
                videoId: videoId, title: title, artist: artist,
                thumbnailUrl: thumbnailUrl ?? fallbackArt, durationText: durationText,
                albumName: albumName, artistId: artistId, albumId: albumId, setVideoId: setVideoId
            )
        }
    }
}

struct ArtistSubscriptionPayload: Codable {
    let channelId: String
    let subscribed: Bool
}

final class InnertubeDetail: Sendable {
    static let shared = InnertubeDetail()

    @MainActor
    func browse(browseId: String, force: Bool = false) async throws -> DetailPageModel {
        try await PageRequests.load("detail:browse:\(browseId)", force: force) { try await self.raw_browse(browseId: browseId) }
    }

    private func raw_browse(browseId: String) async throws -> DetailPageModel {
        try await withCheckedThrowingContinuation { cont in
            DetailBridge.shared.browse(browseId: browseId, callback: DetailCallback { json, msg in
                if let json {
                    do {
                        // Log JSON for diagnostics then decode leniently
                        // print("[Detail] json \(json.prefix(500))")
                        let page = try JSONDecoder().decode(DetailPageModel.self, from: Data(json.utf8))
                        cont.resume(returning: page)
                    } catch {
                        print("[Detail] response could not be decoded")
                        cont.resume(throwing: error)
                    }
                } else {
                    cont.resume(throwing: DetailError(msg ?? "browse failed"))
                }
            })
        }
    }

    @MainActor
    func browseArtist(browseId: String, force: Bool = false) async throws -> DetailPageModel {
        try await PageRequests.load("detail:browseArtist:\(browseId)", force: force) { try await self.raw_browseArtist(browseId: browseId) }
    }

    private func raw_browseArtist(browseId: String) async throws -> DetailPageModel {
        try await withCheckedThrowingContinuation { cont in
            DetailBridge.shared.browseArtist(browseId: browseId, callback: DetailCallback { json, msg in
                if let json {
                    do {
                        let page = try JSONDecoder().decode(DetailPageModel.self, from: Data(json.utf8))
                        cont.resume(returning: page)
                    } catch {
                        print("[Detail] decode artist failed: \(error)")
                        cont.resume(throwing: error)
                    }
                } else {
                    cont.resume(throwing: DetailError(msg ?? "browse failed"))
                }
            })
        }
    }

    func more(token: String) async throws -> DetailPageModel {
        try await withCheckedThrowingContinuation { cont in
            DetailBridge.shared.more(token: token, callback: DetailCallback { json, msg in
                if let json {
                    do {
                        let page = try JSONDecoder().decode(DetailPageModel.self, from: Data(json.utf8))
                        cont.resume(returning: page)
                    } catch {
                        cont.resume(throwing: error)
                    }
                } else {
                    cont.resume(throwing: DetailError(msg ?? "continuation failed"))
                }
            })
        }
    }

    struct DetailError: Error, LocalizedError {
        let message: String
        init(_ m: String) { message = m }
        var errorDescription: String? { message }
    }
}

private final class DetailCallback: DetailBridgeDetailCallback {
    let onResult: (String?, String?) -> Void
    init(_ cb: @escaping (String?, String?) -> Void) { onResult = cb }
    func onResult(json: String?, message: String?) { onResult(json, message) }
}
