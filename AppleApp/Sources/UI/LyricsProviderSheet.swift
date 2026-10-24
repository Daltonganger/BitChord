import SwiftUI
import BitChordShared

/// Upstream's `LyricsProviderSheet` (LyricsProviderSheet.kt): the per-track
/// provider picker. Every enabled source with its live state — a found source
/// applies instantly from the lines the race already brought back, a missed
/// one stays put because its attempt already failed for this track, and the
/// source in use reads as current.
struct LyricsProviderSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(PlaybackController.self) private var controller

    var body: some View {
        NavigationStack {
            List {
                ForEach(orderedSources) { source in
                    providerRow(source)
                }
            }
            .navigationTitle("Lyrics source")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .safeAreaInset(edge: .bottom) {
                Button {
                    dismiss()
                    controller.refetchLyrics()
                } label: {
                    Label("Search again for this track", systemImage: "arrow.clockwise")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .padding()
            }
        }
        #if os(macOS)
        .frame(minWidth: 440, minHeight: 420)
        #endif
    }

    /// The enabled sources, tried top to bottom — the same order the settings
    /// keep, so the picker reads in the order the search actually ran.
    private var orderedSources: [LyricsSourceOption] {
        let saved = LyricsSourceNames.normalizeList(
            PlatformSettings.shared.getString(key: "lyrics_source_order", default: LyricsSourceNames.defaultEnabled)
        ).split(separator: ",").map(String.init)
        let enabled = Set(LyricsSourceNames.normalizeList(
            PlatformSettings.shared.getString(key: "lyrics_sources", default: LyricsSourceNames.defaultEnabled)
        ).split(separator: ",").map(String.init))
        let known = LyricsSourceOption.all.filter { enabled.contains($0.name) }
        let fromSaved = saved.compactMap { id in known.first { $0.name == id } }
        return fromSaved + known.filter { source in !saved.contains(source.name) }
    }

    @ViewBuilder
    private func providerRow(_ source: LyricsSourceOption) -> some View {
        let state = controller.lyricsProviderStates[source.name] ?? .notFetched
        let isCurrent = controller.lyricsSourceLabel == source.label
            || controller.lyricsProviderChoice == source.name
        let row = HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(source.label)
                    .foregroundStyle(state == .notFound ? .secondary : .primary)
                if isCurrent {
                    Text("In use")
                        .font(.caption)
                        .foregroundStyle(.tint)
                }
            }
            Spacer(minLength: 8)
            stateGlyph(state)
        }
        .contentShape(Rectangle())
        Group {
            if state == .found, !isCurrent {
                Button {
                    controller.selectLyricsProvider(named: source.name)
                    dismiss()
                } label: {
                    row
                }
                .buttonStyle(.plain)
                .accessibilityHint("Switches this track to \(source.label) now")
            } else {
                row
                    .accessibilityElement(children: .combine)
            }
        }
    }

    @ViewBuilder
    private func stateGlyph(_ state: LyricsSourceState) -> some View {
        switch state {
        case .fetching:
            ProgressView().controlSize(.small)
                .accessibilityLabel("Searching")
        case .found:
            Image(systemName: "checkmark.circle.fill")
                .foregroundStyle(.tint)
                .accessibilityLabel("Found")
        case .notFound:
            Image(systemName: "xmark.circle")
                .foregroundStyle(.tertiary)
                .accessibilityLabel("Nothing found")
        case .notFetched:
            Image(systemName: "circle.dotted")
                .foregroundStyle(.tertiary)
                .accessibilityLabel("Waiting")
        }
    }
}
