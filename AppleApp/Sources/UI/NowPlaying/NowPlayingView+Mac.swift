import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
import AVKit
import CoreImage
import CoreImage.CIFilterBuiltins
#if os(iOS)
import UIKit
import AVFAudio
#else
import AppKit
#endif

// The Mac player: the window body, its toolbar, and the edge balance
// between Back and AirPlay.
extension NowPlayingView {
    // ---- macOS: window-root player -----------------------------------------
    #if os(macOS)
    var macOSBody: some View {
        // The same arrangement decision iOS makes, and for the same reason: the
        // two shapes are one set of slots arranged differently, so a Mac window
        // that is not wide enough gets the portrait player rather than a
        // two-column layout whose columns cannot hold what they are given.
        GeometryReader { geo in
            ZStack {
                MeshBackdrop(seed: controller.current?.id.hashValue ?? 0, artwork: controller.current?.artworkData)
                    .ignoresSafeArea()
                    .allowsHitTesting(false)
                if usesBlurredArtworkBackdrop(width: geo.size.width, height: geo.size.height) {
                    FullArtworkBlurBackdrop(entry: controller.current)
                        .ignoresSafeArea()
                        .allowsHitTesting(false)
                        .transition(.opacity)
                }

                if PlayerLayout.takesLandscapeShape(
                    width: geo.size.width, height: geo.size.height
                ) {
                    twoColumnPlayer(size: geo.size)
                } else {
                    portraitPlayer
                }
            }
        }
        .toolbar { macPlayerToolbar }
        .toolbar(removing: .title)
        .toolbarBackground(.hidden, for: .windowToolbar)
        .toolbarBackgroundVisibility(.hidden, for: .windowToolbar)
        .navigationTitle("")
        .environment(\.colorScheme, .dark)
    }

    @ToolbarContentBuilder
    var macPlayerToolbar: some ToolbarContent {
        // Back by the traffic lights, and full screen beside it as its own
        // button; volume and AirPlay at the far end. The player spans the
        // whole window's toolbar (RootView collapses the sidebar while it is
        // up), so the flexible space reaches the trailing edge.
        if #available(macOS 26.0, *) {
            ToolbarItem(placement: .navigation) {
                macBackButton
                    .glassEffect(.regular.interactive(), in: Circle())
                    .background(WindowLeadingReader { closeLeading in
                        trailingInset = max(0, closeLeading - Self.systemTitlebarMargin)
                    })
            }
            .sharedBackgroundVisibility(.hidden)
            ToolbarItem(placement: .navigation) {
                MacFullScreenButton()
                    .buttonStyle(.plain)
                    .frame(width: 32, height: 32)
                    .glassEffect(.regular.interactive(), in: Circle())
            }
            .sharedBackgroundVisibility(.hidden)
            ToolbarSpacer(.flexible, placement: .primaryAction)
        } else {
            ToolbarItem(placement: .navigation) { macBackButton }
            ToolbarItem(placement: .navigation) { MacFullScreenButton() }
        }

        if !controller.hideVolumeBar {
            ToolbarItem(placement: .primaryAction) {
                ToolbarVolumeSlider(volume: Binding(
                    get: { controller.volume },
                    set: { controller.volume = $0 }
                ))
                .help("Volume")
            }
        }

        if #available(macOS 26.0, *) {
            ToolbarSpacer(.fixed, placement: .primaryAction)
            ToolbarItem(placement: .primaryAction) {
                AirPlayRouteButton()
                    .frame(width: 22, height: 22)
                    .frame(width: 32, height: 32)
                    .glassEffect(.regular, in: Circle())
                    .help("AirPlay")
            }
            .sharedBackgroundVisibility(.hidden)
            // As far from the right edge as Back is from the left.
            ToolbarItem(placement: .primaryAction) {
                Color.clear.frame(width: trailingInset, height: 1)
            }
            .sharedBackgroundVisibility(.hidden)
        } else {
            ToolbarItem(placement: .primaryAction) {
                AirPlayRouteButton()
                    .frame(width: 22, height: 22)
                    .frame(width: 32, height: 32)
                    .background(.ultraThinMaterial, in: Circle())
                    .help("AirPlay")
            }
        }
    }

    /// The window's own titlebar margin, read from the system: the red
    /// traffic light's inset from the left edge, which the toolbar also keeps
    /// after its last item. Full screen hides the traffic lights, so the value
    /// read in a window is kept for it.
    @MainActor private static var cachedTitlebarMargin: CGFloat?

    @MainActor private static var systemTitlebarMargin: CGFloat {
        if let button = MacFullScreenButton.playerWindow?.standardWindowButton(.closeButton),
           !button.isHidden, button.window?.styleMask.contains(.fullScreen) == false {
            let margin = button.convert(button.bounds, to: nil).minX
            cachedTitlebarMargin = margin
            return margin
        }
        return cachedTitlebarMargin ?? 0
    }

    private var macBackButton: some View {
        Button {
            appModel.nowPlayingPresented = false
        } label: {
            Image(systemName: "chevron.left")
                // Size the label so the whole visible button receives clicks.
                .frame(width: 32, height: 32)
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .keyboardShortcut(.cancelAction)
        .labelStyle(.iconOnly)
        .help("Back to Library")
        .accessibilityLabel("Back to Library")
    }

    #endif
}
