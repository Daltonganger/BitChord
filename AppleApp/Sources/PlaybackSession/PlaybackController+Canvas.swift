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

// Spotify Canvas lookups.
extension PlaybackController {
    /// Re-read the feature/network preferences and run the same lookup used
    /// when a track starts. Settings surfaces call this after changing a
    /// canvas preference.
    func refreshCanvasLookup() {
        guard let current else { return }
        fetchCanvas(for: current)
    }

    func fetchCanvas(for entry: QueueEntry) {
        let titleKey = entry.title.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let artistKey = entry.artist.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let identity = "\(titleKey)|\(artistKey)"
        if canvasIdentity != identity {
            canvasURL = nil
            canvasFallbackURL = nil
            canvasSource = nil
            canvasIdentity = identity
        }
        canvasLookupGeneration &+= 1
        let generation = canvasLookupGeneration
        guard NetworkQuality.shared.canvasAllowed,
              // A catalogue track can have a local downloaded file while
              // retaining its catalogue id. A true library file uses its
              // path as both id and source and has no catalogue lookup.
              !(entry.isLocal && entry.id == entry.source) else {
            canvasURL = nil
            canvasFallbackURL = nil
            canvasSource = nil
            return
        }
        CanvasBridge.shared.lookup(title: entry.title, artist: entry.artist, album: entry.albumName, callback: CanvasAdapter { [weak self] json in
            Task { @MainActor in
                guard let self, self.canvasLookupGeneration == generation,
                      let active = self.current,
                      active.title.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() == titleKey,
                      active.artist.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() == artistKey else { return }
                guard let json, let data = json.data(using: .utf8),
                      let payload = try? JSONDecoder().decode(CanvasPayload.self, from: data),
                      let url = URL(string: payload.url) else {
                    NSLog("[BitChord] no animated artwork found for '%@' by '%@'", entry.title, entry.artist)
                    return
                }
                self.canvasURL = url
                self.canvasFallbackURL = payload.fallbackUrl.flatMap(URL.init(string:))
                self.canvasSource = payload.source
                NSLog("[BitChord] animated artwork source=%@ host=%@ for '%@'",
                      payload.source, url.host ?? "unknown", entry.title)
            }
        })
        if entry.albumName == nil {
            Task { @MainActor [weak self] in
                try? await Task.sleep(for: .milliseconds(350))
                guard let self, self.canvasLookupGeneration == generation,
                      let latest = self.current, latest.id == entry.id,
                      let album = latest.albumName else { return }
                self.canvasLookupGeneration &+= 1
                let retryGeneration = self.canvasLookupGeneration
                CanvasBridge.shared.lookup(title: latest.title, artist: latest.artist, album: album, callback: CanvasAdapter { [weak self] json in
                    Task { @MainActor in
                        guard let self, self.canvasLookupGeneration == retryGeneration,
                              self.current?.id == latest.id else { return }
                        guard let json,
                              let data = json.data(using: .utf8),
                              let payload = try? JSONDecoder().decode(CanvasPayload.self, from: data),
                              let url = URL(string: payload.url) else {
                            NSLog("[BitChord] no animated artwork found for '%@' by '%@' after album metadata arrived",
                                  latest.title, latest.artist)
                            return
                        }
                        self.canvasURL = url
                        self.canvasFallbackURL = payload.fallbackUrl.flatMap(URL.init(string:))
                        self.canvasSource = payload.source
                        NSLog("[BitChord] animated artwork source=%@ host=%@ for '%@' after album metadata arrived",
                              payload.source, url.host ?? "unknown", latest.title)
                    }
                })
            }
        }
    }

    private struct CanvasPayload: Codable {
        let url: String
        let source: String
        let fallbackUrl: String?
    }
}
