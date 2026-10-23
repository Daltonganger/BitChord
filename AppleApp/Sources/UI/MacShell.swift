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

// MARK: - Transport

/// The transport in the window toolbar, at the leading edge where Music puts
/// it: shuffle, previous, play/pause, next and repeat.
///
/// This replaces the floating pill on the Mac. The pill was an iPad shape
/// hovering over the content; a Mac player is a row of titlebar controls with a
/// readout in the middle, and that is what this and `MacNowPlayingLCD` are.
struct MacTransportControls: View {
    @Environment(PlaybackController.self) private var controller

    var body: some View {
        HStack(spacing: 2) {
            Button {
                controller.toggleShuffle()
            } label: {
                Image(systemName: "shuffle")
                    .foregroundStyle(controller.shuffleEnabled ? Color.accentColor : .primary)
            }
            .help(controller.shuffleEnabled ? "Turn Shuffle Off" : "Shuffle")
            .accessibilityLabel("Shuffle")
            .accessibilityAddTraits(controller.shuffleEnabled ? .isSelected : [])

            Button {
                controller.previous()
            } label: {
                Image(systemName: "backward.fill")
            }
            .disabled(!controller.canPlayPrevious)
            .help("Previous")
            .accessibilityLabel("Previous")

            if controller.isBuffering {
                ProgressView()
                    .controlSize(.small)
                    .frame(width: 26, height: 22)
                    .help("Loading")
            } else {
                Button {
                    controller.togglePlayPause()
                } label: {
                    Image(systemName: controller.isPlaying ? "pause.fill" : "play.fill")
                }
                .disabled(controller.current == nil)
                .help(controller.isPlaying ? "Pause" : "Play")
                .accessibilityLabel(controller.isPlaying ? "Pause" : "Play")
            }

            Button {
                controller.next()
            } label: {
                Image(systemName: "forward.fill")
            }
            .disabled(!controller.canPlayNext)
            .help("Next")
            .accessibilityLabel("Next")

            Button {
                controller.cycleRepeat()
            } label: {
                RepeatGlyph(mode: controller.repeatMode, size: 15)
                    .foregroundStyle(controller.repeatMode == .off ? Color.primary : .accentColor)
            }
            .help(repeatHelp)
            .accessibilityLabel(repeatHelp)

            MacPartyButton()
        }
        .labelStyle(.iconOnly)
    }

    private var repeatHelp: String {
        switch controller.repeatMode {
        case .off: "Repeat Off"
        case .all: "Repeat All"
        case .one: "Repeat One"
        }
    }
}

/// Who is listening, and the way in when nobody is — the pill's party control,
/// now a toolbar item. The badge is dropped here: the titlebar has no room for
/// it, and the tooltip says the count.
private struct MacPartyButton: View {
    @Environment(AppModel.self) private var appModel
    @State private var party = PartyStore.shared

    private var help: String {
        guard party.inParty else { return "Listen Together" }
        let count = party.state.members.count
        return count > 1 ? "Listening Together · \(count)" : "Listening Together"
    }

    var body: some View {
        Button {
            if party.inParty {
                appModel.partyMembersPresented = true
            } else {
                appModel.listenTogetherPresented = true
            }
        } label: {
            Image(systemName: "person.2.fill")
                .foregroundStyle(party.inParty ? Color.accentColor : .primary)
        }
        .help(help)
        .accessibilityLabel(help)
    }
}

/// The centered readout: sleeve, title, artist and the scrubber.
///
/// Clicking the sleeve opens the full player, which stays the one place for the
/// big artwork; the LCD is the always-there summary of what is playing.
struct MacNowPlayingLCD: View {
    @Environment(PlaybackController.self) private var controller
    @Environment(AppModel.self) private var appModel
    /// A drag in progress. The slider follows the pointer, and playback only
    /// hears about it when the drag ends — seeking on every frame would fight
    /// the clock it is meant to set.
    @State private var scrub: Double?

    var body: some View {
        HStack(spacing: 10) {
            Button {
                appModel.nowPlayingPresented = true
            } label: {
                ArtworkView(entry: controller.current, side: 28)
                    .clipShape(.rect(cornerRadius: 6, style: .continuous))
            }
            .buttonStyle(.plain)
            .help("Open the Player")
            .accessibilityLabel("Open the Player")

            Button {
                appModel.nowPlayingPresented = true
            } label: {
                VStack(alignment: .leading, spacing: 0) {
                    Text(controller.current?.title ?? "Not Playing")
                        .font(.callout.weight(.semibold))
                        .lineLimit(1)
                    Text(controller.current?.artist ?? "")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                .frame(width: 128, alignment: .leading)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .disabled(controller.current == nil)
            .help("Open the Player")

            VStack(spacing: 2) {
                Slider(value: scrubBinding, in: 0...max(controller.duration, 1)) { editing in
                    guard !editing else { return }
                    if let value = scrub { controller.seek(to: value) }
                    scrub = nil
                }
                .controlSize(.mini)
                .disabled(controller.duration <= 0)
                .help("Scrub")

                HStack(spacing: 4) {
                    Text(clock(scrub ?? controller.position))
                    Spacer(minLength: 4)
                    Text(clock(max(controller.duration - (scrub ?? controller.position), 0)))
                }
                .font(.caption2.monospacedDigit())
                .foregroundStyle(.secondary)
                .lineLimit(1)
            }
            .frame(width: 124)
        }
        .frame(width: 306)
        .contextMenu {
            if let current = controller.current {
                SongActionButtons(entry: current, showSleepTimer: false)
            }
        }
    }

    private var scrubBinding: Binding<Double> {
        Binding(
            get: { scrub ?? min(max(controller.position, 0), max(controller.duration, 0)) },
            set: { scrub = $0 }
        )
    }

    private func clock(_ seconds: Double) -> String {
        guard seconds.isFinite, seconds >= 0 else { return "0:00" }
        let total = Int(seconds.rounded(.down))
        return "\(total / 60):\(String(format: "%02d", total % 60))"
    }
}

// MARK: - Trailing controls

/// Lyrics and queue: two toggles onto the same inspector, the way Music's
/// titlebar has one button per panel.
struct MacInspectorToggles: View {
    @Binding var selection: MacInspectorKind?

    var body: some View {
        HStack(spacing: 2) {
            toggle(.lyrics, icon: .bchLyrics, label: "Lyrics")
            toggle(.queue, icon: .bchQueue, label: "Queue")
        }
    }

    private func toggle(_ kind: MacInspectorKind, icon: ImageResource, label: String) -> some View {
        let active = selection == kind
        return Button {
            selection = active ? nil : kind
        } label: {
            Image(icon)
                .resizable()
                .scaledToFit()
                .frame(width: 15, height: 15)
                .frame(width: 24, height: 20)
                .foregroundStyle(active ? Color.accentColor : .primary)
        }
        .help(active ? "Hide \(label)" : "Show \(label)")
        .accessibilityLabel(label)
        .accessibilityAddTraits(active ? .isSelected : [])
    }
}

/// The titlebar's volume slider — the pill's hairline, sized for the toolbar.
struct MacVolumeControl: View {
    @Environment(PlaybackController.self) private var controller

    var body: some View {
        PillSlider(volume: Binding(
            get: { controller.volume },
            set: { controller.volume = $0 }
        ))
        .frame(width: 92)
        .help("Volume")
        .accessibilityLabel("Volume")
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

    var body: some View {
        TimelineView(.periodic(from: .now, by: 0.25)) { timeline in
            LyricsPane(
                lines: controller.displayedLyrics,
                loading: controller.lyricsLoading,
                position: controller.livePosition(at: timeline.date),
                hasTrack: controller.current != nil,
                sourceLabel: controller.lyricsSourceLabel,
                alignmentInProgress: controller.lyricsAligning,
                onSeek: { controller.seek(to: $0) },
                translator: controller.lyricsTranslator,
                trackId: controller.current?.id ?? "",
                offsetMs: offsetMs
            )
        }
        .onReceive(NotificationCenter.default.publisher(for: .lyricsOffsetChanged)) { _ in
            offsetMs = LyricsOffsetBridge.offsetMs()
        }
    }
}

#endif
