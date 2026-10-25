#if os(iOS)
import SwiftUI

/// The iPad's sidebar, drawn by the app rather than by `TabView`.
///
/// The system's adaptable sidebar floats as an inset glass card over the
/// content. This one is the column many iPad chat apps use: full
/// height, flush with the screen edge, revealed by the content moving aside.
/// With the room to spare (landscape) the content reflows beside it; without
/// (portrait) the content slides over by the sidebar's width and a tap on what
/// is left of it closes the sidebar again.
///
/// It still works with the tab bar rather than replacing it. Closed, the top
/// tab bar is there as before; open, the sidebar is the navigation and the bar
/// steps aside. Both drive the same selection.
@MainActor
@Observable
final class IPadSidebar {
    var isOpen: Bool {
        didSet { UserDefaults.standard.set(isOpen, forKey: Self.openKey) }
    }

    /// How many root pages are showing the toggle at this moment. A page pushed
    /// on top of a root replaces its toolbar, so this drops to zero exactly when
    /// the leading edge belongs to the back swipe — and the edge swipe that opens
    /// the sidebar must not take it.
    var rootToggles = 0

    /// Whether the content reflows beside the sidebar (landscape) or slides over
    /// (portrait). Sliding over hides most of the page, so choosing a row closes
    /// the sidebar there, as in the reference apps; beside it, it stays.
    var reflows = true

    static let openKey = "ipad_sidebar_open"
    static let spring = Animation.spring(response: 0.42, dampingFraction: 0.86)

    init() {
        isOpen = UserDefaults.standard.object(forKey: Self.openKey) as? Bool ?? true
    }

    func toggle() {
        withAnimation(Self.spring) { isOpen.toggle() }
    }

    func close() {
        guard isOpen else { return }
        withAnimation(Self.spring) { isOpen = false }
    }
}

private struct IPadSidebarKey: EnvironmentKey {
    static let defaultValue: IPadSidebar? = nil
}

extension EnvironmentValues {
    /// Set only inside the regular-width iPad shell. Nil everywhere else, which is
    /// how a root page knows to show the BitChord mark instead of the toggle.
    var iPadSidebar: IPadSidebar? {
        get { self[IPadSidebarKey.self] }
        set { self[IPadSidebarKey.self] = newValue }
    }
}

/// The toggle a root page puts where the BitChord mark would be.
struct IPadSidebarToggle: View {
    let sidebar: IPadSidebar

    var body: some View {
        Button { sidebar.toggle() } label: {
            Image(systemName: "sidebar.leading")
                .scaledFont(size: 17, weight: .medium)
                .foregroundStyle(.primary)
                .frame(width: 34, height: 34)
                .contentShape(.rect)
        }
        .buttonStyle(ProfileCircleButtonStyle())
        .accessibilityLabel(sidebar.isOpen ? "Hide Sidebar" : "Show Sidebar")
        .onAppear { sidebar.rootToggles += 1 }
        .onDisappear { sidebar.rootToggles = max(0, sidebar.rootToggles - 1) }
    }
}

/// Lays the sidebar and the content side by side and moves between the two
/// states, following a finger while one is dragging.
struct IPadSidebarShell<Sidebar: View, Content: View>: View {
    @Bindable var state: IPadSidebar
    /// Off in compact width, where the tab bar is the navigation. The content
    /// keeps its place in the tree either way, so turning the sidebar on or off
    /// (a rotation, a Split View resize) does not rebuild the tabs.
    var enabled: Bool
    @ViewBuilder var sidebar: Sidebar
    @ViewBuilder var content: Content

    /// The finger's travel while a drag is under way; zero otherwise.
    @State private var drag: CGFloat = 0
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// Below this the content would be squeezed into a phone's width beside the
    /// sidebar, so it slides over instead.
    private static var reflowWidth: CGFloat { 1000 }

    var body: some View {
        GeometryReader { geo in
            let width = min(320, max(280, (geo.size.width * 0.27).rounded()))
            let reflows = geo.size.width >= Self.reflowWidth
            let base: CGFloat = enabled && state.isOpen ? width : 0
            let shown = enabled ? min(max(base + drag, 0), width) : 0
            let progress = shown / width

            ZStack(alignment: .topLeading) {
                if enabled {
                    sidebar
                    .frame(width: width)
                    .frame(maxHeight: .infinity)
                    // A little slower than the content, so it reads as underneath.
                    .offset(x: reduceMotion ? 0 : (shown - width) * 0.3)
                    .opacity(reduceMotion ? progress : 1)
                    .simultaneousGesture(closingDrag(width: width))
                    .allowsHitTesting(state.isOpen)
                    .accessibilityHidden(!state.isOpen)
                }

                content
                    .frame(width: reflows ? geo.size.width - base : geo.size.width)
                    .frame(maxHeight: .infinity)
                    .overlay {
                        if enabled, !reflows {
                            Color.black
                                .opacity(0.14 * progress)
                                .ignoresSafeArea()
                                .allowsHitTesting(state.isOpen)
                                .onTapGesture { state.close() }
                                .gesture(closingDrag(width: width))
                                .accessibilityHidden(true)
                        }
                    }
                    .overlay(alignment: .leading) {
                        // The hairline the content's edge makes against the
                        // sidebar, as in the reference apps; a shadow when it is
                        // sliding over rather than sitting beside.
                        Rectangle()
                            .fill(Color.primary.opacity(0.08))
                            .frame(width: 0.5)
                            .ignoresSafeArea()
                            .opacity(progress)
                    }
                    .shadow(color: .black.opacity(reflows ? 0 : 0.18 * progress), radius: 18)
                    .offset(x: reflows ? base + (shown - base) : shown)
                    .overlay(alignment: .leading) { edgeStrip(width: width) }
            }
            .background { if enabled { keyboardToggle } }
            .onAppear { state.reflows = reflows }
            .onChange(of: reflows) { _, now in state.reflows = now }
        }
    }

    /// The strip along the leading edge that opens the sidebar on a swipe. Only
    /// there while a root page is up: on a pushed page the edge is the back swipe.
    @ViewBuilder
    private func edgeStrip(width: CGFloat) -> some View {
        if enabled, !state.isOpen, state.rootToggles > 0 {
            Color.clear
                .frame(width: 18)
                .frame(maxHeight: .infinity)
                .contentShape(.rect)
                .ignoresSafeArea()
                .gesture(
                    DragGesture(minimumDistance: 8)
                        .onChanged { drag = max(0, $0.translation.width) }
                        .onEnded { settle($0, width: width) }
                )
                .accessibilityHidden(true)
        }
    }

    private func closingDrag(width: CGFloat) -> some Gesture {
        DragGesture(minimumDistance: 14)
            .onChanged { value in
                // Only a leftward, mostly horizontal drag closes; anything else
                // is the sidebar's own list scrolling.
                guard state.isOpen,
                      value.translation.width < 0,
                      abs(value.translation.width) > abs(value.translation.height) else { return }
                drag = value.translation.width
            }
            .onEnded { value in
                guard state.isOpen, drag != 0 else { return }
                settle(value, width: width)
            }
    }

    /// Where a released drag comes to rest: past halfway, counting the fling's
    /// momentum, it finishes the move; short of it, it springs back.
    private func settle(_ value: DragGesture.Value, width: CGFloat) {
        let base: CGFloat = state.isOpen ? width : 0
        let landing = base + value.predictedEndTranslation.width
        withAnimation(IPadSidebar.spring) {
            state.isOpen = landing > width / 2
            drag = 0
        }
    }

    /// ⌃⌘S, the system's sidebar shortcut, from a hardware keyboard.
    private var keyboardToggle: some View {
        Button("Toggle Sidebar") { state.toggle() }
            .keyboardShortcut("s", modifiers: [.command, .control])
            .opacity(0)
            .accessibilityHidden(true)
    }
}

/// What the sidebar lists: the four tabs, the library's sections, and the
/// listener's own playlists — the same rows the Mac's sidebar has.
struct IPadSidebarView: View {
    @Binding var selection: AppModel.Tab
    let playlists: [UserPlaylistDTO]
    @Binding var openPlaylist: UserPlaylistDTO?
    let state: IPadSidebar

    @Environment(AppModel.self) private var appModel
    @Environment(AuthController.self) private var auth

    var body: some View {
        VStack(spacing: 0) {
            header
            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    row(.home, "Home", "house")
                    row(.explore, "Explore", "square.grid.2x2")
                    row(.search, "Search", "magnifyingglass")

                    sectionTitle("Library")
                    row(.libraryYouTube, "Recent", "clock.arrow.circlepath")
                    row(.librarySongs, "Songs", "music.note")
                    row(.libraryAlbums, "Albums", "square.stack")
                    row(.libraryArtists, "Artists", "music.mic")
                    row(.libraryPlaylists, "Playlists", "music.note.list")
                    row(.librarySubscriptions, "Subscriptions", "bell")
                    row(.libraryPodcasts, "Podcasts", "radio")
                    row(.libraryOnDevice, "On Device", "externaldrive")
                    row(.libraryHistory, "History", "clock")
                    if WebDavStore.shared.isConfigured {
                        row(.libraryWebDav, "WebDAV", "cloud")
                    }

                    if !playlists.isEmpty {
                        sectionTitle("Playlists")
                        ForEach(playlists) { playlist in
                            playlistRow(playlist)
                        }
                    }
                }
                .padding(.horizontal, 10)
                .padding(.bottom, 12)
            }
            .scrollIndicators(.hidden)
            footer
        }
        .background(Color(uiColor: .secondarySystemBackground).ignoresSafeArea())
    }

    private var header: some View {
        HStack(spacing: 10) {
            Image(.bchLogo)
                .resizable()
                .scaledToFit()
                .frame(width: 28, height: 18)
                .accessibilityHidden(true)
            Text("BitChord")
                .font(.title3.weight(.semibold))
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 18)
        .frame(height: 44)
        .padding(.top, 8)
        .padding(.bottom, 4)
    }

    private var footer: some View {
        HStack(spacing: 10) {
            TopBarAccountButton()
            Text(auth.signedIn ? (auth.accountName ?? "Account") : "Not signed in")
                .font(.subheadline.weight(.medium))
                .foregroundStyle(auth.signedIn ? .primary : .secondary)
                .lineLimit(1)
            Spacer(minLength: 0)
            Button {
                appModel.settingsPresented = true
            } label: {
                Image(systemName: "gearshape")
                    .scaledFont(size: 17, weight: .medium)
                    .foregroundStyle(.primary)
                    .frame(width: 36, height: 36)
                    .contentShape(.rect)
            }
            .buttonStyle(ProfileCircleButtonStyle())
            .accessibilityLabel("Settings")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    private func sectionTitle(_ title: LocalizedStringKey) -> some View {
        Text(title)
            .font(.footnote.weight(.semibold))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 10)
            .padding(.top, 18)
            .padding(.bottom, 4)
            .accessibilityAddTraits(.isHeader)
    }

    private func row(_ tab: AppModel.Tab, _ title: LocalizedStringKey, _ symbol: String) -> some View {
        let selected = selection == tab && !appModel.settingsPresented
        return Button { choose(tab) } label: {
            Label {
                Text(title).lineLimit(1)
            } icon: {
                Image(systemName: symbol)
                    .frame(width: 24)
            }
            .font(.callout.weight(selected ? .semibold : .regular))
            .foregroundStyle(.primary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 10)
            .frame(height: 40)
            .background(selected ? Color.primary.opacity(0.08) : .clear, in: .rect(cornerRadius: 10, style: .continuous))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private func playlistRow(_ playlist: UserPlaylistDTO) -> some View {
        let selected = selection == .sidebarPlaylist && openPlaylist?.id == playlist.id && !appModel.settingsPresented
        return Button {
            openPlaylist = playlist
            choose(.sidebarPlaylist)
        } label: {
            HStack(spacing: 10) {
                ArtworkView(url: playlist.thumbnailUrl, data: nil, side: 26)
                    .frame(width: 26, height: 26)
                    .clipShape(.rect(cornerRadius: 5, style: .continuous))
                Text(playlist.title)
                    .lineLimit(1)
            }
            .font(.callout.weight(selected ? .semibold : .regular))
            .foregroundStyle(.primary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 9)
            .frame(height: 40)
            .background(selected ? Color.primary.opacity(0.08) : .clear, in: .rect(cornerRadius: 10, style: .continuous))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private func choose(_ tab: AppModel.Tab) {
        appModel.settingsPresented = false
        selection = tab
        if !state.reflows { state.close() }
    }
}
#endif
