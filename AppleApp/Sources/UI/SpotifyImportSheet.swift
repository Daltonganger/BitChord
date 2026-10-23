import SwiftUI
import BitChordShared

/// Import a Spotify playlist, matched track by track to YouTube Music.
///
/// The work happens in `SpotifyImportBridge`: this screen pastes a link (or
/// picks one of the account's playlists when its `sp_dc` cookie is set), shows
/// each row as it is decided, and creates the playlist from the rows that
/// matched. Rows that only *might* be right are shown with what was found, so
/// they can be judged rather than silently dropped — the import takes the clear
/// matches and says how many it left out.
struct SpotifyImportSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(AuthController.self) private var auth
    @Environment(ToastCenter.self) private var toast

    @State private var link = ""
    @State private var playlists: [SpotifyPlaylistsPayload.Item] = []
    @State private var playlist: SpotifyPlaylistPayload?
    @State private var rows: [SpotifyImportRow] = []
    @State private var phase: Phase = .idle
    @State private var error: String?
    @State private var creating = false

    private enum Phase: Equatable {
        case idle
        case loadingPlaylists
        case loadingPlaylist
        case matching
    }

    private var hasCookie: Bool {
        !PlatformSettings.shared.getString(key: "spotify_spdc_token", default: "").isEmpty
    }

    private var matchedRows: [SpotifyImportRow] { rows.filter { $0.status == .matched } }
    private var settledRows: Int { rows.filter { $0.status != .waiting && $0.status != .searching }.count }

    var body: some View {
        NavigationStack {
            Group {
                if auth.signedIn {
                    content
                } else {
                    EmptyStateView(
                        icon: Image(.bchMusicNote),
                        title: "Sign in to import",
                        subtitle: "A Spotify import becomes a playlist on your YouTube Music account, so it needs a signed-in account.",
                        buttonTitle: nil,
                        action: nil
                    )
                }
            }
            .navigationTitle("Import from Spotify")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(createTitle) { Task { await create() } }
                        .disabled(matchedRows.isEmpty || creating || phase == .matching)
                }
            }
        }
        .task { await loadPlaylistsIfPossible() }
        #if os(macOS)
        .frame(minWidth: 560, minHeight: 520)
        #endif
    }

    private var createTitle: String {
        if creating { return "Creating…" }
        return matchedRows.count == 1 ? "Create Playlist (1)" : "Create Playlist (\(matchedRows.count))"
    }

    // MARK: - Content

    @ViewBuilder
    private var content: some View {
        List {
            Section {
                TextField("Paste a Spotify playlist link", text: $link)
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.URL)
                    #endif
                Button("Load Playlist") { Task { await load(input: link) } }
                    .disabled(link.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || phase == .loadingPlaylist)
            } footer: {
                if hasCookie {
                    Text("Paste the share link, or pick one of your playlists below.")
                } else {
                    Text("Paste a share link. To list your own playlists, add the sp_dc cookie in Settings › Spotify Canvas.")
                }
            }

            if hasCookie, phase == .loadingPlaylists {
                Section { ProgressView() }
            } else if !playlists.isEmpty {
                Section("Your Spotify playlists") {
                    ForEach(playlists) { item in
                        Button {
                            Task { await load(input: item.id) }
                        } label: {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(item.name).lineLimit(1)
                                Text(item.trackCount == 1 ? "1 track" : "\(item.trackCount) tracks")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .buttonStyle(.plain)
                    }
                }
            }

            if let playlist {
                Section {
                    ForEach(rows) { row in
                        SpotifyImportRowView(row: row)
                    }
                } header: {
                    HStack {
                        Text(playlist.name)
                        Spacer()
                        if phase == .matching {
                            Text("\(settledRows) of \(rows.count)")
                                .monospacedDigit()
                        }
                    }
                } footer: {
                    if phase == .matching {
                        Text("Matching \(rows.count) tracks to YouTube Music…")
                    } else if rows.count > matchedRows.count {
                        Text("\(matchedRows.count) of \(rows.count) tracks will be imported. The rest are shown as a choice or missing and are left out.")
                    }
                }
            }

            if let error {
                Section {
                    Text(error).foregroundStyle(.red)
                }
            }
        }
        #if os(iOS)
        .listStyle(.insetGrouped)
        #else
        .listStyle(.inset)
        #endif
    }

    // MARK: - Work

    private func loadPlaylistsIfPossible() async {
        guard auth.signedIn, hasCookie, playlists.isEmpty else { return }
        phase = .loadingPlaylists
        defer { phase = .idle }
        guard let payload = await fetchPlaylists() else { return }
        playlists = payload
    }

    private func load(input: String) async {
        let trimmed = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        error = nil
        phase = .loadingPlaylist
        defer { if phase == .loadingPlaylist { phase = .idle } }
        guard let loaded = await fetchPlaylist(trimmed) else { return }
        playlist = loaded
        link = trimmed
        rows = loaded.tracks.map { SpotifyImportRow(id: $0.id, title: $0.title, artists: $0.artists, durationMs: $0.durationMs) }
        await matchAll()
    }

    /// Matches the rows a few at a time, so the list fills in as it goes instead
    /// of arriving all at once at the end.
    private func matchAll() async {
        guard !rows.isEmpty else { return }
        phase = .matching
        let tracks = rows.map { (id: $0.id, title: $0.title, artists: $0.artists, durationMs: $0.durationMs) }
        await withTaskGroup(of: (String, SpotifyMatchResult).self) { group in
            var next = 0
            let width = 3
            func addTask() {
                guard next < tracks.count else { return }
                let track = tracks[next]
                next += 1
                group.addTask {
                    (track.id, await fetchMatch(track.title, track.artists, track.durationMs))
                }
            }
            for _ in 0..<min(width, tracks.count) { addTask() }
            for await (id, result) in group {
                apply(result, to: id)
                addTask()
            }
        }
        phase = .idle
    }

    private func apply(_ result: SpotifyMatchResult, to id: String) {
        guard let index = rows.firstIndex(where: { $0.id == id }) else { return }
        switch result {
        case .failure(let message):
            rows[index].status = .missing
            rows[index].detail = message
        case .success(let match):
            switch match.status {
            case "MATCHED":
                rows[index].status = .matched
                rows[index].videoId = match.videoId
                rows[index].matchedTitle = match.title
                rows[index].matchedArtist = match.artist
                rows[index].detail = nil
            case "AMBIGUOUS":
                rows[index].status = .ambiguous
                rows[index].matchedTitle = match.title
                rows[index].matchedArtist = match.artist
                rows[index].detail = "Close, but not certain"
            default:
                rows[index].status = .missing
                rows[index].detail = nil
            }
        }
    }

    private func create() async {
        guard !matchedRows.isEmpty else { return }
        creating = true
        defer { creating = false }
        let name = playlist?.name ?? "Spotify import"
        let ids = matchedRows.compactMap(\.videoId)
        let failure = await withCheckedContinuation { (cont: CheckedContinuation<String?, Never>) in
            SpotifyImportBridge.shared.createPlaylist(
                name: name, privacy: "PRIVATE", videoIds: ids,
                callback: BridgeCallback { json, message in
                    if let json,
                       let data = json.data(using: .utf8),
                       let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                       object["playlistId"] is String {
                        cont.resume(returning: nil)
                    } else {
                        cont.resume(returning: message ?? "The playlist could not be created.")
                    }
                }
            )
        }
        if let failure {
            error = failure
        } else {
            toast.show("Imported “\(name)” with \(ids.count) of \(rows.count) tracks")
            dismiss()
        }
    }

    // MARK: - Bridge

    private func fetchPlaylists() async -> [SpotifyPlaylistsPayload.Item]? {
        await withCheckedContinuation { (cont: CheckedContinuation<[SpotifyPlaylistsPayload.Item]?, Never>) in
            SpotifyImportBridge.shared.playlists(callback: BridgeCallback { json, message in
                if let json, let payload = SpotifyPlaylistsPayload.decode(json) {
                    cont.resume(returning: payload.playlists)
                } else {
                    cont.resume(returning: nil)
                    _ = message
                }
            })
        }
    }

    private func fetchPlaylist(_ input: String) async -> SpotifyPlaylistPayload? {
        await withCheckedContinuation { (cont: CheckedContinuation<SpotifyPlaylistPayload?, Never>) in
            SpotifyImportBridge.shared.playlist(input: input, callback: BridgeCallback { json, message in
                if let json, let payload = SpotifyPlaylistPayload.decode(json) {
                    cont.resume(returning: payload)
                } else {
                    Task { @MainActor in self.error = message ?? "That playlist could not be read." }
                    cont.resume(returning: nil)
                }
            })
        }
    }

    private func fetchMatch(_ title: String, _ artists: String, _ durationMs: Int64) async -> SpotifyMatchResult {
        await withCheckedContinuation { (cont: CheckedContinuation<SpotifyMatchResult, Never>) in
            SpotifyImportBridge.shared.match(
                title: title, artists: artists, durationMs: durationMs,
                callback: BridgeCallback { json, message in
                    if let json, let payload = SpotifyMatchPayload.decode(json) {
                        cont.resume(returning: .success(payload))
                    } else {
                        cont.resume(returning: .failure(message ?? "Search failed."))
                    }
                }
            )
        }
    }
}

// MARK: - Rows

struct SpotifyImportRow: Identifiable {
    enum Status {
        case waiting
        case searching
        case matched
        case ambiguous
        case missing
    }

    let id: String
    let title: String
    let artists: String
    let durationMs: Int64
    var status: Status = .waiting
    var videoId: String?
    var matchedTitle: String?
    var matchedArtist: String?
    var detail: String?
}

private struct SpotifyImportRowView: View {
    let row: SpotifyImportRow

    var body: some View {
        HStack(spacing: 10) {
            statusGlyph
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 2) {
                Text(row.title).lineLimit(1)
                Text(row.artists).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                if let matched = row.matchedTitle, row.status != .matched {
                    Text(matched + (row.matchedArtist.map { " · \($0)" } ?? ""))
                        .font(.caption2)
                        .foregroundStyle(.orange)
                        .lineLimit(1)
                } else if let detail = row.detail, row.status == .missing {
                    Text(detail).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            Spacer(minLength: 8)
            Text(statusLabel)
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var statusGlyph: some View {
        switch row.status {
        case .waiting:
            Image(systemName: "circle.dotted").foregroundStyle(.tertiary)
        case .searching:
            ProgressView().controlSize(.small)
        case .matched:
            Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .ambiguous:
            Image(systemName: "questionmark.circle.fill").foregroundStyle(.orange)
        case .missing:
            Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
        }
    }

    private var statusLabel: String {
        switch row.status {
        case .waiting: ""
        case .searching: "Searching"
        case .matched: "Matched"
        case .ambiguous: "Maybe"
        case .missing: "Not found"
        }
    }
}

// MARK: - Payloads

private struct SpotifyPlaylistsPayload: Decodable {
    struct Item: Decodable, Identifiable {
        let id: String
        let name: String
        let trackCount: Int
    }

    let playlists: [Item]

    static func decode(_ json: String) -> Self? {
        json.data(using: .utf8).flatMap { try? JSONDecoder().decode(Self.self, from: $0) }
    }
}

private struct SpotifyPlaylistPayload: Decodable {
    struct Track: Decodable {
        let title: String
        let artists: String
        let durationMs: Int64
        let album: String?

        /// One row's identity within this import: the same song can appear twice
        /// in a playlist, so the title alone is not enough.
        var id: String { "\(title)\u{1}\(artists)\u{1}\(durationMs)" }
    }

    let id: String
    let name: String
    let tracks: [Track]

    static func decode(_ json: String) -> Self? {
        json.data(using: .utf8).flatMap { try? JSONDecoder().decode(Self.self, from: $0) }
    }
}

private struct SpotifyMatchPayload: Decodable {
    let status: String
    let videoId: String?
    let title: String?
    let artist: String?
    let durationText: String?
    let thumbnailUrl: String?
    let confidence: Double

    static func decode(_ json: String) -> Self? {
        json.data(using: .utf8).flatMap { try? JSONDecoder().decode(Self.self, from: $0) }
    }
}

private enum SpotifyMatchResult {
    case success(SpotifyMatchPayload)
    case failure(String)
}

/// The Kotlin bridge calls back off the main thread.
private final class BridgeCallback: SpotifyImportBridgeResultCallback {
    let handler: (String?, String?) -> Void

    init(_ handler: @escaping (String?, String?) -> Void) {
        self.handler = handler
    }

    func onResult(json: String?, message: String?) {
        handler(json, message)
    }
}
