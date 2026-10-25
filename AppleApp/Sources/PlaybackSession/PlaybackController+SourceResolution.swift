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

// Resolving a playable source: downloads, YouTube, substitutes and custom
// sources, raced by module priority, and prefetching what plays next.
extension PlaybackController {
    /// Resolves the entry's source to an engine-loadable string. Local paths
    /// pass through unchanged; `"yt:<videoId>"` sources go through the
    /// `StreamResolver` to get a real HTTP URL, which has already been probed
    /// and proven to serve audio.
    ///
    /// The fetch goes through the shared `Http` client, so it shares a connection
    /// context with both the `player` request that minted the URL and the probe
    /// that cleared it — upstream's rule about why that has to be one stack. An
    /// earlier version ran a separate fetch here "because URLSession has a
    /// different TLS fingerprint"; that was a symptom of the split, not a cause.
    ///
    /// Upstream's ExoPlayer starts on the first bounded range. So do we: the first
    /// chunk is written and we return that path while later chunks keep appending.
    /// The engine's GrowingFile waits at EOF until `.complete`.
    struct ResolvedSource: Sendable {
        let source: String
        let headers: [String: String]
        let kbps: Int
        var lossless: Bool = false
        var codec: String? = nil
        var durationSec: Int? = nil
        /// Player-response loudness figure (YouTube only; substitutes, cache
        /// hits and local files carry none). Rides to the engine's
        /// normalization stage with the load.
        var loudnessDb: Double? = nil
        var clientName: String? = nil
        var resolutionAt: TimeInterval? = nil
        var origin: Origin = .other
        var youtubeVideoId: String? = nil

        enum Origin: Sendable { case local, cache, youtube, substitute, other }

        var format: QualityUpgrade.Format {
            QualityUpgrade.Format(
                codec: codec,
                kbps: kbps > 0 ? kbps : nil,
                lossless: lossless
            )
        }

        func asCandidate() -> QualityUpgrade.Candidate {
            QualityUpgrade.Candidate(
                url: source, headers: headers, format: format, durationSec: durationSec
            )
        }
    }

    struct ResolveOutcome: Sendable {
        let source: ResolvedSource
        let leftover: Task<QualityUpgrade.Candidate?, Never>?
    }

    /// Completes as soon as either source yields a usable stream. A task group
    /// waits for every child when its scope exits, even after cancellation, so
    /// it must not own the two independent lookups in this first-audio race.
    private actor FirstUsableSourceRace {
        enum Winner: Sendable {
            case substitute(QualityUpgrade.Candidate)
            case youtube(ResolvedSource)
            case timedOut
            case none
        }

        private var result: Winner?
        private var lookupFinished = false
        private var fallbackFinished = false
        private var continuation: CheckedContinuation<Winner, Never>?
        private var timeoutTask: Task<Void, Never>?

        func wait(timeoutNanoseconds: UInt64 = 800_000_000) async -> Winner {
            await withCheckedContinuation { continuation in
                if let result {
                    continuation.resume(returning: result)
                } else {
                    self.continuation = continuation
                    timeoutTask = Task {
                        try? await Task.sleep(nanoseconds: timeoutNanoseconds)
                        await self.timeOut()
                    }
                }
            }
        }

        private func timeOut() {
            finish(.timedOut)
        }

        func finishLookup(_ candidate: QualityUpgrade.Candidate?) {
            guard result == nil else { return }
            if let candidate {
                finish(.substitute(candidate))
            } else {
                lookupFinished = true
                if fallbackFinished { finish(.none) }
            }
        }

        func finishFallback(_ source: ResolvedSource?) {
            guard result == nil else { return }
            if let source {
                finish(.youtube(source))
            } else {
                fallbackFinished = true
                if lookupFinished { finish(.none) }
            }
        }

        private func finish(_ winner: Winner) {
            guard result == nil else { return }
            result = winner
            timeoutTask?.cancel()
            timeoutTask = nil
            continuation?.resume(returning: winner)
            continuation = nil
        }
    }

    struct ResolvePrefs: Sendable {
        let maxKbps: Int
        let wantLossless: Bool
        let jiosaavn: Bool
        let canSubstitute: Bool
        let preferMusicOnly: Bool

        @MainActor
        static func current() -> ResolvePrefs {
            let streamQuality = AppSettings.shared.effectiveAudioQuality(metered: NetworkQuality.shared.metered).name
            NSLog("[BitChord] playback quality requested=\(streamQuality) network=\(NetworkQuality.shared.metered ? "metered" : "unmetered")")
            return ResolvePrefs(
                maxKbps: Int(NetworkQuality.shared.maxKbps),
                wantLossless: streamQuality == "LOSSLESS",
                jiosaavn: PlatformSettings.shared.getBoolean(key: "jiosaavn_enabled", default: true),
                canSubstitute: QualityUpgrade.canSubstituteForYouTube(),
                preferMusicOnly: PlatformSettings.shared.getBoolean(key: "prefer_music_only", default: false)
            )
        }
    }

    static func resolveSource(
        _ entry: QueueEntry, prefs: ResolvePrefs,
        latencyResolved: PlaybackLatencyRecorder? = nil
    ) async throws -> ResolveOutcome {
        await DownloadStore.shared.waitUntilReady()
        try Task.checkCancellation()
        if let asset = DownloadStore.shared.asset(for: entry) {
            await latencyResolved?(ProcessInfo.processInfo.systemUptime, "cached", "download")
            return ResolveOutcome(source: ResolvedSource(source: asset.path, headers: [:], kbps: asset.kbps,
                lossless: asset.lossless, codec: asset.codec, origin: .local, youtubeVideoId: asset.youtubeVideoId), leftover: nil)
        }
        if entry.source.hasPrefix("saavn:") || entry.id.hasPrefix("saavn:") {
            let id = entry.source.hasPrefix("saavn:") ? String(entry.source.dropFirst(6)) : String(entry.id.dropFirst(6))
            if let hit = await JioSaavn.streamURL(for: id) {
                await latencyResolved?(ProcessInfo.processInfo.systemUptime, "cold", "JioSaavn")
                return ResolveOutcome(
                    source: ResolvedSource(
                        source: hit.url, headers: [:], kbps: hit.kbps,
                        lossless: false, origin: .substitute
                    ),
                    leftover: nil
                )
            }
            throw InnertubeStreamResolver.StreamError(message: "JioSaavn had no stream")
        }
        if entry.source.hasPrefix("src:") || entry.id.hasPrefix("src:") {
            // An addon search row keeps its own `src:` key rather than a video
            // id, so it resolves through the source resolver's own ordering —
            // its source first, then any enabled source that has the recording.
            let key = entry.source.hasPrefix("src:") ? entry.source : entry.id
            if let stream = await SourceSearch.shared.stream(
                key: key, title: entry.title, artist: entry.artist,
                // Unknown is not zero: a 0 s runtime would be read as a real
                // length and reject every match.
                durationSec: entry.durationSeconds > 0 ? Int(entry.durationSeconds.rounded()) : nil
            ) {
                await latencyResolved?(ProcessInfo.processInfo.systemUptime, "cold", "source substitute")
                return ResolveOutcome(
                    source: ResolvedSource(
                        source: stream.url, headers: stream.headers, kbps: stream.kbps ?? 0,
                        lossless: stream.lossless, codec: stream.codec,
                        durationSec: stream.durationSec, origin: .substitute
                    ),
                    leftover: nil
                )
            }
            throw InnertubeStreamResolver.StreamError(message: "That source had no stream")
        }
        #if DEBUG
        if entry.source.hasPrefix("dolby-test:") {
            await latencyResolved?(ProcessInfo.processInfo.systemUptime, "cold", "fixture")
            return ResolveOutcome(source: ResolvedSource(source: String(entry.source.dropFirst(11)), headers: [:], kbps: 0, codec: "eac3-joc"), leftover: nil)
        }
        #endif
        guard entry.source.hasPrefix("yt:") else {
            await latencyResolved?(ProcessInfo.processInfo.systemUptime, "cold", "WebDAV/local")
            // A file on a remote library plays from its own address, and that address
            // needs the share's credential — the same header the cover fetch and the
            // listing used, asked of the one rule that decides which requests may carry
            // it. Empty for a local path and for a JioSaavn URL, which is why this is
            // safe on the path every non-YouTube row takes.
            return ResolveOutcome(
                source: ResolvedSource(
                    source: entry.source,
                    headers: WebDavBridge.shared.playbackHeaders(fileUrl: entry.source),
                    kbps: 0,
                    codec: ["ac3", "ec3", "eac3"].contains(URL(string: entry.source)?.pathExtension.lowercased() ?? "") ? URL(string: entry.source)?.pathExtension : nil,
                    origin: .local
                ),
                leftover: nil
            )
        }
        let videoId = String(entry.source.dropFirst(3))
        if let growing = streamGate.lock.withLock({ streamGate.growing[videoId] }),
           FileManager.default.fileExists(atPath: growing) {
            await latencyResolved?(ProcessInfo.processInfo.systemUptime, "cached", "stream cache")
            return ResolveOutcome(
                source: ResolvedSource(
                    source: growing, headers: [:], kbps: 0, durationSec: streamGate.lock.withLock { streamGate.durations[videoId] }, origin: .cache
                ),
                leftover: nil
            )
        }
        // A track the listener has reverted is held on YouTube's own upload, so
        // there is nothing to look for: a substitute found here would be the
        // exact thing they rejected. This has to be checked *before* the lookup
        // rather than after it, or a revert still costs a network round trip to
        // arrive at the answer it already knew.
        if OriginalVersion.shared.isPinned(videoId: videoId) {
            let source = try await Self.resolveYouTube(videoId: videoId, prefs: prefs)
            await latencyResolved?(source.resolutionAt ?? ProcessInfo.processInfo.systemUptime,
                                   "cold", source.clientName ?? "unknown")
            return ResolveOutcome(source: source, leftover: nil)
        }
        if let cached = await StreamFileCache.shared.path(for: videoId, maxKbps: prefs.maxKbps, requireLossless: prefs.wantLossless) {
            await latencyResolved?(ProcessInfo.processInfo.systemUptime, "cached", "stream cache")
            let metadata = await StreamFileCache.shared.metadata(at: cached)
            let cachedDuration = metadata?.durationSeconds
            return ResolveOutcome(
                source: ResolvedSource(
                    source: cached, headers: [:], kbps: metadata?.kbps ?? 0, lossless: QualityUpgrade.Format.isLosslessCodec(metadata?.codec), codec: metadata?.codec, durationSec: cachedDuration, loudnessDb: metadata?.relativeLoudnessDb, clientName: "cache", origin: .cache, youtubeVideoId: metadata?.youtubeVideoId ?? videoId
                ),
                leftover: nil
            )
        }
        return try await resolveWithModulePriority(
            entry, videoId: videoId, prefs: prefs, latencyResolved: latencyResolved
        )
    }

    /// Upstream `resolveWithModulePriority`: start YouTube and substitutes
    /// together. First usable URL wins so a slow lookup cannot hold the first
    /// note hostage. A lookup that loses is **not** cancelled — it is handed
    /// to QualityUpgrade as `inFlight`.
    private static func resolveWithModulePriority(
        _ entry: QueueEntry, videoId: String, prefs: ResolvePrefs,
        latencyResolved: PlaybackLatencyRecorder? = nil
    ) async throws -> ResolveOutcome {
        let lookup = Task<QualityUpgrade.Candidate?, Never> {
            await Self.resolveSubstitute(entry, prefs: prefs)?.asCandidate()
        }
        let fallback = Task<ResolvedSource, Error> {
            try await Self.resolveYouTube(videoId: videoId, prefs: prefs)
        }

        // Prefer-music-only for a music video: the video's own upload is the
        // wrong answer when the catalogue audio was asked for, so the race is
        // not run at all — the substitute lookup is awaited, and YouTube's
        // upload is only the fallback when the lookup misses. Upstream does the
        // same switch after the fact (`switchToMusicOnly`); here the wait
        // happens before anything starts, so the video never plays first.
        if prefs.preferMusicOnly, entry.isVideo {
            if let stream = await lookup.value {
                fallback.cancel()
                await latencyResolved?(ProcessInfo.processInfo.systemUptime, "cold", "source substitute")
                return ResolveOutcome(
                    source: ResolvedSource(
                        source: stream.url, headers: stream.headers,
                        kbps: stream.format.kbps ?? 0,
                        lossless: stream.format.lossless, codec: stream.format.codec,
                        durationSec: stream.durationSec,
                        origin: .substitute
                    ),
                    leftover: nil
                )
            }
            let source = try await fallback.value
            await latencyResolved?(source.resolutionAt ?? ProcessInfo.processInfo.systemUptime,
                                   "cold", source.clientName ?? "unknown")
            return ResolveOutcome(source: source, leftover: nil)
        }

        let race = FirstUsableSourceRace()
        _ = Task { await race.finishLookup(await lookup.value) }
        _ = Task { await race.finishFallback(try? await fallback.value) }

        switch await race.wait(timeoutNanoseconds: 800_000_000) {
        case .substitute(let stream):
            fallback.cancel()
            await latencyResolved?(ProcessInfo.processInfo.systemUptime, "cold", "source substitute")
            let source = ResolvedSource(
                source: stream.url, headers: stream.headers,
                kbps: stream.format.kbps ?? 0,
                lossless: stream.format.lossless, codec: stream.format.codec,
                durationSec: stream.durationSec,
                origin: .substitute
            )
            return ResolveOutcome(source: source, leftover: nil)
        case .youtube(let source):
            // Preserve the slower substitute lookup for the in-playback quality
            // upgrade path instead of making it delay the first audible sample.
            await latencyResolved?(source.resolutionAt ?? ProcessInfo.processInfo.systemUptime,
                                   "cold", source.clientName ?? "unknown")
            return ResolveOutcome(source: source, leftover: lookup)
        case .timedOut:
            // A substitute lookup gets a short head start, then the direct
            // YouTube URL carries first sound while the lookup stays available
            // to the existing in-playback quality-upgrade path.
            do {
                let source = try await fallback.value
                await latencyResolved?(source.resolutionAt ?? ProcessInfo.processInfo.systemUptime,
                                       "cold", source.clientName ?? "unknown")
                return ResolveOutcome(source: source, leftover: lookup)
            } catch {
                lookup.cancel()
                throw error
            }
        case .none:
            // Preserve the resolver's actual failure instead of replacing
            // account/CDN errors with an uninformative "No stream".
            let source = try await fallback.value
            await latencyResolved?(source.resolutionAt ?? ProcessInfo.processInfo.systemUptime,
                                   "cold", source.clientName ?? "unknown")
            return ResolveOutcome(source: source, leftover: nil)
        }
    }

    /// Innertube URL + growing local file, the path that starts playback earliest.
    ///
    /// ## Why there is no retry loop, and what replaces it
    ///
    /// There used to be one: on a 403, ask for the same URL again. That is the
    /// worst available response, because the URL is not stale data — a googlevideo
    /// URL is *bound to the session that minted it*, and re-fetching the same one
    /// with the same headers is the single most reliable way to talk a session
    /// into being throttled. It also cannot succeed: the reason the first fetch
    /// was refused is the same reason the second will be.
    ///
    /// What replaces it is a *fresh player request*, which is a different thing:
    ///
    ///  - The bridge has already reported the refusal to the resolver, which forgot
    ///    the URL and stood the client that minted it down. So the second attempt
    ///    asks `player` again and gets a **new URL, usually from a different
    ///    client** — the variable that actually changed is the one that matters.
    ///  - It happens **once**. A second refusal means the problem is not this
    ///    track's URL, and a third attempt would be a session hammering itself.
    ///
    /// A *transport* failure is the opposite case and deliberately does not
    /// re-resolve: the URL was served once, so it is alive, and handing it to the
    /// engine — which has its own connection management and its own retries — is a
    /// better second chance than minting a new URL for a problem the new URL would
    /// not fix.
    private static func resolveYouTube(
        videoId: String, prefs: ResolvePrefs
    ) async throws -> ResolvedSource {
        var stream: ResolvedYouTubeStream
        do {
            stream = try await InnertubeStreamResolver.shared.resolve(videoId: videoId, maxKbps: prefs.maxKbps)
        } catch {
            // The per-client reasons, which the shared module carries and the
            // sentence a listener sees does not. Recorded here because this is where
            // the debug log lives, and a report that says "playback failed" is worth
            // nothing next to one that says which client refused and why.
            if let streamError = error as? InnertubeStreamResolver.StreamError {
                PlaybackDebugLog.shared.record(streamError.raw, about: videoId)
            }
            throw error
        }
        var resolvedAt = ProcessInfo.processInfo.systemUptime
        var rangeRetry = 0
        while true {
            if let seconds = stream.durationSeconds, seconds > 0 {
                streamGate.lock.withLock { streamGate.durations[videoId] = seconds }
            }
            do {
                let localPath = try await streamViaKtor(
                    videoId: videoId, url: stream.url, headers: stream.headers,
                    codec: stream.mimeType.lowercased().contains("opus") ? "Opus" : "AAC",
                    kbps: stream.kbps, relativeLoudnessDb: stream.loudnessDb,
                    youtubeVideoId: videoId, durationSeconds: stream.durationSeconds,
                    maxKbps: prefs.maxKbps, expectedMimeType: stream.mimeType,
                    firstRangeRetry: rangeRetry
                )
                return ResolvedSource(
                    source: localPath, headers: [:], kbps: stream.kbps,
                    durationSec: stream.durationSeconds, loudnessDb: stream.loudnessDb,
                    clientName: stream.clientName, resolutionAt: resolvedAt,
                    origin: .youtube, youtubeVideoId: videoId
                )
            } catch {
                let reason = (error as? InnertubeStreamResolver.StreamError)?.message
                let rejected = StreamDownloadBridge.shared.isInvalidFirstRange(message: reason)
                    || StreamDownloadBridge.shared.isRefusal(message: reason)
                guard rejected else {
                    DebugLog.shared.d(message: "\(videoId): stream failed: \(reason ?? "\(error)")")
                    return ResolvedSource(
                        source: stream.url, headers: stream.headers, kbps: stream.kbps,
                        durationSec: stream.durationSeconds, loudnessDb: stream.loudnessDb,
                        clientName: stream.clientName, resolutionAt: resolvedAt,
                        origin: .youtube, youtubeVideoId: videoId
                    )
                }
                guard rangeRetry < 2 else {
                    throw InnertubeStreamResolver.StreamError(
                        message: "Opening media range was rejected after same-client and alternate-client retries: \(reason ?? "unknown response")"
                    )
                }
                rangeRetry += 1
                DebugLog.shared.d(message: "\(videoId): opening range rejected; resolving retry \(rangeRetry)")
                stream = try await InnertubeStreamResolver.shared.resolve(videoId: videoId, maxKbps: prefs.maxKbps)
                resolvedAt = ProcessInfo.processInfo.systemUptime
            }
        }
    }

    /// Catalogues ranked above YouTube — first match that is the same recording.
    /// The cross-source race, delegated to the shared resolver.
    ///
    /// This used to be a hand-rolled `withTaskGroup` over the custom HTTP source,
    /// the JS module host and JioSaavn, with "JioSaavn must beat 256 kbps" as the
    /// only quality bar, no notion of the user's ranking, and no recording check at
    /// all beyond a runtime comparison the caller had to remember to apply. It could
    /// not be made correct without the parts that are judgement, which now live in
    /// the shared module's `SourceResolver`:
    ///
    ///  - The track is matched by `TrackMatcher` before anything is opened, so a
    ///    cover cannot be substituted for the recording the listener picked.
    ///  - Rank decides who is asked; the race decides who answers first. The two
    ///    are not the same thing, and only the first one is a list.
    ///  - `playingDurationSec` set routes this through the *upgrade* path, whose bar
    ///    is "beats what is actually playing" rather than "satisfies the request",
    ///    and which waits for every source so a slow one holding the FLAC still
    ///    gets to serve it mid-track rather than being dropped for a fast 320.
    static func resolveSubstitute(
        _ entry: QueueEntry, prefs: ResolvePrefs, waitForAll: Bool = false,
        playingDurationSec: Int? = nil, playing: QualityUpgrade.Format? = nil
    ) async -> ResolvedSource? {
        // Nothing configured outranks YouTube, so the race cannot be won and asking
        // would be pure latency. Answerable from the source list alone, with no
        // search, which is why it is safe to call before the track is resolved.
        guard prefs.canSubstitute else { return nil }

        let stream = await SourceSubstitution.substitute(
            title: entry.title,
            artist: entry.artist,
            durationSec: playingDurationSec,
            album: entry.albumName,
            isExplicit: entry.isExplicit.map { KotlinBoolean(value: $0) },
            isVideo: entry.isVideo,
            playing: (playing ?? (waitForAll ? QualityUpgrade.Format(codec: nil, kbps: nil, lossless: false) : nil)).map { SourceSubstitution.Format(codec: $0.codec, kbps: $0.kbps, lossless: $0.lossless) }
        )
        guard let stream else { return nil }
        return ResolvedSource(
            source: stream.url,
            headers: stream.headers,
            kbps: stream.kbps ?? 0,
            lossless: stream.lossless, codec: stream.codec,
            durationSec: stream.durationSec,
            origin: .substitute
        )
    }

    private static func resolveCustom(title: String, artist: String) async -> ResolvedSource? {
        await withCheckedContinuation { cont in
            SourceBridge.shared.resolveCustom(title: title, artist: artist, quality: "LOSSLESS", callback: CustomStreamAdapter { json, _ in
                guard let json, let data = json.data(using: .utf8),
                      let hit = try? JSONDecoder().decode(CustomHit.self, from: data) else {
                    cont.resume(returning: nil)
                    return
                }
                cont.resume(returning: ResolvedSource(
                    source: hit.url, headers: [:], kbps: hit.kbps,
                    lossless: hit.lossless == true, origin: .substitute
                ))
            })
        }
    }

    private struct CustomHit: Codable {
        let url: String
        let kbps: Int
        let lossless: Bool?
    }

    /// Warm short-lived stream URLs for the upcoming queue window. This does
    /// not request media bytes; the opening range remains coupled to playback.
    func warmUpcoming(around index: Int, generation: UInt64) {
        guard state == .playing || state == .buffering || state == .paused else { return }
        // Repeat-one never arms a different next track into the engine, but the
        // song after the loop still has to be measured while the loop runs —
        // otherwise turning repeat off starts a cold whole-track decode with
        // seconds left (upstream requestAnalysisAround). Automix self-mix only
        // needs the current file, which is already loaded.
        // The shared resolver caches issued URLs for 20 seconds. Keep the work
        // within that freshness window and cap it to a visible queue-sized set.
        let metered = NetworkQuality.shared.metered
        let prefetchCount = metered ? 1 : 3
        let prefetch = queue.dropFirst(index + 1).prefix(prefetchCount).filter {
            $0.source.hasPrefix("yt:") && !$0.id.hasPrefix("saavn:")
        }
        let prefs = ResolvePrefs.current()
        let fingerprint = "\(prefs.maxKbps)|metered=\(metered)|" + prefetch.map(\.id).joined(separator: "|")
        guard fingerprint != urlPrefetchFingerprint else { return }
        urlPrefetchFingerprint = fingerprint
        urlPrefetchTask?.cancel()
        guard !prefetch.isEmpty else { return }
        PlaybackDebugLog.shared.record("latency: prefetch url \(prefetch.count) rows")
        urlPrefetchTask = Task.detached(priority: .utility) { [weak self] in
            for entry in prefetch {
                guard !Task.isCancelled else { return }
                let id = entry.id
                let stillQueued = await MainActor.run { [weak self] in
                    guard let self else { return false }
                    guard self.playGeneration == generation,
                          self.queue.contains(where: { $0.id == id }) else { return false }
                    // A path change can happen while this sequential read-ahead
                    // is running. Keep the first queued item, but stop before
                    // resolving additional speculative URLs on a metered path.
                    return !NetworkQuality.shared.metered || id == prefetch.first?.id
                }
                guard stillQueued else { continue }
                let videoId = String(entry.source.dropFirst(3))
                _ = try? await InnertubeStreamResolver.shared.resolve(videoId: videoId, maxKbps: prefs.maxKbps)
            }
        }
    }

    func refreshUpcomingURLPrefetch() {
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.warmUpcoming(around: self.playingIndex, generation: self.playGeneration)
        }
    }

    private final class StreamGate: @unchecked Sendable {
        let lock = NSLock()
        var tasks: [String: Task<String, Error>] = [:]
        /// Path of a download that has its first bytes, set before the
        /// downloader's continuation resumes so a second resolve cannot miss it.
        var growing: [String: String] = [:]
        var growingQuality: [String: String] = [:]
        var growingKbps: [String: Int] = [:]
        var durations: [String: Int] = [:]
    }

    private static let streamGate = StreamGate()

    /// Starts playback as soon as the first range is on disk — up to a megabyte,
    /// or half that for a client that caps lower. Remaining ranges keep
    /// appending; [StreamFileCache] is filled when the last one lands so a
    /// re-tap does not fetch again.
    static func streamViaKtor(
        videoId: String, url: String, headers: [String: String], codec: String = "unknown", kbps: Int = 0, relativeLoudnessDb: Double? = nil, youtubeVideoId: String? = nil, durationSeconds: Int? = nil, maxKbps: Int = Int(Int32.max), expectedMimeType: String? = nil, firstRangeRetry: Int = 0
    ) async throws -> String {
        let taskKey = videoId + "|" + StreamFileCache.qualityIdentity + "|" + DiskCache.hashName(url)
        let task: Task<String, Error> = streamGate.lock.withLock {
            if let existing = streamGate.tasks[taskKey] { return existing }
            let created = Task { try await streamViaKtorOnce(videoId: videoId, url: url, headers: headers, codec: codec, kbps: kbps, relativeLoudnessDb: relativeLoudnessDb, youtubeVideoId: youtubeVideoId, durationSeconds: durationSeconds, maxKbps: maxKbps, expectedMimeType: expectedMimeType, firstRangeRetry: firstRangeRetry) }
            streamGate.tasks[taskKey] = created
            return created
        }
        do {
            let path = try await task.value
            streamGate.lock.withLock {
                if streamGate.tasks[taskKey] != nil { streamGate.tasks[taskKey] = nil }
            }
            return path
        } catch {
            streamGate.lock.withLock {
                if streamGate.tasks[taskKey] != nil { streamGate.tasks[taskKey] = nil }
            }
            throw error
        }
    }

    private static func streamViaKtorOnce(
        videoId: String, url: String, headers: [String: String], codec: String = "unknown", kbps: Int = 0, relativeLoudnessDb: Double? = nil, youtubeVideoId: String? = nil, durationSeconds: Int? = nil, maxKbps: Int = Int(Int32.max), expectedMimeType: String? = nil, firstRangeRetry: Int = 0
    ) async throws -> String {
        let qualityIdentity = StreamFileCache.qualityIdentity
        return try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<String, Error>) in
            let lock = NSLock()
            var resumed = false
            let ready = DownloadCallbackAdapter { path, message in
                lock.lock()
                defer { lock.unlock() }
                guard !resumed else { return }
                resumed = true
                if let path {
                    streamGate.lock.withLock { streamGate.growing[videoId] = path; streamGate.growingQuality[videoId] = qualityIdentity; streamGate.growingKbps[videoId] = kbps }
                    Task { await StreamFileCache.shared.noteGrowing(videoId: videoId, path: path, kbps: kbps, quality: qualityIdentity, relativeLoudnessDb: relativeLoudnessDb, youtubeVideoId: youtubeVideoId, durationSeconds: durationSeconds) }
                    continuation.resume(returning: path)
                } else {
                    continuation.resume(throwing: InnertubeStreamResolver.StreamError(
                        message: message ?? "Stream failed"))
                }
            }
            let done = DownloadCallbackAdapter { path, message in
                // The bridge reports a failure before the first byte — a refused
                // or invalid opening range, a download with no bytes — on `done`
                // alone and never calls `ready`. Unanswered, the continuation
                // leaked and the load waiting on it hung for good: the next song
                // never started and nothing said why. Answering it here is also
                // what lets the caller's re-resolve on a rejected range run.
                lock.lock()
                let waiting = !resumed
                resumed = true
                lock.unlock()
                if waiting, path == nil {
                    continuation.resume(throwing: InnertubeStreamResolver.StreamError(
                        message: message ?? "Stream ended before its first bytes"))
                    return
                }
                if waiting, let path { continuation.resume(returning: path) }
                if let path {
                    Task { await StreamFileCache.shared.store(videoId, path: path, sourceIdentity: DiskCache.hashName(url), codec: codec, kbps: kbps, quality: qualityIdentity, relativeLoudnessDb: relativeLoudnessDb, youtubeVideoId: youtubeVideoId, durationSeconds: durationSeconds) }
                } else if let message {
                    print("[Playback] stream tail failed for \(videoId): \(message)")
                    streamGate.lock.withLock { streamGate.growing[videoId] = nil }
                    Task { await StreamFileCache.shared.dropGrowing(videoId: videoId) }
                }
            }
            // With the YouTube id the bridge can answer a mid-track refusal with a
            // fresh URL for the same rendition instead of cutting the song short.
            if let youtubeVideoId {
                StreamDownloadBridge.shared.streamTrackToFile(
                    videoId: youtubeVideoId, maxKbps: Int32(clamping: maxKbps),
                    url: url, headers: headers, expectedMimeType: expectedMimeType,
                    firstRangeRetry: Int32(clamping: firstRangeRetry),
                    ready: ready, done: done)
                return
            }
            StreamDownloadBridge.shared.streamToFile(url: url, headers: headers, ready: ready, done: done)
        }
    }
}
