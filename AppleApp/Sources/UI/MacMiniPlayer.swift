#if os(macOS)
import SwiftUI
import AppKit

/// The Mac's small players: a floating Mini Player window (⌥⌘M, as in Music)
/// and the same controls in a menu bar extra. Both are the ordinary transport
/// on the shared controller, so they stay in step with the main window, the
/// Now Playing widget and the media keys without any state of their own.
struct MiniPlayerView: View {
    /// The menu bar version is narrower and has no window chrome to sit in.
    var compact = false
    @Environment(PlaybackController.self) private var controller
    @Environment(AppModel.self) private var appModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        VStack(spacing: 12) {
            HStack(spacing: 12) {
                artwork
                VStack(alignment: .leading, spacing: 2) {
                    Text(controller.current?.title ?? "Not Playing")
                        .font(.headline)
                        .lineLimit(1)
                    Text(controller.current?.artist ?? "Pick something to play")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
                if controller.current != nil {
                    Button { controller.toggleLike() } label: {
                        Image(systemName: controller.isLiked ? "heart.fill" : "heart")
                            .foregroundStyle(controller.isLiked ? Color.accentColor : .secondary)
                    }
                    .buttonStyle(.plain)
                    .help(controller.isLiked ? "Remove from Liked" : "Like")
                }
            }
            progress
            HStack(spacing: 28) {
                transportButton("backward.fill", help: "Previous") { controller.previous() }
                transportButton(controller.isPlaying || controller.isBuffering ? "pause.fill" : "play.fill",
                                help: controller.isPlaying ? "Pause" : "Play", size: 24) {
                    controller.togglePlayPause()
                }
                transportButton("forward.fill", help: "Next") { controller.next() }
            }
            .frame(maxWidth: .infinity)
            if compact {
                Divider()
                HStack {
                    Button("Open BitChord") {
                        NSApp.activate()
                        NSApp.windows.first { $0.canBecomeMain }?.makeKeyAndOrderFront(nil)
                    }
                    Spacer()
                    Button("Mini Player") { openWindow(id: MiniPlayerWindow.id) }
                }
                .buttonStyle(.borderless)
                .font(.callout)
            }
        }
        .padding(16)
        .frame(width: compact ? 300 : 340)
    }

    private var artwork: some View {
        Group {
            if controller.current != nil {
                ArtworkView(entry: controller.current, side: 56)
                    .id(controller.current?.id)
            } else {
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .fill(.quaternary)
                    .overlay { Image(systemName: "music.note").foregroundStyle(.secondary) }
            }
        }
        .frame(width: 56, height: 56)
        .clipShape(.rect(cornerRadius: 8, style: .continuous))
        .onTapGesture {
            guard controller.current != nil else { return }
            NSApp.activate()
            appModel.nowPlayingPresented = true
        }
    }

    private var progress: some View {
        TimelineView(.periodic(from: .now, by: 0.5)) { _ in
            let duration = max(controller.duration, 0)
            VStack(spacing: 2) {
                Slider(
                    value: Binding(
                        get: { min(controller.position, duration) },
                        set: { controller.seek(to: $0) }
                    ),
                    in: 0...max(duration, 1)
                )
                .controlSize(.mini)
                .disabled(duration <= 0)
                HStack {
                    Text(Self.clock(controller.position))
                    Spacer()
                    Text("-" + Self.clock(max(0, duration - controller.position)))
                }
                .font(.caption2.monospacedDigit())
                .foregroundStyle(.secondary)
            }
        }
    }

    private func transportButton(_ symbol: String, help: String, size: CGFloat = 17, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: size, weight: .semibold))
                .frame(width: 36, height: 30)
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .disabled(controller.current == nil && controller.queue.isEmpty)
        .help(help)
    }

    private static func clock(_ seconds: Double) -> String {
        guard seconds.isFinite, seconds >= 0 else { return "0:00" }
        let total = Int(seconds)
        return String(format: "%d:%02d", total / 60, total % 60)
    }
}

enum MiniPlayerWindow {
    static let id = "mini-player"
}

/// Window ▸ Mini Player, ⌥⌘M — Music's binding for the same thing.
struct MiniPlayerCommands: Commands {
    @Environment(\.openWindow) private var openWindow

    var body: some Commands {
        CommandGroup(after: .windowArrangement) {
            Button("Mini Player") { openWindow(id: MiniPlayerWindow.id) }
                .keyboardShortcut("m", modifiers: [.command, .option])
        }
    }
}

/// The Dock icon's menu: transport, as every Mac player has it.
final class MacAppDelegate: NSObject, NSApplicationDelegate {
    @MainActor static weak var controller: PlaybackController?

    @MainActor
    func applicationDockMenu(_ sender: NSApplication) -> NSMenu? {
        guard let controller = Self.controller else { return nil }
        let menu = NSMenu()
        if let current = controller.current {
            let title = NSMenuItem(title: "\(current.title) — \(current.artist)", action: nil, keyEquivalent: "")
            title.isEnabled = false
            menu.addItem(title)
            menu.addItem(.separator())
        }
        menu.addItem(DockAction(controller.isPlaying ? "Pause" : "Play") { controller.togglePlayPause() })
        menu.addItem(DockAction("Next") { controller.next() })
        menu.addItem(DockAction("Previous") { controller.previous() })
        return menu
    }
}

/// A menu item that runs a closure, so the Dock menu needs no selector plumbing.
private final class DockAction: NSMenuItem {
    private let run: @MainActor () -> Void

    init(_ title: String, run: @escaping @MainActor () -> Void) {
        self.run = run
        super.init(title: title, action: #selector(fire), keyEquivalent: "")
        target = self
    }

    required init(coder: NSCoder) { fatalError("unused") }

    @objc private func fire() {
        MainActor.assumeIsolated { run() }
    }
}
#endif
