import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
#if os(iOS)
import UIKit
#else
import AppKit
#endif

struct QualityPickerPage: View {
    let title: String
    @Binding var selection: String
    let options: [AudioQualityOption]

    var body: some View {
        List {
            ForEach(options) { option in
                Button {
                    selection = option.id
                } label: {
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(option.title)
                                .foregroundStyle(.primary)
                            Text(option.detail)
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        if selection == option.id {
                            Image(systemName: "checkmark")
                                .fontWeight(.semibold)
                                .foregroundStyle(Color.accentColor)
                        }
                    }
                }
            }
        }
        .navigationTitle(title)
    }
}

struct AutomixPerformancePage: View {
    @Binding var selection: String

    private let modes = [
        ("EFFICIENT", "Efficient", "1 analysis thread · kindest to battery"),
        ("BALANCED", "Balanced", "2 threads · the default"),
        ("PERFORMANCE", "Performance", "4 threads · fastest transitions, warmest phone"),
    ]

    var body: some View {
        List {
            ForEach(modes, id: \.0) { mode in
                Button {
                    selection = mode.0
                } label: {
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(mode.1)
                                .foregroundStyle(.primary)
                            Text(mode.2)
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        if selection == mode.0 {
                            Image(systemName: "checkmark")
                                .fontWeight(.semibold)
                                .foregroundStyle(Color.accentColor)
                        }
                    }
                }
            }
        }
        .navigationTitle("Automix Performance")
    }
}

struct SpotifyCanvasAuthView: View {
    @Binding var cookie: String

    var body: some View {
        Form {
            Section {
                SecureField("sp_dc cookie", text: $cookie)
                    .textContentType(.password)
            } header: {
                Text("Spotify Canvas Setup")
            } footer: {
                Text("Paste the sp_dc cookie from an open Spotify web session. BitChord uses it only to look up looping motion art. Leave blank to skip.")
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Spotify Canvas")
    }
}

// MARK: - Rows

struct AudioQualityOption: Identifiable {
    let id: String
    let title: String
    let detail: String

    static let stream: [AudioQualityOption] = [
        .init(id: "LOW", title: "Low", detail: "64 kbps · uses the least data"),
        .init(id: "MEDIUM", title: "Medium", detail: "Compressed audio · balances data use and quality"),
        .init(id: "HIGH", title: "High", detail: "High-quality audio from enabled sources · YouTube fallback"),
        .init(id: "LOSSLESS", title: "Lossless", detail: "Lossless and Hi-Res from sources that supply them"),
    ]

    static let download: [AudioQualityOption] = [
        .init(id: "STANDARD", title: "Standard", detail: "128 kbps AAC"),
        .init(id: "HIGH", title: "High", detail: "Best AAC the source will give"),
        .init(id: "LOSSLESS", title: "Lossless", detail: "Keeps FLAC when a source has it"),
    ]

    static func label(_ id: String, in options: [AudioQualityOption]) -> String {
        options.first { $0.id == id }?.title ?? id.capitalized
    }
}
