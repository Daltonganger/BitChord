#if os(macOS)
import SwiftUI
import AppKit
import BitChordShared

/// Which panel is open beside the library.
///
/// Lyrics and the queue used to be destinations *inside* the full player, which
/// on the Mac meant the whole window. Music keeps them in a trailing inspector
/// so the library stays where it is while you read the lyrics — this is that
/// panel, and the player keeps them too for when it is open.
enum MacInspectorKind: String, Identifiable {
    case lyrics
    case queue

    var id: String { rawValue }

    var label: String {
        self == .lyrics ? "Lyrics" : "Queue"
    }
}

extension EnvironmentValues {
    /// The inspector the pill's lyrics and queue buttons open and close.
    @Entry var macInspectorSelection: Binding<MacInspectorKind?> = .constant(nil)
}

/// The floating pill under a page in the browse stack. Each page carries it,
/// the root and every pushed one: an inset on the stack itself reaches only
/// its root page.
struct MacPillInset: ViewModifier {
    func body(content: Content) -> some View {
        content.safeAreaInset(edge: .bottom, spacing: 0) {
            PlaybackPill()
                .frame(maxWidth: 940)
                .padding(.horizontal, 18)
                .padding(.bottom, 12)
                .frame(maxWidth: .infinity)
        }
    }
}

// MARK: - Full screen

/// Enters or leaves full screen, and draws which: arrows pointing out to go
/// full screen, pointing in to come back.
struct MacFullScreenButton: View {
    @State private var isFullScreen = false

    var body: some View {
        Button(
            isFullScreen ? "Exit Full Screen" : "Enter Full Screen",
            systemImage: isFullScreen
                ? "arrow.down.right.and.arrow.up.left"
                : "arrow.up.left.and.arrow.down.right"
        ) {
            NSApp.keyWindow?.toggleFullScreen(nil)
        }
        .keyboardShortcut("f", modifiers: [.command, .control])
        .labelStyle(.iconOnly)
        .help(isFullScreen ? "Exit Full Screen" : "Enter Full Screen")
        .accessibilityLabel(isFullScreen ? "Exit Full Screen" : "Enter Full Screen")
        .onAppear {
            isFullScreen = NSApp.keyWindow?.styleMask.contains(.fullScreen) ?? false
        }
        .onReceive(NotificationCenter.default.publisher(for: NSWindow.didEnterFullScreenNotification)) { _ in
            isFullScreen = true
        }
        .onReceive(NotificationCenter.default.publisher(for: NSWindow.didExitFullScreenNotification)) { _ in
            isFullScreen = false
        }
    }
}

// MARK: - Downloads

/// The titlebar's Downloads button. The list opens as a popover anchored to
/// the button, so a click anywhere else in the window puts it away, rather
/// than a sheet that holds the window until it is dismissed.
struct MacDownloadsButton: View {
    @Environment(AppModel.self) private var appModel

    var body: some View {
        @Bindable var appModel = appModel
        Button {
            appModel.downloadManagerPresented.toggle()
        } label: {
            Image(systemName: "arrow.down.circle")
        }
        .help("Downloads")
        .accessibilityLabel("Downloads")
        .popover(isPresented: $appModel.downloadManagerPresented, arrowEdge: .bottom) {
            DownloadManagerView()
                .frame(width: 420, height: 520)
        }
    }
}

// MARK: - Bare-key transport

/// Space plays and pauses, ← and → step the queue — for the whole window, from
/// wherever the listener is.
///
/// These are not menu key equivalents, and that is the point: a menu shortcut with
/// no modifier is matched before the focused view sees the event, so a Space menu
/// item takes the space out of the search box, the playlist name and the cookie
/// field. Here the key arrives only if nothing else took it — a text field's field
/// editor inserts the space and the event never climbs this far — which is the
/// behaviour Music has.
struct MacTransportKeys: ViewModifier {
    let controller: PlaybackController

    func body(content: Content) -> some View {
        content
            .onKeyPress(.space, phases: .down) { press in
                guard press.modifiers.isEmpty else { return .ignored }
                controller.togglePlayPause()
                return .handled
            }
            .onKeyPress(.leftArrow, phases: .down) { press in
                guard press.modifiers.isEmpty else { return .ignored }
                controller.previous()
                return .handled
            }
            .onKeyPress(.rightArrow, phases: .down) { press in
                guard press.modifiers.isEmpty else { return .ignored }
                controller.next()
                return .handled
            }
    }
}

// MARK: - Inspector

/// What the inspector shows: the same panes the player uses, so a permanent
/// queue panel and the player's queue column can never disagree.
struct MacInspectorPane: View {
    let kind: MacInspectorKind
    @Environment(PlaybackController.self) private var controller

    var body: some View {
        Group {
            switch kind {
            case .lyrics:
                MacLyricsInspector()
            case .queue:
                UpNextPane(showsCurrentTrackHeader: true)
            }
        }
        .background {
            // The panes are drawn for the player's dark backdrop and its white
            // text. The inspector gives them that backdrop rather than a second
            // light-mode styling of the same content.
            MeshBackdrop(
                seed: controller.current?.id.hashValue ?? 0,
                artwork: controller.current?.artworkData
            )
            .ignoresSafeArea()
        }
        .environment(\.colorScheme, .dark)
    }
}

/// Lyrics in the inspector, on the same clock the player uses.
///
/// `TimelineView` rather than a timer: the pane reads the position on each tick
/// and never owns a clock of its own, exactly as the player's lyrics do.
private struct MacLyricsInspector: View {
    @Environment(PlaybackController.self) private var controller
    @State private var offsetMs = LyricsOffsetBridge.offsetMs()
    @State private var showProviderPicker = false

    var body: some View {
        TimelineView(.periodic(from: .now, by: 0.25)) { timeline in
            LyricsPane(
                lines: controller.displayedLyrics,
                originalLines: controller.lyrics,
                loading: controller.lyricsLoading,
                position: controller.livePosition(at: timeline.date),
                hasTrack: controller.current != nil,
                sourceLabel: controller.lyricsSourceLabel,
                alignmentInProgress: controller.lyricsAligning,
                alignedLocally: controller.lyricsAlignedLocally,
                onChooseSource: { showProviderPicker = true },
                onSeek: { controller.seek(to: $0) },
                translator: controller.lyricsTranslator,
                trackId: controller.current?.id ?? "",
                offsetMs: offsetMs,
                trackTitle: controller.current?.title ?? "",
                trackArtist: controller.current?.artist ?? "",
                artworkData: controller.current?.artworkData,
                artworkUrl: controller.current?.thumbnailUrl
            )
        }
        .onReceive(NotificationCenter.default.publisher(for: .lyricsOffsetChanged)) { _ in
            offsetMs = LyricsOffsetBridge.offsetMs()
        }
        .sheet(isPresented: $showProviderPicker) {
            LyricsProviderSheet()
        }
    }
}

#endif
