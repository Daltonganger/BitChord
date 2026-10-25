import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// Whether a scanned file counts as music, ported 1:1 from upstream
/// `LocalMediaRepository.isEligibleLocalMusic`.
///
/// Media scanners mark notification sounds and voice notes as music, so the
/// scan needs a second gate of its own: at least 30 seconds long, a music
/// container, and nowhere under an alarms / notifications / ringtones /
/// podcasts / audiobooks / recordings path. Kept beside the toggle that
/// controls it so the rule and its switch cannot drift apart; the scan calls
/// this per file while `filter_non_music_audio` is on.
enum LocalMusicEligibility {
    static let minDurationSeconds = 30.0

    static let musicExtensions: Set<String> = [
        "mp3", "m4a", "flac", "ogg", "opus", "aac", "webm",
    ]

    static let nonMusicPathSegments = [
        "/alarms/",
        "/notifications/",
        "/ringtones/",
        "/podcasts/",
        "/audiobooks/",
        "/recordings/",
        "/voice recorder/",
        "/sound_recorder/",
        "/call_rec/",
        "/whatsapp voice notes/",
    ]

    static func isEligible(durationSeconds: Double, displayName: String, path: String?) -> Bool {
        if durationSeconds < minDurationSeconds { return false }
        let fileName: String = {
            if let path, let last = path.split(separator: "/").last, !last.isEmpty {
                return String(last)
            }
            return displayName
        }()
        guard let ext = fileName.split(separator: ".").last.map({ $0.lowercased() }),
              musicExtensions.contains(ext)
        else {
            return false
        }
        guard let path else { return true }
        let normalized = path.replacingOccurrences(of: "\\", with: "/").lowercased()
        return !nonMusicPathSegments.contains { normalized.contains($0) }
    }
}
