import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif
import BitChordShared
import UniformTypeIdentifiers

/// The playlists behind one mood or genre category — upstream
/// `MoodGenrePlaylistsScreen`, a push rather than a sheet because it is a page
/// with its own scroll position and a back button, not an action on what is on
/// screen.
struct MoodGenrePlaylistsView: View {
    let category: MoodGenre
    @Environment(PlaybackController.self) private var controller
    @Environment(AuthController.self) private var auth
    @Environment(AppModel.self) private var appModel
    @State private var shelves: [FeedShelf] = []
    @State private var dataGeneration: Int64?
    @State private var phase: LoadPhase = .loading

    enum LoadPhase { case loading, loaded, failed(String) }

    var body: some View {
        Group {
            switch phase {
            case .loading:
                ScrollView { FeedSkeleton() }
            case .failed(let message):
                EmptyStateView(
                    icon: Image(.bchExplore),
                    title: "Nothing here right now",
                    subtitle: message,
                    buttonTitle: "Retry"
                ) { Task { await load() } }
            case .loaded:
                ScrollView {
                    VStack(alignment: .leading, spacing: 28) {
                        ForEach(shelves) { shelf in
                            VStack(alignment: .leading, spacing: 10) {
                                HStack {
                                    Text(shelf.title)
                                        .font(.headline)
                                        .lineLimit(2)
                                    Spacer()
                                    if shelf.items.count > 5 {
                                        PushLink {
                                            LibraryGridView(title: shelf.title, items: shelf.items)
                                        } label: {
                                            HStack(spacing: 2) {
                                                Text("Show all")
                                                Image(systemName: "chevron.right")
                                                    .font(.caption.weight(.semibold))
                                            }
                                            .font(.callout)
                                            .foregroundStyle(.tint)
                                        }
                                    }
                                }
                                ScrollView(.horizontal, showsIndicators: false) {
                                    LazyHStack(alignment: .top, spacing: 14) {
                                        ForEach(shelf.items) { card in
                                            ShelfCardView(card: card, preferTrack: true)
                                        }
                                    }
                                    .padding(.vertical, 2)
                                }
                            }
                        }
                    }
                    .padding(.horizontal, 24)
                    .padding(.vertical, 20)
                }
                .refreshable { await load(force: true) }
            }
        }
        .navigationTitle(category.title)
        #if os(iOS)
        .navigationBarTitleDisplayMode(.large)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                TopBarAccountButton()
            }
        }
        #endif
        .task(id: auth.sessionEpoch) { shelves = []; phase = .loading; await load() }
    }

    private func load(force: Bool = false) async {
        let generation = PageSession.generation()
        if shelves.isEmpty { phase = .loading }
        do {
            let result = try await InnertubeFeed.shared.moodGenreShelves(
                browseId: category.browseId, params: category.params, force: force
            )
            guard generation == PageSession.generation(), !Task.isCancelled else { return }
            shelves = result
            phase = .loaded
        } catch {
            guard generation == PageSession.generation(), !Task.isCancelled else { return }
            if !shelves.isEmpty { return }
            phase = .failed(error.localizedDescription)
        }
    }
}

/// Loads the mood/genre categories, and their tile artwork afterwards.
///
/// The two-step shape is upstream's and it is deliberate: the grid is worth
/// painting before it is worth labelling with pictures, and the artwork comes
/// from each category's page. Simultaneous requests share the network work;
/// completed page data is not retained.
@MainActor @Observable
final class MoodGenreLoader {
    enum Phase { case loading, loaded([MoodGenreSection]), failed(String) }

    private(set) var phase: Phase = .loading
    private var loadedGeneration: Int64?
    private var requestID = UUID()

    func load(force: Bool) async {
        let generation = PageSession.generation()
        let id = UUID(); requestID = id
        if loadedGeneration != generation { phase = .loading }
        loadedGeneration = generation
        do {
            var sections = try await InnertubeFeed.shared.moodAndGenres(force: force)
            guard generation == PageSession.generation(), requestID == id, !Task.isCancelled else { return }
            phase = .loaded(sections)
            await fillArtwork(&sections, generation: generation, id: id)
        } catch {
            guard generation == PageSession.generation(), requestID == id, !Task.isCancelled else { return }
            if case .loaded = phase { return }
            phase = .failed(error.localizedDescription)
        }
    }

    /// Replaces the categories with ones carrying artwork, once each has
    /// answered. Published one section at a time so the first covers to arrive
    /// are on screen while the rest are still being asked for.
    private func fillArtwork(_ sections: inout [MoodGenreSection], generation: Int64, id: UUID) async {
        // Start in display order and cap concurrency: visible tiles go first.
        let jobs = sections.indices.flatMap { section in
            sections[section].items.indices.compactMap { item -> (Int, Int, MoodGenre)? in
                let category = sections[section].items[item]
                return category.thumbnailUrl == nil ? (section, item, category) : nil
            }
        }
        await withTaskGroup(of: (Int, Int, String?).self) { group in
            var next = 0
            func enqueue() {
                guard next < jobs.count else { return }
                let job = jobs[next]; next += 1
                group.addTask {
                    let cover = await InnertubeFeed.shared.moodGenreArtwork(browseId: job.2.browseId, params: job.2.params)
                    return (job.0, job.1, cover)
                }
            }
            for _ in 0..<min(4, jobs.count) { enqueue() }
            while let (section, item, cover) = await group.next() {
                guard generation == PageSession.generation(), requestID == id, !Task.isCancelled else {
                    group.cancelAll(); return
                }
                if let cover { sections[section].items[item].thumbnailUrl = cover; publish(sections) }
                enqueue()
            }
        }
    }

    private func publish(_ sections: [MoodGenreSection]) {
        guard case .loaded = phase else { return }
        phase = .loaded(sections)
    }
}
