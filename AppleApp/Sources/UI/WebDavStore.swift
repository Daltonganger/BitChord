import SwiftUI
import Observation
import BitChordShared
import Foundation

/// The remote file library, as one observable value.
///
/// # Why a store rather than the views calling the bridge
///
/// A Kotlin `StateFlow` is not something a SwiftUI view can observe — it is not an
/// `Observable` and there is no subscription a view can make — so the shared module
/// hands values over one call at a time and this holds them. It is also where the
/// *decisions* live, and those are worth keeping in one place:
///
/// - **A listing is re-read, not cached.** The server is the only thing that knows
///   what is on it, and a cached listing is a listing that is wrong the moment
///   somebody uploads an album. `WebDavRepository` re-lists for the same reason.
/// - **An empty share and a refused share are different sentences.** A share that
///   answers with an error is not an empty share, and "no audio files" would send
///   somebody hunting through folder paths for a password problem — which is the
///   whole content of `RemoteListing.state` on the other side of the bridge.
/// - **Albums are the parent folder**, which is what groups a
///   `Music/Artist/Album/track.flac` share into releases without reading a tag. It
///   is also why a share dumped flat has no albums: there are none, and inventing
///   one out of the file name would be a lie the listener has no way to see past.
@MainActor
@Observable
final class WebDavStore {

    static let shared = WebDavStore()

    /// Where the page is, as four states rather than three.
    ///
    /// `loading` and `empty` are separate because a share that has answered and found
    /// nothing is a *result*, and drawing a spinner over it forever is the one outcome
    /// that makes a listener think the app is broken.
    enum State: Equatable {
        case notConfigured
        case loading
        case empty
        case loaded
        case failed(String)
    }

    private(set) var state: State = .notConfigured
    private(set) var albums: [RemoteAlbum] = []
    private(set) var trackCount = 0
    private(set) var uploadingEntryIDs: Set<String> = []

    /// The share as it is configured right now.
    ///
    /// Read from the shared settings rather than mirrored here, so there is no second
    /// copy of the address to fall out of step with the one a listing dials. Mirroring
    /// is what would let the settings screen say one thing and the library another.
    var url: String { WebDavBridge.shared.url() }
    var username: String { WebDavBridge.shared.username() }
    var hasPassword: Bool { WebDavBridge.shared.hasPassword() }
    var isConfigured: Bool { WebDavBridge.shared.isConfigured() }

    /// A short description of the share for a settings row: the address, and the
    /// account on it when there is one.
    var subtitle: String {
        guard !url.isEmpty else { return "Not set up" }
        return username.isEmpty ? url : "\(url) · \(username)"
    }

    // MARK: - Loading

    /// Read the share.
    ///
    /// Safe to call on every appearance: a load already in flight is not restarted,
    /// and one that finished is not repeated. What *does* re-read is a change to the
    /// address, which is a different library.
    func load(force: Bool = false) async {
        guard isConfigured else {
            state = .notConfigured
            albums = []
            trackCount = 0
            return
        }
        if !force, state == .loading { return }
        if !force, case .loaded = state { return }
        state = .loading
        do {
            let songs = try await fetchLibrary()
            guard !Task.isCancelled else { return }
            albums = Self.group(songs)
            trackCount = songs.count
            state = songs.isEmpty ? .empty : .loaded
        } catch {
            guard !Task.isCancelled else { return }
            // The sentence comes from the shared module, which has already mapped the
            // status to something a person can act on. What arrives here is never a
            // host name and a stack trace: a listener's share address is not something
            // to put in front of them in an error.
            albums = []
            trackCount = 0
            state = .failed(Self.describe(error))
        }
    }

    // MARK: - The share's settings

    /// Store the share.
    ///
    /// `password` nil means "leave the stored one alone", which is what a form
    /// showing a filled password field it never read the value of has to do — writing
    /// the empty string back would forget the credential because the form did not
    /// have it.
    func save(url: String, username: String, password: String?) {
        WebDavBridge.shared.save(url: url, username: username, password: password)
    }

    /// Forget the share and its credential together.
    func forget() {
        WebDavBridge.shared.forget()
        state = .notConfigured
        albums = []
        trackCount = 0
    }

    /// Whether an address and a credential work, as the reason they did not.
    ///
    /// Null for success. The sentence is a `WebDavFailure`'s, which is written in
    /// terms somebody can act on — "that server did not accept the username and
    /// password" rather than a status.
    func test(url: String, username: String, password: String?) async -> String? {
        do {
            _ = try await testShare(url: url, username: username, password: password)
            return nil
        } catch {
            return Self.describe(error)
        }
    }

    /// Copy one local audio file to the share. PUTs stream from the file URL and
    /// carry `If-None-Match: *`; a collision gets a numbered sibling rather than
    /// replacing somebody else's recording.
    func upload(path: String, title: String, artist: String) async throws -> String {
        guard isConfigured, !path.hasPrefix("http://"), !path.hasPrefix("https://") else {
            throw WebDavUploadError.notAvailable
        }
        let fileURL = URL(fileURLWithPath: path)
        guard FileManager.default.isReadableFile(atPath: fileURL.path) else {
            throw WebDavUploadError.unreadable
        }
        let ext = fileURL.pathExtension.lowercased()
        let bridge = WebDavBridge.shared
        guard bridge.supportsUploadExtension(fileExtension: ext) else { throw WebDavUploadError.notAvailable }
        let baseName = bridge.uploadFileName(title: title, artist: artist, fileExtension: ext)
        let suffix = ".\(ext)"
        let stem = baseName.hasSuffix(suffix) ? String(baseName.dropLast(suffix.count)) : baseName
        uploadingEntryIDs.insert(path)
        defer { uploadingEntryIDs.remove(path) }

        for number in 0..<1_000 {
            let fileName = number == 0 ? baseName : "\(stem) (\(number))\(suffix)"
            guard let target = URL(string: bridge.uploadFileUrl(fileName: fileName)) else {
                throw WebDavUploadError.invalidAddress
            }
            var request = URLRequest(url: target)
            request.httpMethod = "PUT"
            request.setValue("*", forHTTPHeaderField: "If-None-Match")
            request.setValue(Self.mimeType(for: ext), forHTTPHeaderField: "Content-Type")
            if let authorization = bridge.uploadAuthorizationHeader(fileUrl: target.absoluteString) {
                request.setValue(authorization, forHTTPHeaderField: "Authorization")
            }

            let response: URLResponse
            do {
                (_, response) = try await URLSession.shared.upload(for: request, fromFile: fileURL)
            } catch {
                throw WebDavUploadError.unreachable
            }
            guard let http = response as? HTTPURLResponse else { throw WebDavUploadError.unreachable }
            if http.statusCode == 412 { continue }
            guard (200..<300).contains(http.statusCode) else {
                throw WebDavUploadError.refused(http.statusCode)
            }
            await load(force: true)
            return fileName
        }
        throw WebDavUploadError.tooManyConflicts
    }

    /// Local paths only. A WebDAV row is local to the queue, but its source is a
    /// server URL and copying it back to that same share would be a loop.
    func canUpload(path: String, id: String) -> Bool {
        guard isConfigured, !path.isEmpty, !uploadingEntryIDs.contains(path), !bridgeIsRemote(id) else { return false }
        let ext = URL(fileURLWithPath: path).pathExtension
        return WebDavBridge.shared.supportsUploadExtension(fileExtension: ext)
    }

    private func bridgeIsRemote(_ id: String) -> Bool {
        WebDavBridge.shared.isRemoteId(videoId: id)
    }

    private static func mimeType(for ext: String) -> String {
        switch ext {
        case "mp3": "audio/mpeg"
        case "m4a", "mp4": "audio/mp4"
        case "flac": "audio/flac"
        case "ogg", "opus": "audio/ogg"
        case "aac": "audio/aac"
        case "wav": "audio/wav"
        case "webm": "audio/webm"
        default: "application/octet-stream"
        }
    }

    // MARK: - Grouping

    /// A folder's worth of tracks, with the cover filed beside them.
    struct RemoteAlbum: Identifiable, Equatable {
        /// The folder's name, or nil for tracks at the root of the share.
        var name: String?
        /// The cover for the folder, or nil when the folder has none.
        var coverUrl: String?
        var songs: [Song]
        /// The id of the folder's cover track — every track in a folder shares one, and
        /// it is what the embedded-cover path keys on when no picture was filed.
        var id: String { songs.first?.videoId ?? name ?? "album" }
    }

    /// The listing as albums, in the order a person would expect to read them.
    ///
    /// Albums by name, case- and diacritic-insensitively, because `Ärger` next to
    /// `Art` is a sorting accident rather than an order; tracks by title the same
    /// way. Both localised, so a library in a script the device does not sort by
    /// codepoint still reads correctly.
    static func group(_ songs: [Song]) -> [RemoteAlbum] {
        var order: [String] = []
        var grouped: [String: [Song]] = [:]
        var covers: [String: String] = [:]
        for song in songs {
            let key = song.albumName ?? ""
            if grouped[key] == nil {
                grouped[key] = []
                order.append(key)
            }
            grouped[key]?.append(song)
            // The first track in a folder that has a filed cover gives the album its
            // picture. One cover per album, not per track: the same bytes fetched
            // twenty times for twenty rows of one release is twenty requests to say
            // the same thing.
            if covers[key] == nil, let url = song.thumbnailUrl { covers[key] = url }
        }
        return order
            .sorted { $0.localizedStandardCompare($1) == .orderedAscending }
            .map { key in
                RemoteAlbum(
                    name: grouped[key]?.first?.albumName,
                    coverUrl: covers[key],
                    songs: (grouped[key] ?? []).sorted {
                        $0.title.localizedStandardCompare($1.title) == .orderedAscending
                    },
                )
            }
    }

    // MARK: - Plumbing

    /// A sentence to show, from whatever came back.
    ///
    /// A Kotlin exception's own message when there is one, because the shared module
    /// writes those as sentences for this purpose. A network failure that arrived some
    /// other way is described without its details: the raw error carries a host name
    /// and a framework stack, and neither belongs in front of a listener.
    static func describe(_ error: Error) -> String {
        let ns = error as NSError
        if ns.domain == "KotlinException" {
            let said = ns.localizedDescription.trimmingCharacters(in: .whitespacesAndNewlines)
            if !said.isEmpty { return said }
        }
        if let url = ns.userInfo[NSURLErrorFailingURLErrorKey] as? URL {
            return "Couldn’t reach \(url.host() ?? "that server")."
        }
        return "Couldn’t reach that server."
    }
}

private enum WebDavUploadError: LocalizedError {
    case notAvailable, unreadable, invalidAddress, unreachable, refused(Int), tooManyConflicts

    var errorDescription: String? {
        switch self {
        case .notAvailable: "This track cannot be uploaded to WebDAV."
        case .unreadable: "Could not read this audio file."
        case .invalidAddress, .unreachable: "Could not reach the WebDAV share."
        case .refused(401): "The server did not accept the WebDAV username and password."
        case .refused(403): "The server refused the upload."
        case .refused(507): "The WebDAV share is out of storage."
        case let .refused(status): "The WebDAV server refused the upload (\(status))."
        case .tooManyConflicts: "Could not find an unused name for this upload."
        }
    }
}

// MARK: - Bridging the suspending calls
//
// Kotlin suspend functions arrive in Swift as completion handlers and a library
// listing is awaited from a view rather than from a callback. These keep the
// continuation in one place, and each resumes exactly once: a call that reported
// neither a value nor a reason resumes with an error of its own rather than
// hanging, which is a failure mode a callback API should not have.

private enum WebDavStoreError: Error {
    case noValue
}

private func fetchLibrary() async throws -> [Song] {
    try await withCheckedThrowingContinuation { continuation in
        WebDavBridge.shared.library { songs, error in
            if let error { continuation.resume(throwing: error) }
            else if let songs { continuation.resume(returning: songs) }
            else { continuation.resume(throwing: WebDavStoreError.noValue) }
        }
    }
}

private func testShare(url: String, username: String, password: String?) async throws -> String {
    try await withCheckedThrowingContinuation { continuation in
        WebDavBridge.shared.test(url: url, username: username, password: password) { _, error in
            if let error { continuation.resume(throwing: error) }
            else { continuation.resume(returning: "") }
        }
    }
}
