import SwiftUI
#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// Loads one Home/Explore feed off the shared bridge, including signed-in
/// continuation paging (upstream `moreHome`).
@MainActor @Observable
final class FeedLoader {
    enum Source { case home, explore }
    enum Phase { case loading, loaded([FeedShelf]), failed(String) }
    static let home = FeedLoader(.home)
    static let explore = FeedLoader(.explore)
    private(set) var phase: Phase = .loading
    private(set) var loadingMore = false
    private(set) var refreshError: String?
    private var continuation: String?
    private var loadedEpoch: Int?
    private var context: PageContext?
    private var loadTask: Task<Void, Never>?
    private var requestID = UUID()
    private let source: Source
    private var pageName: String { source == .home ? "home" : "explore" }

    init(_ source: Source) { self.source = source }
    func load() async { await load(force: true, epoch: loadedEpoch) }

    func load(force: Bool, epoch: Int?) async {
        let current = PageSession.capture()
        if let loadTask, context == current { await loadTask.value; return }
        if !force, context == current, loadedEpoch == epoch, case .loaded = phase { return }
        if context != current {
            loadTask?.cancel(); loadTask = nil
            phase = .loading; continuation = nil
        }
        context = current
        let id = UUID(); requestID = id
        let task = Task { await self.fetch(epoch: epoch, context: current, id: id) }
        loadTask = task
        await task.value
        if requestID == id { loadTask = nil }
    }

    private func fetch(epoch: Int?, context: PageContext, id: UUID) async {
        refreshError = nil
        continuation = nil
        do {
            let started = ContinuousClock.now
            var first = true
            for try await result in InnertubeFeed.shared.progressive(home: source == .home) {
                guard requestID == id, PageSession.generation() == context.generation, !Task.isCancelled else { return }
                if !result.shelves.isEmpty {
                    phase = .loaded(result.shelves)
                    Task { await LaunchReadiness.shared.contentAppeared() }
                    if first {
                        PlaybackDebugLog.shared.record("\(pageName) first content: \(started.duration(to: .now))")
                        first = false
                    }
                }
                continuation = result.continuation
                loadedEpoch = epoch

            }
        } catch {
            guard requestID == id, PageSession.generation() == context.generation, !Task.isCancelled else { return }
            continuation = nil
            if case .loaded = phase { refreshError = error.localizedDescription }
            else { phase = .failed(error.localizedDescription) }
        }
    }

    func loadMore() async {
        guard let context, PageSession.generation() == context.generation,
              let token = continuation, !token.isEmpty, !loadingMore else { return }
        let id = requestID
        loadingMore = true
        defer { loadingMore = false }
        do {
            let result = source == .home ? try await InnertubeFeed.shared.moreHome(token: token)
                : try await InnertubeFeed.shared.moreExplore(token: token)
            guard requestID == id, PageSession.generation() == context.generation, !Task.isCancelled else { return }
            continuation = result.continuation
            if case .loaded(let existing) = phase {
                let seen = Set(existing.map(\.title))
                phase = .loaded(existing + result.shelves.filter { !seen.contains($0.title) })
            }
        } catch {
            guard requestID == id, PageSession.generation() == context.generation, !Task.isCancelled else { return }
            refreshError = error.localizedDescription
        }
    }
}

/// Shared shelf heading: bold headline over a subtitle, with an optional
/// "Show all" chevron link. One shape for Home, Explore and Library so the
/// headings line up across tabs. `onShowAll` is only ever set on Library,
/// whose rows stop at five cards rather than running the shelf's whole
/// length — Home and Explore never pass it.
struct FeedSectionHeader: View {
    let title: String
    var subtitle: String? = nil
    var onShowAll: (() -> Void)? = nil

    var body: some View {
        HStack(alignment: .center) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.headline)
                    .lineLimit(2)
                if let subtitle, !subtitle.isEmpty {
                    Text(subtitle)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 8)
            if let onShowAll {
                Button(action: onShowAll) {
                    HStack(spacing: 2) {
                        Text("Show all")
                        Image(systemName: "chevron.right")
                            .font(.caption.weight(.semibold))
                    }
                    .font(.callout)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.tint)
                .accessibilityLabel("Show all \(title)")
            }
        }
    }
}

/// Upstream's shelf carousel: bold heading over a horizontal run of cards.
/// Cards are tappable — a track card plays immediately, an album/playlist/artist
/// card navigates to its detail page (spec parity with upstream's two-row cards).
struct ShelfCarousel: View {
    let shelf: FeedShelf
    /// Flips which end of a card that has *both* ids wins. See
    /// [ShelfCardView] for why the two orders are not interchangeable.
    var preferTrack: Bool = false
    var onShowAll: (() -> Void)? = nil
    /// The side padding of the page the shelf sits in; see [ShelfScroller].
    var inset: CGFloat = 24
    @Environment(PlaybackController.self) private var controller
    @Environment(\.horizontalSizeClass) private var sizeClass

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            FeedSectionHeader(title: shelf.title, subtitle: shelf.subtitle, onShowAll: onShowAll)
            ShelfScroller(inset: inset) {
                LazyHStack(alignment: .top, spacing: 14) {
                    ForEach(shelf.items) { card in
                        if sizeClass == .regular {
                            // A wide window shows fewer, larger sleeves: the card
                            // is a quarter of the run, and the artwork follows the
                            // room rather than staying a phone-sized 160pt.
                            ShelfCardView(card: card, artworkSide: 220, preferTrack: preferTrack)
                                .containerRelativeFrame(.horizontal, count: 4, spacing: 14)
                        } else {
                            ShelfCardView(card: card, preferTrack: preferTrack)
                        }
                    }
                }
                .padding(.vertical, 2)
            }
        }
    }
}

/// The horizontal run every shelf sits in.
///
/// - **Edge to edge.** The run escapes its parent's side padding and puts it
///   back as a content margin, so cards scroll out to the window edge instead
///   of being sliced off at an invisible line 24 points in.
/// - **Snaps** to a card's leading edge, so a shelf never rests on half a card.
/// - **Pages on hover** (macOS, iPad with a pointer): chevrons at each end
///   move one screenful, the way Music's shelves do. A trackpad swipe still works.
struct ShelfScroller<Content: View>: View {
    var inset: CGFloat = 24
    @ViewBuilder var content: Content

    @State private var position = ScrollPosition(edge: .leading)
    @State private var offset: CGFloat = 0
    @State private var viewport: CGFloat = 0
    @State private var extent: CGFloat = 0
    @State private var hovering = false

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            content.scrollTargetLayout()
        }
        .scrollTargetBehavior(.viewAligned)
        .contentMargins(.horizontal, inset, for: .scrollContent)
        .scrollPosition($position)
        .onScrollGeometryChange(for: [CGFloat].self) { geometry in
            [geometry.contentOffset.x, geometry.containerSize.width, geometry.contentSize.width]
        } action: { _, value in
            offset = value[0]; viewport = value[1]; extent = value[2]
        }
        .padding(.horizontal, -inset)
        .overlay(alignment: .leading) { pager(forward: false) }
        .overlay(alignment: .trailing) { pager(forward: true) }
        .onHover { hovering = $0 }
    }

    @ViewBuilder
    private func pager(forward: Bool) -> some View {
        let canPage = forward ? offset + viewport < extent - 1 : offset > -inset + 1
        if hovering, canPage {
            Button {
                let page = max(viewport - inset * 3, 120)
                let target = forward
                    ? min(offset + page, extent - viewport)
                    : max(offset - page, -inset)
                withAnimation(.smooth(duration: 0.35)) { position.scrollTo(x: target) }
            } label: {
                Image(systemName: forward ? "chevron.right" : "chevron.left")
                    .font(.body.weight(.semibold))
                    .frame(width: 34, height: 34)
                    .background(.regularMaterial, in: .circle)
                    .shadow(color: .black.opacity(0.18), radius: 6, y: 2)
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 8)
            .help(forward ? "Next" : "Previous")
            .accessibilityLabel(forward ? "Scroll shelf forward" : "Scroll shelf back")
            .transition(.opacity)
        }
    }
}

struct ShelfCardView: View {
    let card: ShelfCard
    /// The sleeve's side, and with it the card's width. A carousel on a wide
    /// window passes a larger one; everywhere else this is the phone size.
    var artworkSide: CGFloat = Theme.Metrics.shelfCard
    @Environment(PlaybackController.self) private var controller
    /// Whether the pointer is over the card. A card is a target, not just a
    /// picture, and on the Mac that has to show before the click.
    @State private var hovering = false

    /// Which end of a card carrying *both* a video id and a browse id wins.
    ///
    /// The two orders are not interchangeable, and upstream uses both. On Home
    /// and a detail page the browse id wins: the card is a shelf of albums and
    /// playlists, and a browse id is what the shelf meant. On a mood or genre
    /// category the track wins — upstream branches on `videoId` first there, and
    /// it is the more specific thing the listener aimed at, since the category
    /// page is a list of things to *start*, not things to read.
    var preferTrack: Bool = false

    /// Whether this card's collection is in `pinned_playlists`. Read live so a
    /// pin toggled from a detail page or menu shows without a feed reload.
    private var isPinned: Bool {
        guard let browseId = card.browseId, !browseId.isEmpty else { return false }
        return PlaylistPinning.pinnedIds().contains(browseId)
    }

    var body: some View {
        Group {
            if preferTrack, let videoId = card.videoId, !videoId.isEmpty {
                trackButton(videoId)
            } else if let browseId = card.browseId, !browseId.isEmpty {
                NavigationLink(destination: DetailView(browseId: browseId, initialTitle: card.title)) {
                    cardContent
                }
                .buttonStyle(.plain)
            } else if let videoId = card.videoId, !videoId.isEmpty {
                trackButton(videoId)
            } else {
                cardContent
            }
        }
    }

    private func trackButton(_ videoId: String) -> some View {
        Button {
            controller.playRadio(QueueEntry.youtube(
                videoId: videoId, title: card.title, artist: card.subtitle ?? "",
                thumbnailUrl: card.thumbnailUrl
            ))
        } label: {
            cardContent
                .overlay(alignment: .topLeading) {
                    if controller.current?.id == videoId && controller.isBuffering {
                        ProgressView()
                            .controlSize(.small)
                            .padding(8)
                    }
                }
        }
        .buttonStyle(.plain)
    }

    private var cardContent: some View {
        VStack(alignment: .leading, spacing: 8) {
            ZStack(alignment: .topTrailing) {
                ArtworkView(url: card.thumbnailUrl, data: nil, side: artworkSide)
                    .clipShape(.rect(cornerRadius: 10, style: .continuous))
                    .shadow(color: .black.opacity(0.15), radius: 5, y: 2)
                if isPinned {
                    Image(systemName: "pin.fill")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .padding(6)
                        .background(.thinMaterial, in: Circle())
                        .padding(6)
                        .accessibilityLabel("Pinned")
                }
            }
            Text(card.title)
                .font(.callout.weight(.semibold))
                .lineLimit(1)
            if let subtitle = card.subtitle, !subtitle.isEmpty {
                Text(subtitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .frame(width: artworkSide, alignment: .leading)
        // The highlight reaches past the card's own bounds so the sleeve and the
        // two lines under it read as one target.
        .background {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(hovering ? Color.primary.opacity(0.06) : .clear)
                .padding(-6)
        }
        .contentShape(.rect)
        .onHover { hovering = $0 }
        .shelfCardContextMenu(card)
    }
}

/// Navigation destination for detail drill-down from any shelf.
enum BrowseDestination: Hashable, Identifiable {
    case detail(browseId: String, title: String)
    var id: String {
        switch self {
        case .detail(let browseId, _): browseId
        }
    }
}

/// Upstream's `SignInBanner`. Opens the in-app Google login WebView — never
/// the system browser, which would put the session in Safari instead of here.
struct SignInBanner: View {
    var onSignIn: () -> Void

    var body: some View {
        HStack(spacing: 14) {
            Image(systemName: "person.crop.circle")
                .font(.title2)
                .foregroundStyle(.secondary)
                .decorative()
            VStack(alignment: .leading, spacing: 2) {
                Text("Sign in to YouTube Music")
                    .font(.headline)
                Text("Personalized recommendations and your library, on every device.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 12)
            Button("Sign In", action: onSignIn)
                .buttonStyle(.bordered)
        }
        .padding(16)
        .background(.quaternary.opacity(0.5), in: .rect(cornerRadius: 14, style: .continuous))
    }
}

/// Upstream `feedSkeleton()` — shimmer shelves while the feed loads.
struct FeedSkeleton: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            ForEach(0..<3, id: \.self) { _ in
                VStack(alignment: .leading, spacing: 10) {
                    SkeletonBlock(height: 22, cornerRadius: 6)
                        .frame(width: 140)
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 14) {
                            ForEach(0..<5, id: \.self) { _ in
                                VStack(alignment: .leading, spacing: 8) {
                                    SkeletonBlock(height: 160, cornerRadius: 10)
                                    SkeletonBlock(height: 14, cornerRadius: 4)
                                        .frame(width: 120)
                                    SkeletonBlock(height: 12, cornerRadius: 4)
                                        .frame(width: 80)
                                }
                                .frame(width: 160)
                            }
                        }
                    }
                }
            }
        }
        .padding(.horizontal, 24)
        .padding(.vertical, 20)
    }
}

/// First Home shelf as a larger pager, matching upstream's hero row.
struct HeroShelf: View {
    let shelf: FeedShelf

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(shelf.title)
                .font(.title2.weight(.bold))
            ShelfScroller {
                LazyHStack(spacing: 16) {
                    ForEach(shelf.items) { card in
                        ShelfCardView(card: card)
                    }
                }
            }
        }
    }
}
