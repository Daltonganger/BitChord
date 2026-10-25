import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif
import BitChordShared
import UniformTypeIdentifiers

struct LibraryGridView: View {
    let title: String
    let items: [ShelfCard]
    var isPlaylists: Bool = false

    @Environment(AppModel.self) private var appModel
    #if os(macOS)
    @Environment(\.openWindow) private var openWindow
    #endif
    @State private var searchQuery = ""
    @State private var sortOrder: SortOrder = .defaultOrder

    enum SortOrder: String, CaseIterable, Identifiable {
        case defaultOrder = "Default"
        case titleAsc = "Title (A–Z)"
        case titleDesc = "Title (Z–A)"
        var id: String { rawValue }
    }

    private var sortedAndFilteredItems: [ShelfCard] {
        var result = items
        if !searchQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            let q = searchQuery.lowercased()
            result = result.filter {
                $0.title.lowercased().contains(q) || ($0.subtitle?.lowercased().contains(q) == true)
            }
        }
        switch sortOrder {
        case .defaultOrder:
            let pinned = PlaylistPinning.pinnedIds()
            if isPlaylists, !pinned.isEmpty {
                result.sort { a, b in
                    let ap = pinned.contains(a.browseId ?? "")
                    let bp = pinned.contains(b.browseId ?? "")
                    if ap == bp { return false }
                    return ap && !bp
                }
            }
        case .titleAsc:
            result.sort { $0.title.localizedCaseInsensitiveCompare($1.title) == .orderedAscending }
        case .titleDesc:
            result.sort { $0.title.localizedCaseInsensitiveCompare($1.title) == .orderedDescending }
        }
        return result
    }

    var body: some View {
        ScrollView {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 150, maximum: 180), spacing: 16)], spacing: 20) {
                if isPlaylists {
                    NewPlaylistTile {
                        appModel.playlistPicker = PlaylistPickerRequest(videoId: "", title: "")
                    }
                }
                ForEach(sortedAndFilteredItems) { card in
                    ShelfCardView(card: card)
                }
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 16)
        }
        .navigationTitle(title)
        #if os(iOS)
        .navigationBarTitleDisplayMode(.large)
        .searchable(text: $searchQuery, prompt: "Filter \(title)")
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                Menu {
                    Picker("Sort by", selection: $sortOrder) {
                        ForEach(SortOrder.allCases) { order in
                            Text(order.rawValue).tag(order)
                        }
                    }
                } label: {
                    Image(systemName: "arrow.up.arrow.down")
                        .scaledFont(size: 14, weight: .medium)
                        .foregroundStyle(.primary)
                        .frame(width: 34, height: 34)
                        .toolbarChrome(in: Circle())
                }
                .buttonStyle(ProfileCircleButtonStyle())
                .accessibilityLabel("Sort \(title)")

                TopBarAccountButton()
            }
        }
        #else
        .searchable(text: $searchQuery, prompt: "Filter \(title)")
        .toolbar {
            ToolbarItem {
                Menu {
                    Picker("Sort by", selection: $sortOrder) {
                        ForEach(SortOrder.allCases) { order in
                            Text(order.rawValue).tag(order)
                        }
                    }
                } label: {
                    Label("Sort", systemImage: "arrow.up.arrow.down")
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Button {
                    openWindow(
                        id: MacBrowseWindow.sceneID,
                        value: MacBrowseWindowRoute.grid(title: title, items: items)
                    )
                } label: {
                    Label("Open in New Window", systemImage: "macwindow.badge.plus")
                }
                .help("Open this grid in a new window")
            }
        }
        #endif
    }
}

/// One local track as a card, for the grid.
///
/// Artwork first because that is the point of a grid: the reason to look at a
/// collection rather than search it. A file with no embedded artwork gets a
/// generated tile from its own initials rather than a blank square, so a library
/// of untagged files is still recognisable at a glance.
struct LocalTrackCard: View {
    let track: LocalTrack
    let play: () -> Void

    var body: some View {
        Button(action: play) {
            VStack(alignment: .leading, spacing: 8) {
                artwork
                    .aspectRatio(1, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .strokeBorder(.separator, lineWidth: 0.5)
                    )
                VStack(alignment: .leading, spacing: 2) {
                    Text(track.title)
                        .font(.subheadline.weight(.medium))
                        .lineLimit(1)
                    Text(track.artist)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .modifier(MacCardHoverLift())
        .help("\(track.title) — \(track.artist)")
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(track.title), \(track.artist)")
        .accessibilityHint("Plays this track")
    }

    @ViewBuilder
    private var artwork: some View {
        if let data = track.artwork, hasArtwork(data) {
            decoded(data)
        } else {
            ZStack {
                Rectangle().fill(.quaternary)
                Text(initials)
                    .font(.title2.weight(.semibold))
                    .foregroundStyle(.secondary)
            }
        }
    }

    /// Whether this data decodes to an image at all.
    ///
    /// Separate from [decoded] because the card body is a `ViewBuilder`, and a
    /// guard around a view-building call cannot be expressed without either
    /// decoding twice or letting a bad image through.
    private func hasArtwork(_ data: Data) -> Bool {
        #if os(macOS)
        return NSImage(data: data) != nil
        #else
        return UIImage(data: data) != nil
        #endif
    }

    /// The embedded artwork.
    ///
    /// Two spellings because the type is genuinely two types — `UIImage` on iOS,
    /// `NSImage` on macOS — and `Image` initialises from each under a different
    /// label. Wrapped so the card body above reads as one thing rather than as a
    /// platform conditional.
    @ViewBuilder
    private func decoded(_ data: Data) -> some View {
        #if os(macOS)
        if let image = NSImage(data: data) {
            Image(nsImage: image).resizable().aspectRatio(contentMode: .fill)
        }
        #else
        if let image = UIImage(data: data) {
            Image(uiImage: image).resizable().aspectRatio(contentMode: .fill)
        }
        #endif
    }

    /// Up to two initials from the title, falling back to the artist.
    private var initials: String {
        let source = track.title.isEmpty ? track.artist : track.title
        let words = source.split(separator: " ").prefix(2)
        let letters = words.compactMap { $0.first }.map(String.init)
        return letters.isEmpty ? "♪" : letters.joined().uppercased()
    }
}
