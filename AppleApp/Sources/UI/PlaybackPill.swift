import SwiftUI
import BitChordShared
#if canImport(UIKit)
import UIKit
#endif

/// The Apple Music-style playback pill (UI spec §3.1/§3.2), shared by both
/// platforms. Ports upstream's `MiniPlayer.kt`: artwork thumbnail (tap →
/// Now Playing), title/artist, transport. The pill is **always visible** —
/// with nothing playing it shows the empty state, exactly as Music keeps its
/// bottom pill around. The Mac does not use it: its transport lives in the
/// window toolbar (`MacTransportControls`), so this view is iOS-only.
#if os(iOS)
struct PlaybackPill: View {
    @Environment(PlaybackController.self) private var controller
    @Environment(AppModel.self) private var appModel
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    var body: some View {
        iOSPill
    }

    // ---- iOS: mini-player above the tab bar --------------------------------
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    private var isRegularWidth: Bool { horizontalSizeClass == .regular }

    private var iOSPill: some View {
        HStack(spacing: 12) {
            if isRegularWidth {
                padLeadingTransport
            }

            Button {
                appModel.nowPlayingPresented = true
            } label: {
                HStack(spacing: 10) {
                    artworkOrPlaceholder
                    info
                    Spacer(minLength: 4)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(controller.current?.title ?? "Not Playing"), \(controller.current?.artist ?? "")")
            .accessibilityHint("Opens Now Playing")

            if isRegularWidth {
                padTrailingControls
            } else {
                buttons
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .modifier(MiniPlayerChrome(fill: chromeFill))
        .overlay(alignment: .bottom) { progressHairline.clipShape(Capsule()) }
        .contentShape(.rect)
        .modifier(NowPlayingZoomSource())
        // Horizontal swipe to skip, matching upstream MiniPlayer's 72dp fling:
        // left for next, right for previous.
        // `minimumDistance: 20` keeps ordinary taps on the tap-to-expand path.
        .gesture(
            DragGesture(minimumDistance: 20)
                .onEnded { value in
                    let dx = value.translation.width
                    guard abs(dx) >= 72 else { return }
                    guard !PartyStore.shared.state.controlsLocked else {
                        Haptics.play(.tap)
                        return
                    }
                    if dx < 0 {
                        guard controller.canPlayNext else { return }
                        Haptics.play(.skipNext)
                        controller.next()
                    } else {
                        guard controller.canPlayPrevious else { return }
                        Haptics.play(.skipPrevious)
                        controller.previous()
                    }
                }
        )
    }
    #endif

    // ---- macOS: Music's floating glass pill --------------------------------

    // ---- Shared pieces ------------------------------------------------------

    @ViewBuilder
    private var artworkOrPlaceholder: some View {
        if controller.current != nil {
            ArtworkView(entry: controller.current, side: 40)
                .clipShape(.rect(cornerRadius: 8, style: .continuous))
                .overlay {
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .stroke(.white.opacity(0.10), lineWidth: 0.5)
                }
                .id(controller.current?.id)
        } else {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(.quaternary.opacity(0.6))
                .frame(width: 40, height: 40)
                .overlay {
                        Image(.bchMusicNote)
                            .resizable()
                            .scaledToFit()
                            .frame(width: 17)
                            .foregroundStyle(.secondary)
                }
        }
    }

    private var info: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(controller.current?.title ?? "Not Playing")
                .font(.callout.weight(.semibold))
                .foregroundStyle(controller.current == nil ? .secondary : .primary)
                .lineLimit(1)
            Text(controller.current?.artist ?? "Music you start appears here")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .lineLimit(1)
        }
    }

    // MARK: - The party

    /**
     * Who is listening, and the way in when nobody is.
     *
     * A person glyph and nothing else, on the same reasoning as the rest of this
     * pill: the pill is a row of transport glyphs, and anything that is not transport
     * belongs in the player. But the *count* does belong here, because this is where
     * a listener looks to find out whether anybody else is listening at all, and the
     * alternative — opening the player to discover it — is a step for a fact.
     *
     * Highlighted when in a party, so the pill says "you are not playing this on your
     * own" at a glance. Upstream does the same, and for the same reason.
     */
    private var partyButton: some View {
        let party = PartyStore.shared
        return Button {
            if party.inParty {
                // Who is here, rather than the screen that manages it: there is
                // nothing to create or join once there is a party, and a listener who
                // is already playing wants to know who else is here rather than go
                // and manage anything.
                appModel.partyMembersPresented = true
            } else {
                appModel.listenTogetherPresented = true
            }
        } label: {
            ZStack(alignment: .topTrailing) {
                Image(systemName: "person.2.fill")
                    .font(.system(size: 15, weight: .semibold))
                    .frame(width: 32, height: 32)
                    .contentShape(.rect)
                if party.inParty, party.state.members.count > 1 {
                    Text("\(party.state.members.count)")
                        .font(.system(size: 9, weight: .bold))
                        .monospacedDigit()
                        .foregroundStyle(.white)
                        .padding(.horizontal, 3)
                        .background(Color.accentColor, in: Capsule())
                        .offset(x: 4, y: -2)
                }
            }
        }
        .buttonStyle(.plain)
        .foregroundStyle(party.inParty ? Color.accentColor : .secondary)
        .help(partyHelp(party))
        .accessibilityLabel(partyHelp(party))
    }

    /// Spoken, the count is the whole point of the control; drawn, it would cost the
    /// pill its symmetry for something the badge already says.
    private func partyHelp(_ party: PartyStore) -> String {
        guard party.inParty else { return "Listen together" }
        let count = party.state.members.count
        return count > 1 ? "Listening together · \(count)" : "Listening together"
    }

    /// Track actions open as a native menu anchored to this button. It stays tied
    /// to the mini player instead of becoming a detached player-sized overlay.
    @ViewBuilder
    private var playerActionsButton: some View {
        #if os(macOS)
        Menu { playerActionItems } label: { playerActionLabel }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .disabled(controller.current == nil)
        #else
        Menu { playerActionItems } label: { playerActionLabel }
            .disabled(controller.current == nil)
        #endif
    }

    @ViewBuilder
    private var playerActionItems: some View {
        if let current = controller.current {
            SongActionButtons(entry: current, showSleepTimer: false)
        }
    }

    private var playerActionLabel: some View {
        Image(systemName: "ellipsis")
            .font(.system(size: 15, weight: .semibold))
            .frame(width: 32, height: 32)
            .contentShape(Circle())
            .foregroundStyle(.secondary)
    }

    private var queueButton: some View {
        Button {
            PlatformSettings.shared.putString(key: "last_player_screen", value: "QUEUE")
            appModel.nowPlayingPresented = true
        } label: {
            Image(.bchQueue)
                .resizable()
                .scaledToFit()
                .frame(width: 16, height: 16)
                .frame(width: 32, height: 32)
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(.secondary)
        .help("Queue")
        .accessibilityLabel("Queue")
    }

    private var reduceBlur: Bool {
        PlatformSettings.shared.getBoolean(key: "reduce_dynamic_blur", default: false)
    }

    /// The pill's fill, honouring both the app's own preference and the system's.
    ///
    /// Two settings, and the difference matters: `reduce_dynamic_blur` is a
    /// preference *this app* offers and someone can turn it off, while Reduce
    /// Transparency is a system accessibility setting that applies to every
    /// translucent surface on the device. Reading only the first — which is what
    /// this did — left the pill as the one surface in the app that stayed
    /// translucent for someone who had asked the system, everywhere, for it not to
    /// be.
    private var chromeFill: AnyShapeStyle {
        if reduceTransparency || reduceBlur {
            return AnyShapeStyle(Color.primary.opacity(0.12))
        }
        return AnyShapeStyle(.ultraThinMaterial)
    }

    /// Progress hairline hugging the iOS mini-player's bottom edge.
    private var progressHairline: some View {
        GeometryReader { geo in
            let progress = controller.duration > 0
                ? controller.position / controller.duration
                : 0
            Capsule()
                .fill(.tertiary)
                .frame(width: geo.size.width * progress, height: 2)
                .frame(maxHeight: .infinity, alignment: .bottom)
        }
        .allowsHitTesting(false)
    }

    #if os(iOS)
    /// The mini-player's capsule shell — drawn by us everywhere except inside
    /// iOS 27's bottom accessory on iPhone, whose container draws its own
    /// pill. Keeping ours there nests a second capsule inside the system's
    /// (verified by stripping ours in the simulator: the pill still renders
    /// fully). iPad mounts via inset with no system shell, so it keeps ours,
    /// as do iOS 26 and the pre-26 fallback.
    private struct MiniPlayerChrome: ViewModifier {
        var fill: AnyShapeStyle

        func body(content: Content) -> some View {
            if #available(iOS 27.0, *),
               UIDevice.current.userInterfaceIdiom == .phone {
                content
            } else {
                content
                    .background { Capsule().fill(fill) }
                    .overlay { Capsule().stroke(.white.opacity(0.10), lineWidth: 0.5) }
                    .clipShape(Capsule())
            }
        }
    }
    #endif

    #if os(iOS)
    private var buttons: some View {
        HStack(spacing: 8) {
            Button {
                Haptics.play(controller.isPlaying ? .pause : .resume)
                controller.togglePlayPause()
            } label: {
                if controller.isBuffering {
                    ProgressView().controlSize(.small).frame(width: 22, height: 22)
                } else {
                    Image(systemName: controller.isPlaying ? "pause.fill" : "play.fill")
                        .font(.system(size: 20, weight: .semibold))
                }
            }
            .buttonStyle(.plain)
            .frame(width: 40, height: 40)
            .disabled(controller.current == nil && !controller.isBuffering)

            Button {
                Haptics.play(.skipNext)
                controller.next()
            } label: {
                Image(systemName: "forward.fill")
                    .font(.system(size: 19, weight: .semibold))
            }
            .buttonStyle(.plain)
            .frame(width: 40, height: 40)
            .foregroundStyle(
                PartyStore.shared.state.controlsLocked ? Color.primary.opacity(0.3) : Color.primary
            )
            .disabled(!controller.canPlayNext || PartyStore.shared.state.controlsLocked)

            partyButton.frame(width: 40, height: 40)
        }
        .foregroundStyle(.primary)
    }

    private var padLeadingTransport: some View {
        HStack(spacing: 4) {
            Button {
                guard controller.canPlayPrevious else { return }
                Haptics.play(.skipPrevious)
                controller.previous()
            } label: {
                Image(systemName: "backward.fill")
                    .font(.system(size: 15, weight: .semibold))
                    .frame(width: 34, height: 34)
                    .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .disabled(!controller.canPlayPrevious)

            if controller.isBuffering {
                ProgressView()
                    .controlSize(.small)
                    .frame(width: 34, height: 34)
            } else {
                Button {
                    Haptics.play(controller.isPlaying ? .pause : .resume)
                    controller.togglePlayPause()
                } label: {
                    Image(systemName: controller.isPlaying ? "pause.fill" : "play.fill")
                        .font(.system(size: 18, weight: .semibold))
                        .frame(width: 34, height: 34)
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .disabled(controller.current == nil && !controller.isBuffering)
            }

            Button {
                guard controller.canPlayNext else { return }
                Haptics.play(.skipNext)
                controller.next()
            } label: {
                Image(systemName: "forward.fill")
                    .font(.system(size: 15, weight: .semibold))
                    .frame(width: 34, height: 34)
                    .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .foregroundStyle(
                PartyStore.shared.state.controlsLocked ? Color.primary.opacity(0.3) : Color.primary
            )
            .disabled(!controller.canPlayNext || PartyStore.shared.state.controlsLocked)
        }
        .foregroundStyle(.primary)
    }

    private var padTrailingControls: some View {
        HStack(spacing: 6) {
            playerActionsButton

            Button {
                appModel.nowPlayingPresented = true
                PlatformSettings.shared.putString(key: "last_player_screen", value: "LYRICS")
            } label: {
                Image(.bchLyrics)
                    .resizable()
                    .scaledToFit()
                    .frame(width: 15, height: 15)
                    .frame(width: 32, height: 32)
                    .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .accessibilityLabel("Lyrics")

            queueButton

            AirPlayRouteButton()
                .frame(width: 22, height: 22)
                .frame(width: 32, height: 32)
                .accessibilityLabel("AirPlay")

            partyButton.frame(width: 32, height: 32)
        }
    }
    #endif
}
#endif

/// Slim volume slider — hairline capsule track with a small knob, in the
/// spirit of upstream's `ThinSlider`. Adapts to the surrounding foreground
/// style (dark Now Playing backdrop vs. light pill).
struct PillSlider: View {
    @Binding var volume: Double

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: volume == 0 ? "speaker.slash.fill" : "speaker.fill")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(.secondary)
                .frame(width: 16, height: 16)
            GeometryReader { geo in
                let w = geo.size.width
                let x = w * volume
                ZStack(alignment: .leading) {
                    Capsule()
                        .fill(.quaternary)
                        .frame(height: 4)
                    Capsule()
                        .fill(.primary.opacity(0.55))
                        .frame(width: max(4, x), height: 4)
                    Circle()
                        .fill(.primary)
                        .frame(width: 10, height: 10)
                        .offset(x: min(max(0, x - 5), w - 10))
                        .shadow(color: .black.opacity(0.25), radius: 2, y: 1)
                }
                .frame(height: geo.size.height)
                .contentShape(.rect)
                .gesture(
                    DragGesture(minimumDistance: 0)
                        .onChanged { g in
                            volume = min(max(0, g.location.x / w), 1)
                        }
                )
            }
            .frame(height: 14)
        }
        .accessibilityElement()
        .accessibilityLabel("Volume")
        .accessibilityValue("\(Int(volume * 100)) percent")
        .accessibilityAdjustableAction { direction in
            switch direction {
            case .increment: volume = min(1, volume + 0.1)
            case .decrement: volume = max(0, volume - 0.1)
            default: break
            }
        }
    }
}

#if os(iOS)
/// WWDC 323: the mini player is the zoom source for the Now Playing sheet.
private struct NowPlayingZoomSource: ViewModifier {
    @Environment(\.nowPlayingZoomNamespace) private var zoomNamespace

    func body(content: Content) -> some View {
        if let zoomNamespace {
            content.matchedTransitionSource(id: NowPlayingZoom.sourceID, in: zoomNamespace)
        } else {
            content
        }
    }
}
#endif
