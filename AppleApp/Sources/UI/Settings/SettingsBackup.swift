import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
#if os(iOS)
import UIKit
#else
import AppKit
#endif

struct SettingsBackupFile: FileDocument {
    static var readableContentTypes: [UTType] { [.json] }
    var text: String
    init(text: String) { self.text = text }
    init(configuration: ReadConfiguration) throws {
        text = String(data: configuration.file.regularFileContents ?? Data(), encoding: .utf8) ?? "{}"
    }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: Data(text.utf8))
    }
}

@MainActor
enum BitChordBackup {
    static func suggestedName() -> String {
        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy-MM-dd"
        return "bitchord-backup-\(formatter.string(from: Date()))"
    }

    static func exportText() -> String {
        var prefs = AppSettings.shared.exportPrefsJson()
        let genres = PlatformSettings.shared.getBoolean(key: "replay_genres", default: true)
        if prefs.hasSuffix("}") {
            let insert = "\"replay_genres\":\"\(genres)\""
            prefs = prefs == "{}" ? "{\(insert)}" : String(prefs.dropLast()) + ",\(insert)}"
        }
        let listening = ListeningStore.shared.exportJSON().flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
        return "{\"app\":\"bitchord\",\"version\":1,\"prefs\":\(prefs),\"listening\":\(listening)}"
    }

    static func importText(_ raw: String) {
        guard let data = raw.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            AppSettings.shared.importPrefsJson(raw: raw)
            return
        }
        if let listening = obj["listening"],
           let payload = try? JSONSerialization.data(withJSONObject: listening) {
            ListeningStore.shared.importJSON(payload)
        }
        if let prefs = obj["prefs"],
           let payload = try? JSONSerialization.data(withJSONObject: prefs),
           let text = String(data: payload, encoding: .utf8) {
            AppSettings.shared.importPrefsJson(raw: text)
            if let map = prefs as? [String: Any] {
                let flag = (map["replay_genres"] as? String) ?? (map["replay_genres"] as? Bool).map { $0 ? "true" : "false" }
                if let flag { PlatformSettings.shared.putBoolean(key: "replay_genres", value: flag == "true") }
            }
            return
        }
        AppSettings.shared.importPrefsJson(raw: raw)
    }
}
