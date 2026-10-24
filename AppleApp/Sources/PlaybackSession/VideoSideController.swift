import AVFoundation
import Foundation
import Observation
import QuartzCore

/// The clock the picture follows. The host supplies these from the audio player;
/// they are closures rather than the controller itself so this file stays out
/// of the engine and owns no audio state.
struct VideoClock {
    var position: @MainActor () -> Double
    var isPlaying: @MainActor () -> Bool
    /// 1.0 normally; the engine's pitch-preserving speed otherwise.
    var rate: @MainActor () -> Double = { 1 }
}

/// One muted, video-only `AVPlayer` that follows the audio engine's clock.
///
/// The audio engine is the only clock; this player never owns sound and never
/// tells the engine anything. Every 250 ms it compares the picture's position
/// with the engine's and either lets it run, nudges its rate by at most ±0.1,
/// seeks it when the drift is past 150 ms, or pauses it with the transport.
/// Any video failure is contained here: the state becomes `.failed`, the layer
/// disappears, and the artwork returns while the audio keeps playing.
///
/// A surface owns the `AVPlayerLayer`; this class only owns the player and the
/// policy. Nothing is wired to the player UI yet: the O9 surfaces pass a
/// `VideoClock` and call `attach`/`detach`.
@MainActor
@Observable
final class VideoSideController {
    enum State: Equatable {
        case idle
        case loading
        case ready
        /// The picture is not usable; the surface should show the artwork.
        case failed
    }

    private(set) var state: State = .idle
    /// The side currently attached, for the surface's aspect ratio and title.
    private(set) var side: ResolvedVideoSide?
    /// Song time to picture time, when the picture is another upload of the
    /// song with its own cut (an intro, an interlude). Nil: the same timeline.
    @ObservationIgnored private var timeMap: VideoVersionAlignment.Map?

    @ObservationIgnored let player = AVPlayer()
    @ObservationIgnored private var clock: VideoClock
    @ObservationIgnored private var timer: Timer?
    @ObservationIgnored private var statusObservation: NSKeyValueObservation?
    @ObservationIgnored private var failedObserver: NSObjectProtocol?
    /// The asset's resource loader holds its delegate weakly.
    @ObservationIgnored private var loader: ChunkedVideoLoader?
    @ObservationIgnored private var isSeeking = false
    @ObservationIgnored private var lastSeekAt: CFTimeInterval = 0
    /// How long a seek on this picture takes to land, averaged: the policy
    /// aims that far ahead of a playing engine.
    @ObservationIgnored private var seekLead: Double = 0.4
    @ObservationIgnored private var lastStatusLog: CFTimeInterval = 0

    /// - Parameter clock: read on the main actor, once per tick.
    init(clock: VideoClock) {
        self.clock = clock
        player.isMuted = true
        player.volume = 0
        player.preventsDisplaySleepDuringVideoPlayback = false
        player.audiovisualBackgroundPlaybackPolicy = .pauses
        // The picture never leaves for AirPlay: the engine owns the audio route,
        // and a muted picture on a TV must not look like a route change.
        player.allowsExternalPlayback = false
        player.actionAtItemEnd = .pause
        // The picture is not the point of the track: do not wait for a large
        // buffer before showing the frame the engine is already at.
        player.automaticallyWaitsToMinimizeStalling = false
    }

    deinit {
        timer?.invalidate()
        if let failedObserver {
            NotificationCenter.default.removeObserver(failedObserver)
        }
    }

    /// The clock can be supplied after construction (the surface owns the
    /// environment player); every surface passes the same closures.
    func setClock(_ clock: VideoClock) {
        self.clock = clock
        tick()
    }

    /// Attach the side for the current track, or nil to go back to the artwork.
    /// Attaching the same URL again is a no-op, so a view can call this on every
    /// body pass.
    func attach(_ side: ResolvedVideoSide?, timeMap: VideoVersionAlignment.Map? = nil) {
        guard let side, let url = side.url.flatMap(URL.init(string:)) else {
            detach()
            return
        }
        self.timeMap = timeMap
        if self.side?.url == side.url, player.currentItem != nil {
            // Keep the current item, including a failed one: a repeated body
            // pass must not rebuild (and retry) a video that already refused.
            self.side = side
            return
        }
        tearDownItem()
        self.side = side
        state = .loading
        load(side, url: url, segmented: true)
    }

    /// One item for the side. `segmented`: through the rendition's segment
    /// index as an HLS playlist, ready after its first fragment; otherwise
    /// the whole MP4 in bounded ranges, which AVFoundation reads to the end
    /// before it is ready (the fallback when the head has no index).
    private func load(_ side: ResolvedVideoSide, url: URL, segmented: Bool) {
        // Bounded ranges, with the headers the resolving client sent: the
        // player's own open-ended requests are slowed to playback speed.
        let loader = ChunkedVideoLoader(source: url, headers: side.headers)
        let options: [String: Any] = side.headers.isEmpty ? [:] : ["AVURLAssetHTTPHeaderFieldsKey": side.headers]
        let asset: AVURLAsset
        if segmented {
            asset = AVURLAsset(url: ChunkedVideoLoader.playlistURL, options: options)
        } else if let chunked = loader.assetURL {
            asset = AVURLAsset(url: chunked)
        } else {
            asset = AVURLAsset(url: url, options: options)
        }
        asset.resourceLoader.setDelegate(loader, queue: loader.queue)
        self.loader = loader
        let item = AVPlayerItem(asset: asset)
        let attachedAt = CACurrentMediaTime()
        PlaybackDebugLog.shared.record(
            "video sync: loading \(side.width ?? 0)x\(side.height ?? 0) itag \(side.itag ?? 0) via \(side.clientName) "
                + "\(segmented ? "segmented" : "whole"), "
                + "headers \(side.headers.keys.sorted().joined(separator: ",")), throttled=\(side.hasNParameter)"
        )
        // Install the observers before assigning the item: a cached item can be
        // ready immediately, and each callback checks item identity after hopping.
        statusObservation = item.observe(\.status, options: [.initial, .new]) { [weak self] item, _ in
            Task { @MainActor [weak self] in
                guard let self, self.player.currentItem === item else { return }
                switch item.status {
                case .readyToPlay:
                    PlaybackDebugLog.shared.record(String(
                        format: "video sync: ready in %.1fs", CACurrentMediaTime() - attachedAt
                    ))
                    self.state = .ready
                    self.lastSeekAt = 0
                    self.tick()
                case .failed:
                    PlaybackDebugLog.shared.record(
                        "video sync: the item failed: \(item.error.map { String(describing: $0) } ?? "no error")"
                    )
                    if segmented {
                        self.tearDownItem()
                        self.load(side, url: url, segmented: false)
                    } else {
                        self.fail()
                    }
                default:
                    break
                }
            }
        }
        failedObserver = NotificationCenter.default.addObserver(
            forName: .AVPlayerItemFailedToPlayToEndTime, object: item, queue: .main
        ) { [weak self] _ in
            Task { @MainActor [weak self] in
                guard let self, self.player.currentItem === item else { return }
                self.fail()
            }
        }
        player.replaceCurrentItem(with: item)
        startTimer()
        tick()
    }

    /// Detach the picture: stop, tear the item down, go back to idle/artwork.
    func detach() {
        tearDownItem()
        side = nil
        timeMap = nil
        state = .idle
    }

    /// One policy pass. Called by the timer; exposed so a surface can force a
    /// pass after a transport change instead of waiting for the next 250 ms.
    func tick() {
        if state != .idle, CACurrentMediaTime() - lastStatusLog >= 2,
           state != .ready || isSeeking || player.currentItem?.status != .readyToPlay {
            lastStatusLog = CACurrentMediaTime()
            PlaybackDebugLog.shared.record(
                "video sync: held (state=\(state) item=\(player.currentItem?.status.rawValue ?? -1) seeking=\(isSeeking))"
            )
        }
        guard state == .ready, let item = player.currentItem, item.status == .readyToPlay, !isSeeking else {
            return
        }
        // The engine's position is song time; the picture runs on its own
        // upload's timeline when the two cuts differ. Within a segment the map
        // has slope one, so the rate policy below is unchanged, and crossing
        // into the next segment is a drift past the threshold: one seek over
        // the interlude, aimed ahead as any other.
        let songTime = clock.position()
        let engine = timeMap.map { $0.videoTime(at: songTime) } ?? songTime
        let video = player.currentTime().seconds
        guard engine.isFinite, video.isFinite else { return }
        let now = CACurrentMediaTime()
        if now - lastStatusLog >= 2 {
            lastStatusLog = now
            PlaybackDebugLog.shared.record(String(
                format: "video sync: engine %.2f video %.2f playing=%@ rate %.2f control=%d waiting=%@ lead %.2f",
                engine, video, clock.isPlaying() ? "yes" : "no", player.rate, player.timeControlStatus.rawValue,
                player.reasonForWaitingToPlay?.rawValue ?? "-", seekLead
            ))
        }
        let decision = VideoSyncPolicy.decide(
            engineSeconds: engine,
            videoSeconds: video,
            enginePlaying: clock.isPlaying(),
            engineRate: clock.rate(),
            videoRate: player.rate,
            secondsSinceSeek: now - lastSeekAt,
            seekLead: seekLead
        )
        switch decision {
        case .none:
            break
        case .pause:
            player.pause()
        case .play(let rate):
            if abs(player.rate - rate) > 0.005 || player.rate == 0 {
                player.playImmediately(atRate: rate)
            }
        case .seek(let seconds):
            isSeeking = true
            lastSeekAt = now
            let time = CMTime(seconds: max(0, seconds), preferredTimescale: 600)
            let tolerance = CMTime(seconds: 0.05, preferredTimescale: 600)
            player.seek(to: time, toleranceBefore: tolerance, toleranceAfter: tolerance) { [weak self] finished in
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    self.isSeeking = false
                    if finished {
                        let took = CACurrentMediaTime() - now
                        self.seekLead = min(2, max(0.05, self.seekLead * 0.5 + took * 0.5))
                    }
                    self.tick()
                }
            }
        }
    }

    private func fail() {
        state = .failed
        player.pause()
    }

    private func startTimer() {
        guard timer == nil else { return }
        let timer = Timer(timeInterval: 0.25, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in self?.tick() }
        }
        // Common mode keeps the clock running while a list is being scrolled.
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    private func tearDownItem() {
        statusObservation = nil
        if let failedObserver {
            NotificationCenter.default.removeObserver(failedObserver)
        }
        failedObserver = nil
        timer?.invalidate()
        timer = nil
        player.pause()
        player.replaceCurrentItem(with: nil)
        loader = nil
        isSeeking = false
        lastSeekAt = 0
    }
}
