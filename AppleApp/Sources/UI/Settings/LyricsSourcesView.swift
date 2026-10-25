import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
#if os(iOS)
import UIKit
#else
import AppKit
#endif

struct LyricsSourcesView: View {
    @Binding var selection: String
    @Binding var order: String
    @Binding var syllableSync: Bool
    /// The key for the two authenticated PaxSeniX routes.
    ///
    /// Held here rather than in the ordinary settings because it is a bearer
    /// credential, and this is the only screen where it belongs: a source
    /// configured from a list of names, not a row anybody types into.
    @State private var paxSenixKey = PlatformSettings.shared.getSecret(key: "paxsenix_api_key") ?? ""
    /// iOS-only: `EditMode` does not exist on macOS, where the list is reordered
    /// by click-to-move instead.
    #if os(iOS)
    @State private var editMode: EditMode = .inactive
    #endif

    var body: some View {
        List {
            Section {
                ForEach(orderedIds, id: \.self) { id in
                    if let source = LyricsSourceOption.all.first(where: { $0.name == id }) {
                        Toggle(isOn: enabledBinding(source.id)) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(source.label)
                                Text(source.detail)
                                    .font(.subheadline)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
                .onMove(perform: move)
            } footer: {
                Text("Tried in this order — drag to reorder. The highest-priority source to answer at all wins, unless Prefer Word-Synced Lyrics says to keep looking for a word-synced one.")
            }
            if usesAuthenticatedRoutes {
                Section {
                    SecureField("API key", text: $paxSenixKey)
                        #if os(iOS)
                        .textContentType(.password)
                        #endif
                    if !paxSenixKey.isEmpty {
                        Button("Remove Key", role: .destructive) {
                            paxSenixKey = ""
                            AppSettings.shared.setPaxSenixApiKey(value: "")
                        }
                    }
                } header: {
                    Text("PaxSeniX Key")
                } footer: {
                    Text("PaxSeniX Spotify and PaxSeniX Musixmatch reach two catalogues Apple Music does not carry, and both need a key from paxsenix.org. Everything else on this list works without one. Stored in the Keychain, not in preferences.")
                }
            }
            Section {
                Button("Reset to Default") {
                    AppSettings.shared.resetLyricsSourceSettings()
                    order = LyricsSourceNames.defaultEnabled
                    selection = LyricsSourceNames.defaultEnabled
                    syllableSync = false
                }
            }
        }
        .navigationTitle("Lyrics Sources")
        #if os(iOS)
        // Reordering the provider list needs edit mode, but forcing it active
        // left the user with no way out of it. An explicit control is what
        // Music does for exactly this.
        .environment(\.editMode, $editMode)
        .toolbar {
            ToolbarItem(placement: .automatic) {
                Button(editMode == .active ? "Done" : "Reorder") {
                    withAnimation { editMode = editMode == .active ? .inactive : .active }
                }
            }
        }
        #endif
        .onAppear {
            order = LyricsSourceNames.normalizeList(order)
            selection = LyricsSourceNames.normalizeList(selection)
        }
        .onChange(of: paxSenixKey) { _, value in
            AppSettings.shared.setPaxSenixApiKey(value: value)
        }
    }

    private var orderedIds: [String] {
        let saved = LyricsSourceNames.normalizeList(order).split(separator: ",").map(String.init)
        let known = LyricsSourceOption.all.map(\.name)
        let fromSaved = saved.filter { known.contains($0) }
        return fromSaved + known.filter { !fromSaved.contains($0) }
    }

    /// Whether a key is worth asking about at all.
    ///
    /// Asked rather than always shown: with neither authenticated source enabled
    /// the field is a credential box for a host this app is not contacting, and a
    /// row that does nothing is worse than no row.
    private var usesAuthenticatedRoutes: Bool {
        let enabled = Set(LyricsSourceNames.normalizeList(selection)
            .split(separator: ",").map(String.init))
        return enabled.contains("PAXSENIX_SPOTIFY") || enabled.contains("PAXSENIX_MUSIXMATCH")
    }

    private func move(from source: IndexSet, to dest: Int) {
        var ids = orderedIds
        ids.move(fromOffsets: source, toOffset: dest)
        order = ids.joined(separator: ",")
        let enabled = Set(selection.split(separator: ",").map(String.init))
        selection = ids.filter { enabled.contains($0) }.joined(separator: ",")
    }

    private func enabledBinding(_ id: String) -> Binding<Bool> {
        Binding(
            get: { Set(selection.split(separator: ",").map(String.init)).contains(id) },
            set: { on in
                let ids = orderedIds
                var enabled = Set(selection.split(separator: ",").map(String.init))
                if on {
                    enabled.insert(id)
                } else if enabled.count > 1 {
                    enabled.remove(id)
                }
                selection = ids.filter { enabled.contains($0) }.joined(separator: ",")
            }
        )
    }
}

/// One lyrics source, as the shared module describes it.
///
/// The list, the labels, the subtitles and the order all come from
/// `AppSettings.lyricsSourceCatalogJson()` rather than a copy held here. This
/// screen used to keep its own, and the three sources added along with the ISRC
/// pass did not appear in it — so the feature was on by default and had no row
/// anywhere to be found, switched off, or read about.
struct LyricsSourceOption: Identifiable, Decodable {
    let name: String
    let label: String
    let detail: String
    let wordSynced: Bool
    let enabled: Bool

    var id: String { name }

    enum CodingKeys: String, CodingKey {
        case name, label, detail, wordSynced, enabled
    }

    /// Every source, in the order the shared module puts them.
    static var all: [LyricsSourceOption] {
        let json = AppSettings.shared.lyricsSourceCatalogJson()
        guard let data = json.data(using: .utf8),
              let decoded = try? JSONDecoder().decode([LyricsSourceOption].self, from: data)
        else {
            // An empty list rather than a hard-coded fallback: a fallback here
            // would be the stale copy all over again, and the symptom would be a
            // settings screen that looks fine and controls nothing.
            return []
        }
        return decoded
    }

    static func summary(_ raw: String) -> String {
        let enabled = Set(LyricsSourceNames.normalizeList(raw).split(separator: ",").map(String.init))
        let names = all.filter { enabled.contains($0.name) }.map(\.label)
        if names.isEmpty { return "None — no lyrics will be fetched" }
        return names.joined(separator: ", ")
    }
}

struct TranslationPreferenceView: View {
    @Binding var selection: String
    let translator: LyricsTranslator
    @State private var search = ""
    var body: some View {
        List {
            Button { selection = "" } label: {
                HStack { Text("Follow the App Language"); Spacer(); if selection.isEmpty { Image(systemName: "checkmark") } }
            }
            ForEach(translator.languages(for: .translate).filter { search.isEmpty || $0.name.localizedCaseInsensitiveContains(search) }) { language in
                Button { selection = language.code } label: {
                    HStack { Text(language.name); Spacer(); if selection == language.code { Image(systemName: "checkmark") } }
                }
            }
        }
        .foregroundStyle(.primary)
        .navigationTitle("Translation Language")
        .searchable(text: $search)
        .onAppear { translator.loadLanguages() }
    }
}
