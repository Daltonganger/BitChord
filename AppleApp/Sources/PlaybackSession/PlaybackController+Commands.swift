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

// Sleep timer, downloads, AutoMix and Autoplay toggles, and Last.fm.
extension PlaybackController {
    func sleepRemaining(_ deadline: ContinuousClock.Instant) -> Double {
        let parts = ContinuousClock.now.duration(to: deadline).components
        return Double(parts.seconds) + Double(parts.attoseconds) / 1e18
    }

    func startSleep(minutes: Int) { armSleep(seconds: Double(max(0, minutes)) * 60) }

    func armSleep(seconds: Double) {
        cancelSleep()
        let deadline = ContinuousClock.now.advanced(by: .seconds(max(0, seconds)))
        sleepDeadline = deadline; sleepSecondsRemaining = Int(seconds.rounded(.up))
        let engine = self.engine
        sleepTask = Task.detached(priority: .userInitiated) { [weak self, engine] in
            var displayedSeconds = -1
            while !Task.isCancelled {
                let parts = ContinuousClock.now.duration(to: deadline).components
                let remaining = Double(parts.seconds) + Double(parts.attoseconds) / 1e18
                guard !Task.isCancelled else { return }
                if remaining <= 0 {
                    // Output muting does not wait behind a busy UI actor.
                    try? engine.pause()
                    try? engine.setSleepGain(gain: 1)
                    await MainActor.run { [weak self] in
                        guard let self, self.sleepDeadline == deadline else { return }
                        self.sleepDeadline = nil; self.sleepSecondsRemaining = nil; self.sleepTask = nil
                        self.pausePlayback(releaseAudioSession: true)
                        self.debugLog.record("sleep deadline expired; output paused and gain restored")
                    }
                    return
                }
                try? engine.setSleepGain(gain: Float(min(1, remaining / 6)))
                await MainActor.run { [weak self] in
                    guard let self, self.sleepDeadline == deadline else { return }
                    self.dolbyRenderer?.volume = self.targetOutputGain * Float(min(1, remaining / 6))
                }
                let whole = Int(remaining.rounded(.up))
                if whole != displayedSeconds {
                    displayedSeconds = whole
                    Task { @MainActor [weak self] in
                        guard let self, self.sleepDeadline == deadline else { return }
                        self.sleepSecondsRemaining = whole
                    }
                }
                try? await Task.sleep(for: .milliseconds(100), clock: .continuous)
            }
        }
    }

    func startSleepAfterTrack() {
        cancelSleep()
        sleepAfterTrack = true
        queueNextRevision &+= 1
        try? engine.holdTrackEnd(hold: true)
        debugLog.record("sleep armed after effective track end")
    }

    func cancelSleep() {
        sleepTask?.cancel(); sleepTask = nil
        sleepDeadline = nil; sleepSecondsRemaining = nil
        sleepAfterTrack = false
        try? engine.setSleepGain(gain: 1)
        dolbyRenderer?.volume = targetOutputGain
        try? engine.holdTrackEnd(hold: false)
        syncEngineQueueNext()
    }

    func downloadCurrent() -> DownloadStore.RequestResult? {
        guard let current else { return nil }
        return DownloadStore.shared.download(current)
    }

    func playQueueItem(at index: Int) {
        loadCurrent(index)
    }

    func clearUpcoming() {
        guard playingIndex + 1 < queue.count else { return }
        let removed = queue[(playingIndex + 1)...]
        removed.forEach { QualityUpgrade.forget($0.id) }
        queue.removeSubrange((playingIndex + 1)...)
        queueEditRevision &+= 1
        persistSession()
        syncEngineQueueNext()
    }

    func toggleAutoplay() {
        autoplayEnabled.toggle()
        AppSettings.shared.setAutoplay(value: autoplayEnabled)
        if !autoplayEnabled {
            autoplayRefresh.invalidate()
            // Upstream clears the stash when AutoPlay is switched off mid-loop
            // so ending ALL later does not resurrect dropped suggestions.
            repeatAllStash = []
            repeatAllStashSeed = nil
        } else if repeatMode != .all {
            maybeAutoplay(force: true)
        }
    }

    func toggleAutomix() {
        setAutomixEnabled(!automixEnabled)
    }

    func setAutomixEnabled(_ enabled: Bool) {
        guard automixEnabled != enabled else { return }
        automixEnabled = enabled
        AppSettings.shared.setSmartFadeEnabled(value: enabled)
        syncSkipMixEnabled()
        refreshPlaybackRegions()
        syncEngineQueueNext()
        applyListOrder()
    }

    /// Updated by Settings' "Mix on Skip" control.
    func setAutomixOnSkipEnabled(_ enabled: Bool) {
        guard automixOnSkipEnabled != enabled else { return }
        automixOnSkipEnabled = enabled
        PlatformSettings.shared.putBoolean(key: "automix_on_skip", value: enabled)
        syncSkipMixEnabled()
    }

    func syncSkipMixEnabled() {
        let enabled = automixEnabled
            && automixOnSkipEnabled
            && !PartyStore.shared.inParty
            && repeatMode != .one
        engine.setSkipMixEnabled(enabled: enabled)
    }

    func authoriseLastFm() {
        ScrobbleBridge.shared.lastFmAuthUrl(callback: AuthAdapter { ok, message in
            guard ok, let message else { return }
            let parts = message.split(separator: "\n", maxSplits: 1).map(String.init)
            guard parts.count == 2, let url = URL(string: parts[0]) else { return }
            #if os(macOS)
            NSWorkspace.shared.open(url)
            #else
            UIApplication.shared.open(url)
            #endif
            DispatchQueue.main.asyncAfter(deadline: .now() + 8) {
                ScrobbleBridge.shared.lastFmComplete(token: parts[1], callback: AuthAdapter { _, _ in })
            }
        })
    }

    // ---- Engine bridge ------------------------------------------------------
}
