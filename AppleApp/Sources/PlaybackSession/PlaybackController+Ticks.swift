import Foundation
import BitChordShared
import os
#if os(iOS)
import UIKit
#if DEBUG
import AVFoundation
#endif
#else
import AppKit
#endif

// The periodic ticks: sleep, history, scrobbling, listening and widgets.
extension PlaybackController {
    func tickSleep() {
        if let mixFadeUntil, Date() >= mixFadeUntil {
            self.mixFadeUntil = nil
            smartMixInProgress = false
            // An upgrade that arrived during the ramp was shelved so it would
            // not cut the blend. The new song is on its own now.
            if let entry = current, QualityUpgrade.shelvedFor(entry.id) != nil {
                lookForBetterCopy(entry, codec: "", kbps: nerd?.kbps ?? 0)
            }
        }
    }

    func tickHistory() {
        guard let id = historyVideoID else { return }
        PlaybackTrackerBridge.shared.onProgress(
            videoId: id,
            positionSeconds: Int64(position)
        )
    }

    func tickScrobble() {
        guard let current, duration > 0 else { return }
        let minDur = Double(PlatformSettings.shared.getInt(key: "scrobble_min_duration", default: 30))
        let pct = Double(PlatformSettings.shared.getFloat(key: "scrobble_delay_percent", default: 0.5))
        let maxDelay = Double(PlatformSettings.shared.getInt(key: "scrobble_delay_seconds", default: 180))
        let threshold = min(max(duration * pct, minDur), maxDelay)
        if !scrobbleArmed, position >= minDur {
            scrobbleArmed = true
        }
        if scrobbleArmed, !scrobbleSent, position >= threshold {
            ScrobbleBridge.shared.scrobble(
                artist: scrobbleArtist(for: current), title: current.title, album: current.albumName,
                durationSec: Swift.Int32(duration)
            )
            scrobbleSent = true
        }
    }

    func tickListening() {
        guard let current else { return }
        ListeningStore.shared.onSample(
            id: current.id, title: current.title, artist: current.artist,
            album: current.albumName, albumId: current.albumId, artistId: current.artistId,
            art: current.thumbnailUrl, duration: duration
        )
    }

    func pollWidgetCommands() {
        guard let defaults = UserDefaults(suiteName: "group.app.bitchord.BitChord"),
              let cmd = defaults.string(forKey: "widget.command") else { return }
        defaults.removeObject(forKey: "widget.command")
        switch cmd {
        case "toggle": togglePlayPause()
        case "next": next()
        case "previous": previous()
        default: break
        }
    }

    func pauseForBackground() {
        if isPlaying || isBuffering { pausePlayback() }
    }
}
