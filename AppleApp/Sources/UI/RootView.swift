import SwiftUI
import BitChordShared
#if os(iOS)
import UIKit
#elseif os(macOS)
import AppKit
#endif

/// Root navigation shell — one `TabView` with `.sidebarAdaptable` (UI spec §2):
/// bottom tab bar on iOS, Apple Music's leading sidebar on macOS, from a
/// single declaration. Exactly four canonical tabs, upstream icons, search
/// role pinned trailing. `@SceneStorage` keeps the last tab across launches.
struct RootView: View {
    @SceneStorage("bitchord.selectedTab") private var selection: Int = AppModel.Tab.home.rawValue
    @Environment(PlaybackController.self) private var controller
    @Environment(AppModel.self) private var appModel
    @Environment(AuthController.self) private var auth
    @Environment(ToastCenter.self) private var toast
    @State private var updateChecker = UpdateChecker.shared
    #if os(macOS)
    @Environment(\.openSettings) private var openSettings
    /// Which side panel is open beside the library, if any (M4).
    @State private var macInspector: MacInspectorKind?
    #endif
    @State private var homeFeed = FeedLoader(.home)
    #if os(iOS)
    @Namespace private var nowPlayingZoom
    #endif
    @AppStorage(OnboardingView.doneKey) private var onboardingDone = false
    @State private var onboardingPresented = false

    var body: some View {
        shell
        .task {
            // First run only. Someone upgrading — signed in, or with a queue
            // to come back to — has already onboarded themselves.
            guard !onboardingDone else { return }
            try? await Task.sleep(for: .milliseconds(700))
            if auth.signedIn || !controller.queue.isEmpty {
                onboardingDone = true
            } else {
                onboardingPresented = true
            }
        }
        #if os(iOS)
        .fullScreenCover(isPresented: $onboardingPresented) { OnboardingView() }
        #else
        .sheet(isPresented: $onboardingPresented) { OnboardingView() }
        #endif
        .onChange(of: appModel.requestedTab) { _, requested in
            guard let requested else { return }
            selection = resolvedTab(requested).rawValue
            appModel.requestedTab = nil
        }
        .sheet(isPresented: Binding(
            get: { auth.loginPresented },
            set: { auth.loginPresented = $0 }
        )) {
            NavigationStack {
                YtMusicLoginView(
                    onCaptured: { session, done in
                        auth.accept(session) { accepted in
                            if accepted { auth.loginPresented = false }
                            done(accepted)
                        }
                    },
                    onDismiss: { auth.loginPresented = false }
                )
            }
            #if os(macOS)
            .frame(minWidth: 720, minHeight: 640)
            #endif
        }
        .sheet(item: Binding(
            get: { appModel.playlistPicker },
            set: { appModel.playlistPicker = $0 }
        )) { req in
            PlaylistPickerView(request: req)
        }
        .sheet(isPresented: Binding(
            get: { appModel.downloadManagerPresented },
            set: { appModel.downloadManagerPresented = $0 }
        )) {
            DownloadManagerView()
        }
        .sheet(isPresented: Binding(
            get: { appModel.replayPresented },
            set: { appModel.replayPresented = $0 }
        )) {
            ReplayView()
        }
        // Listen Together, presented over whatever the listener was doing. A party
        // invite is a request to look at a party, and the screen that answers it has
        // to be the one that arrives.
        .sheet(isPresented: Binding(
            get: { appModel.listenTogetherPresented },
            set: { appModel.listenTogetherPresented = $0 }
        )) {
            NavigationStack { ListenTogetherView() }
        }
        .sheet(isPresented: Binding(
            get: { appModel.partyMembersPresented },
            set: { appModel.partyMembersPresented = $0 }
        )) {
            PartyMembersSheet()
                .environment(PartyStore.shared)
        }
        // The first-run model offer. Its own sheet at the root, because it is the
        // one question the app asks before it has been used at all and it must not
        // be attached to a screen the listener might never open.
        .sheet(isPresented: Binding(
            get: { appModel.automixModelsPresented },
            set: { appModel.automixModelsPresented = $0 }
        )) {
            AutomixModelsSheet(onDecline: { appModel.declineAutomixModels() })
        }
        .sheet(item: Binding(
            get: { appModel.pendingDetail },
            set: { appModel.pendingDetail = $0 }
        )) { dest in
            NavigationStack {
                if case .detail(let id, let title) = dest {
                    DetailView(browseId: id, initialTitle: title)
                        .toolbar {
                            ToolbarItem(placement: .cancellationAction) {
                                Button("Close") { appModel.pendingDetail = nil }
                            }
                        }
                }
            }
            #if os(macOS)
            .frame(minWidth: 720, minHeight: 640)
            #endif
        }
        .alert("Only 5 playlists can be pinned", isPresented: Binding(
            get: { appModel.pinLimitAlert },
            set: { appModel.pinLimitAlert = $0 }
        )) {
            Button("OK", role: .cancel) {}
        }
        .alert("Rename Playlist", isPresented: Binding(
            get: { appModel.playlistRename != nil },
            set: { if !$0 { appModel.playlistRename = nil } }
        )) {
            RenamePlaylistAlert()
        }
        #if os(iOS)
        .alert("A BitChord update is available", isPresented: Binding(
            get: { updateChecker.available != nil },
            set: { if !$0 { updateChecker.dismiss() } }
        )) {
            Button("View Release Notes") {
                if let release = updateChecker.available,
                   let url = URL(string: release.releaseUrl) {
                    UIApplication.shared.open(url)
                }
                updateChecker.dismiss()
            }
            Button("Later", role: .cancel) { updateChecker.dismiss() }
        } message: {
            Text("BitChord \(updateChecker.available?.version ?? "") is available. Update using the same method you used to install BitChord. The release page has the change notes.")
        }
        #endif
        .onChange(of: tabBinding.wrappedValue) { old, new in
            if new == .search { appModel.focusSearch = true }
            _ = old
        }
        #if os(macOS)
        .frame(minWidth: 860, minHeight: 600)
        // A menu item that asks for the queue or the lyrics opens the inspector
        // — unless the full player is up, in which case it owns both panes and
        // answers the request itself.
        .onChange(of: appModel.queueRevealRequested) { _, requested in
            guard requested, !appModel.nowPlayingPresented else { return }
            macInspector = .queue
            appModel.queueRevealRequested = false
        }
        .onChange(of: appModel.lyricsRevealRequested) { _, requested in
            guard requested, !appModel.nowPlayingPresented else { return }
            macInspector = .lyrics
            appModel.lyricsRevealRequested = false
        }
        #endif
        .preferredColorScheme(appModel.preferredScheme)
        .environment(\.locale, appModel.appLanguage.isEmpty ? .autoupdatingCurrent : Locale(identifier: appModel.appLanguage))
        .overlay {
            // One host, at the top of the tree, so a notice raised from a sheet or
            // a context menu is never clipped by the view that raised it. Same
            // reason upstream puts `QueueActionNoticeHost` above everything else.
            ToastHost()
        }
        // Playback failures are reported here rather than as a banner inside each
        // feed. `lastError` is a *playback* error — "Audio engine failed to start"
        // has nothing to do with a feed that happened to be on screen — and it
        // used to be rendered as a hard-coded red bar at the top of Home,
        // Explore and Search results, which is both the wrong place and the wrong
        // severity for a transient failure.
        .onChange(of: controller.lastError) { _, message in
            guard let message, !message.isEmpty else { return }
            toast.show(message, kind: .failure)
        }
    }

    @ViewBuilder
    private var shell: some View {
        #if os(macOS)
        // Keep the tab hierarchy mounted while the player covers the window.
        // Removing it loses navigation paths, scroll offsets, and local loaders.
        ZStack {
            tabShell
                .opacity(appModel.nowPlayingPresented ? 0 : 1)
                .allowsHitTesting(!appModel.nowPlayingPresented)
                .accessibilityHidden(appModel.nowPlayingPresented)
            if appModel.nowPlayingPresented {
                NowPlayingView()
            }
        }
        #else
        // Keep every tab and its navigation/scroll state mounted while Settings
        // covers it. Replacing tabShell recreates the feeds when returning.
        ZStack {
            tabShell
                .opacity(appModel.settingsPresented ? 0 : 1)
                .allowsHitTesting(!appModel.settingsPresented)
                .accessibilityHidden(appModel.settingsPresented)
            if appModel.settingsPresented {
                NavigationStack {
                    SettingsView(embedded: true)
                        .toolbar {
                            ToolbarItem(placement: .topBarLeading) {
                                Button { appModel.settingsPresented = false } label: {
                                    Label("Back", systemImage: "chevron.left")
                                }
                            }
                        }
                }
            }
        }
            .environment(\.nowPlayingZoomNamespace, nowPlayingZoom)
            .modifier(NowPlayingTakeover(
                isPresented: Binding(
                    get: { appModel.nowPlayingPresented },
                    set: { appModel.nowPlayingPresented = $0 }
                ),
                zoomNamespace: nowPlayingZoom
            ))
        #endif
    }

    private var tabShell: some View {
        TabView(selection: tabBinding) {
            #if os(macOS)
            Tab(value: AppModel.Tab.home) {
                HomeView(feed: homeFeed)
            } label: {
                // Navigation is one icon family: SF Symbols, as the system
                // sidebar and tab bar draw them (upstream's own tab glyphs are
                // now copies of these iOS shapes). Brand glyphs stay on the
                // player, where they are the identity rather than chrome.
                Label("Home", systemImage: "house")
            }
            Tab(value: AppModel.Tab.explore) {
                ExploreView()
            } label: {
                Label("Explore", systemImage: "square.grid.2x2")
            }
            TabSection("Library") {
                Tab(value: AppModel.Tab.libraryYouTube) {
                    LibraryView(lockedSection: .youtube)
                } label: {
                    // Upstream has no distinct Recent glyph; the `bchLibrary`
                    // shelves are already taken by Albums below. The repeat-clock
                    // SF Symbol stays distinct from History's plain `bchClock`.
                    Label("Recent", systemImage: "clock.arrow.circlepath")
                }
                Tab(value: AppModel.Tab.librarySongs) {
                    LibraryView(lockedSection: .songs)
                } label: {
                    Label("Songs", systemImage: "music.note")
                }
                Tab(value: AppModel.Tab.libraryAlbums) {
                    LibraryView(lockedSection: .albums)
                } label: {
                    // Songs keeps the music note and Albums keeps the library
                    // shelves — the more fitting half of each conflicted pair.
                    Label("Albums", systemImage: "square.stack")
                }
                Tab(value: AppModel.Tab.libraryArtists) {
                    LibraryView(lockedSection: .artists)
                } label: {
                    // Upstream has no distinct Artists glyph and the music note
                    // is already taken by Songs above; the mic reads as the
                    // performer rather than the track.
                    Label("Artists", systemImage: "music.mic")
                }
                Tab(value: AppModel.Tab.libraryPlaylists) {
                    LibraryView(lockedSection: .playlists)
                } label: {
                    Label("Playlists", systemImage: "music.note.list")
                }
                Tab(value: AppModel.Tab.librarySubscriptions) {
                    LibraryView(lockedSection: .subscriptions)
                } label: {
                    Label("Subscriptions", systemImage: "bell")
                }
                Tab(value: AppModel.Tab.libraryPodcasts) {
                    LibraryView(lockedSection: .podcasts)
                } label: {
                    Label("Podcasts", systemImage: "radio")
                }
                Tab(value: AppModel.Tab.libraryOnDevice) {
                    LibraryView(lockedSection: .ondevice)
                } label: {
                    Label("On Device", systemImage: "externaldrive")
                }
                Tab(value: AppModel.Tab.libraryHistory) {
                    LibraryView(lockedSection: .history)
                } label: {
                    Label("History", systemImage: "clock")
                }
                // Only when there is a share. A sidebar row that leads to "not set up
                // yet" is a row that exists to be disappointing, and Sources is where
                // this is configured.
                if WebDavStore.shared.isConfigured {
                    Tab(value: AppModel.Tab.libraryWebDav) {
                        LibraryView(lockedSection: .webdav)
                    } label: {
                        // An SF Symbol, not one of the `bch` images the other rows use.
                        // Upstream puts Material's `Cloud` on its WebDAV row and has no
                        // glyph to copy, and the Sources screen already draws this
                        // feature's row with the same symbol — so the two agree.
                        Label("WebDAV", systemImage: "cloud")
                    }
                }
            }
            Tab(value: AppModel.Tab.search, role: .search) {
                SearchView()
            } label: {
                Label("Search", systemImage: "magnifyingglass")
            }
            #else
            if UIDevice.current.userInterfaceIdiom == .pad {
                Tab("Home", systemImage: "house.fill", value: AppModel.Tab.home) {
                    HomeView(feed: homeFeed).modifier(iPadPlaybackChrome())
                }
                Tab("Explore", systemImage: "square.grid.2x2.fill", value: AppModel.Tab.explore) {
                    ExploreView().modifier(iPadPlaybackChrome())
                }
                TabSection("Library") {
                    Tab(value: AppModel.Tab.libraryYouTube) {
                        LibraryView(lockedSection: .youtube).modifier(iPadPlaybackChrome())
                    } label: {
                        Label("Recent", systemImage: "clock.arrow.circlepath")
                    }
                    Tab("Songs", systemImage: "music.note", value: AppModel.Tab.librarySongs) {
                        LibraryView(lockedSection: .songs).modifier(iPadPlaybackChrome())
                    }
                    Tab("Albums", systemImage: "square.stack", value: AppModel.Tab.libraryAlbums) {
                        LibraryView(lockedSection: .albums).modifier(iPadPlaybackChrome())
                    }
                    Tab(value: AppModel.Tab.libraryArtists) {
                        LibraryView(lockedSection: .artists).modifier(iPadPlaybackChrome())
                    } label: {
                        Label("Artists", systemImage: "music.mic")
                    }
                    Tab(value: AppModel.Tab.libraryPlaylists) {
                        LibraryView(lockedSection: .playlists).modifier(iPadPlaybackChrome())
                    } label: {
                        Label("Playlists", systemImage: "music.note.list")
                    }
                    Tab(value: AppModel.Tab.librarySubscriptions) {
                        LibraryView(lockedSection: .subscriptions).modifier(iPadPlaybackChrome())
                    } label: {
                        Label("Subscriptions", systemImage: "bell")
                    }
                    Tab(value: AppModel.Tab.libraryPodcasts) {
                        LibraryView(lockedSection: .podcasts).modifier(iPadPlaybackChrome())
                    } label: {
                        Label("Podcasts", systemImage: "radio")
                    }
                    Tab(value: AppModel.Tab.libraryOnDevice) {
                        LibraryView(lockedSection: .ondevice).modifier(iPadPlaybackChrome())
                    } label: {
                        Label("On Device", systemImage: "externaldrive")
                    }
                    Tab("History", systemImage: "clock", value: AppModel.Tab.libraryHistory) {
                        LibraryView(lockedSection: .history).modifier(iPadPlaybackChrome())
                    }
                    if WebDavStore.shared.isConfigured {
                        Tab(value: AppModel.Tab.libraryWebDav) {
                            LibraryView(lockedSection: .webdav).modifier(iPadPlaybackChrome())
                        } label: {
                            Label("WebDAV", systemImage: "cloud")
                        }
                    }
                }
                Tab("Search", systemImage: "magnifyingglass", value: AppModel.Tab.search, role: .search) {
                    SearchView().modifier(iPadPlaybackChrome())
                }
            } else {
                Tab("Home", systemImage: "house.fill", value: AppModel.Tab.home) {
                    HomeView(feed: homeFeed)
                }
                Tab("Explore", systemImage: "square.grid.2x2.fill", value: AppModel.Tab.explore) {
                    ExploreView()
                }
                Tab("Library", systemImage: "square.stack.fill", value: AppModel.Tab.library) {
                    LibraryView()
                }
                Tab("Search", systemImage: "magnifyingglass", value: AppModel.Tab.search, role: .search) {
                    SearchView()
                }
            }
            #endif
        }
        .tabViewStyle(.sidebarAdaptable)
        #if os(iOS)
        .defaultAdaptableTabBarPlacement(.sidebar)
        #endif
        #if os(macOS)
        .environment(\.sidebarRowSize, .medium)
        .overlay(alignment: .topLeading) {
            MacWindowToolbar(isPlayerPresented: appModel.nowPlayingPresented)
                .frame(width: 0, height: 0)
            MacSidebarWidth()
                .frame(width: 0, height: 0)
        }
        // Lyrics and the queue are a trailing panel, not another screen: the
        // library stays visible and the inspector sits beside it.
        .inspector(isPresented: Binding(
            get: { macInspector != nil },
            set: { if !$0 { macInspector = nil } }
        )) {
            MacInspectorPane(kind: macInspector ?? .queue)
                .inspectorColumnWidth(min: 280, ideal: 340, max: 460)
        }
        .toolbar {
            if !appModel.nowPlayingPresented {
                // Music's arrangement: the transport at the leading edge, the
                // readout in the middle, the panel and output controls trailing.
                ToolbarItem(placement: .navigation) {
                    MacTransportControls()
                }
                ToolbarItem(placement: .principal) {
                    MacNowPlayingLCD()
                }
                ToolbarItemGroup(placement: .primaryAction) {
                    MacInspectorToggles(selection: $macInspector)
                    if !controller.hideVolumeBar {
                        MacVolumeControl()
                    }
                    AirPlayRouteButton()
                        .frame(width: 22, height: 22)
                        .frame(width: 28, height: 24)
                        .help("AirPlay")
                        .accessibilityLabel("AirPlay")
                }
                ToolbarItem {
                    Button {
                        appModel.downloadManagerPresented = true
                    } label: {
                        Image(systemName: "arrow.down.circle")
                    }
                    .help("Downloads")
                    .accessibilityLabel("Downloads")
                }
                ToolbarItem {
                    Button {
                        openSettings()
                    } label: {
                        Image(systemName: "gearshape")
                    }
                    .help("Settings")
                    .accessibilityLabel("Settings")
                }
            }
        }
        #endif
        .modifier(PlaybackPillMount())
    }



    private var tabBinding: Binding<AppModel.Tab> {
        Binding(
            get: { resolvedTab(AppModel.Tab(rawValue: selection) ?? .home) },
            set: { newTab in
                // Upstream's `searchFocusTrigger`: re-tapping the tab that is
                // already selected focuses its search field rather than doing
                // nothing. A `TabView` cannot tell a re-tap from a no-op tap on
                // its own — the binding's setter only sees the value it is being
                // set to — so the "same tab again" case is recognised here.
                //
                // Only Search, because a field is the only thing on any of the
                // other tabs that a re-tap could reasonably mean. A re-tap that
                // scrolled Home to the top or popped the Library back would be
                // inventing behaviour nobody asked for.
                if newTab == .search, newTab == resolvedTab(AppModel.Tab(rawValue: selection) ?? .home) {
                    appModel.searchFocusTrigger &+= 1
                }
                selection = newTab.rawValue
            }
        )
    }

    private func resolvedTab(_ tab: AppModel.Tab) -> AppModel.Tab {
        #if os(macOS)
        if tab == .library { return .libraryYouTube }
        return tab
        #else
        if UIDevice.current.userInterfaceIdiom == .pad && tab == .library {
            return .libraryYouTube
        }
        return tab
        #endif
    }
}

#if os(macOS)
/// Configure the system-generated toolbar items without creating another
/// sidebar button. TabView puts its toggle in the sidebar's toolbar section;
/// that section can have no room for it when the sidebar is collapsed.
private struct MacWindowToolbar: NSViewRepresentable {
    let isPlayerPresented: Bool

    func makeNSView(context: Context) -> MacWindowToolbarView { MacWindowToolbarView() }
    func updateNSView(_ nsView: MacWindowToolbarView, context: Context) {
        nsView.setPlayerPresented(isPlayerPresented)
    }
}

private final class MacWindowToolbarView: NSView {
    private var isPlayerPresented = false
    private var wasPlayerPresented = false
    private var normalItems: [NSToolbarItem.Identifier: Bool] = [:]
    private var observers: [NSObjectProtocol] = []
    private var updateScheduled = false

    func setPlayerPresented(_ presented: Bool) {
        guard isPlayerPresented != presented else { return }
        isPlayerPresented = presented
        scheduleUpdate()
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        observers.forEach { NotificationCenter.default.removeObserver($0) }
        observers.removeAll()
        guard let window else { return }
        for name in [NSToolbar.willAddItemNotification, NSToolbar.didRemoveItemNotification] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] note in
                guard let self, (note.object as? NSToolbar) === self.window?.toolbar else { return }
                self.scheduleUpdate()
            })
        }
        observers.append(NotificationCenter.default.addObserver(forName: NSWindow.didResizeNotification, object: window, queue: .main) { [weak self] _ in
            self?.scheduleUpdate()
        })
        scheduleUpdate()
    }

    deinit {
        observers.forEach { NotificationCenter.default.removeObserver($0) }
    }

    private func scheduleUpdate() {
        guard window != nil, !updateScheduled else { return }
        updateScheduled = true
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.updateScheduled = false
            self.configureToolbar()
        }
    }

    private func configureToolbar() {
        guard let toolbar = window?.toolbar else { return }
        if isPlayerPresented {
            for item in toolbar.items where normalItems[item.itemIdentifier] != nil {
                item.isHidden = true
            }
        } else {
            if wasPlayerPresented {
                for item in toolbar.items {
                    if let hidden = normalItems[item.itemIdentifier] { item.isHidden = hidden }
                }
            }
            // The sidebar toggle belongs in the sidebar's own titlebar, beside
            // the traffic lights, as in Music and Finder — not floating at the
            // head of the content column. Keep SwiftUI's own control and
            // action: reinserting its identifier asks the window's existing
            // toolbar delegate for the same item.
            if let toggleIndex = toolbar.items.firstIndex(where: Self.isSidebarToggle),
               let dividerIndex = toolbar.items.firstIndex(where: {
                   $0 is NSTrackingSeparatorToolbarItem
               }), toggleIndex > dividerIndex {
                let identifier = toolbar.items[toggleIndex].itemIdentifier
                toolbar.removeItem(at: toggleIndex)
                toolbar.insertItem(withItemIdentifier: identifier, at: dividerIndex)
            }
            if let toggle = toolbar.items.first(where: Self.isSidebarToggle) {
                toggle.visibilityPriority = .user
            }
            normalItems = Dictionary(toolbar.items.map { ($0.itemIdentifier, $0.isHidden) }, uniquingKeysWith: { first, _ in first })
        }
        wasPlayerPresented = isPlayerPresented
    }

    private static func isSidebarToggle(_ item: NSToolbarItem) -> Bool {
        item.itemIdentifier == .toggleSidebar
            || item.itemIdentifier.rawValue.hasSuffix(".toggleSidebar")
    }
}
#endif

/// The shared playback pill (UI spec §3.1/§3.2): `.tabViewBottomAccessory` on
/// iOS 26 — Music's own bottom accessory — with a material `safeAreaInset`
/// fallback below that floor. Same view, mounted per platform.
///
/// The accessory is tab-level, so it is unaffected by iPad sidebar
/// collapse/expand; inset mounting broke that and overlapped the tab bar.
struct PlaybackPillMount: ViewModifier {
    func body(content: Content) -> some View {
        #if os(iOS)
        if UIDevice.current.userInterfaceIdiom == .pad {
            // iPad mounts per tab (iPadPlaybackChrome): the tab-level
            // accessory container spans full width and stretches the pill,
            // and a TabView-level inset never reflows with the sidebar.
            content
        } else if #available(iOS 26.0, *) {
            content.tabViewBottomAccessory {
                PlaybackPill()
                    .frame(maxWidth: .infinity)
            }
        } else {
            fallbackMount(content)
        }
        #else
        // macOS has no pill: the transport lives in the window toolbar.
        content
        #endif
    }

    #if os(iOS)
    private func fallbackMount(_ content: Content) -> some View {
        content.safeAreaInset(edge: .bottom) {
            PlaybackPill()
                // Clamped to 720pt so it centers elegantly on iPad while taking
                // natural width on iPhone.
                .frame(maxWidth: 720)
                .padding(.horizontal, 10)
                .padding(.bottom, 8)
                .frame(maxWidth: .infinity)
        }
    }
    #endif
}

#if os(iOS)
/// iPad per-tab pill mount. Lives inside the detail column so sidebar
/// collapse/expand reflows it and it stays centered on the main view. Content-sized at 620pt like Music's
/// compact pill instead of the full-width stretch of the tab-level
/// accessory container. No-op on iPhone, which mounts once at the TabView.
struct iPadPlaybackChrome: ViewModifier {
    func body(content: Content) -> some View {
        if UIDevice.current.userInterfaceIdiom == .pad {
            content.safeAreaInset(edge: .bottom) {
                PlaybackPill()
                    .frame(maxWidth: 620)
                    .padding(.horizontal, 10)
                    .padding(.bottom, 8)
                    .frame(maxWidth: .infinity)
            }
        } else {
            content
        }
    }
}
#endif

#if os(iOS)
/// iPhone: a large swipe-to-dismiss sheet that zooms out of the mini player
/// (WWDC 323 `MusicPlaybackView` pattern). iPad: a page-sized sheet.
struct NowPlayingTakeover: ViewModifier {
    @Binding var isPresented: Bool
    var zoomNamespace: Namespace.ID

    func body(content: Content) -> some View {
        Group {
            if UIDevice.current.userInterfaceIdiom == .pad {
                // Full screen, unconditionally: `.page` sheet sizing renders
                // as a floating card on iPadOS 27, and only a cover is
                // guaranteed edge to edge.
                content.fullScreenCover(isPresented: $isPresented) {
                    NowPlayingView()
                        .navigationTransition(.zoom(sourceID: NowPlayingZoom.sourceID, in: zoomNamespace))
                }
            } else {
                content.sheet(isPresented: $isPresented) {
                    NowPlayingView()
                        .modifier(NowPlayingSheetSizing())
                        .navigationTransition(.zoom(sourceID: NowPlayingZoom.sourceID, in: zoomNamespace))
                }
            }
        }
    }
}

private struct NowPlayingSheetSizing: ViewModifier {
    @ViewBuilder
    func body(content: Content) -> some View {
        if UIDevice.current.userInterfaceIdiom == .pad {
            content
                .presentationDetents([.large])
                .presentationSizing(.page)
                .presentationDragIndicator(.visible)
        } else {
            content
                .presentationDetents([.large])
                .presentationDragIndicator(.visible)
                .presentationCornerRadius(24)
        }
    }
}

enum NowPlayingZoom {
    static let sourceID = "now-playing"
}

private struct NowPlayingZoomNamespaceKey: EnvironmentKey {
    static let defaultValue: Namespace.ID? = nil
}

extension EnvironmentValues {
    var nowPlayingZoomNamespace: Namespace.ID? {
        get { self[NowPlayingZoomNamespaceKey.self] }
        set { self[NowPlayingZoomNamespaceKey.self] = newValue }
    }
}
#endif
