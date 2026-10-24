import AVFoundation
import Observation
import SwiftUI

/// App-wide owner of the O9 video side.
///
/// One muted `AVPlayer` feeds every visible surface, and a video-side request
/// exists only while **all** of these are true: the "Show Music Videos" switch
/// is on, at least one player surface is on screen, the app is active, and a
/// track with a videoId is playing. Nothing is prefetched for the queue: the
/// switch going on does not start a request until a surface registers.
@MainActor
@Observable
final class VideoPlayback {
    static let shared = VideoPlayback()

    private(set) var side: ResolvedVideoSide?
    let controller = VideoSideController(clock: VideoClock(position: { 0 }, isPlaying: { false }))

    @ObservationIgnored private var enabled = false
    @ObservationIgnored private var surfaces = 0
    @ObservationIgnored private var active = true
    @ObservationIgnored private var trackId: String?
    @ObservationIgnored private var mountedId: String?
    @ObservationIgnored private var resolvedId: String?
    @ObservationIgnored private var retriedId: String?
    @ObservationIgnored private var resolveTask: Task<Void, Never>?

    private init() {}

    var player: AVPlayer { controller.player }
    var state: VideoSideController.State { controller.state }
    /// Whether a surface should draw the picture instead of the artwork.
    var showsVideo: Bool { side != nil && controller.state == .ready }

    /// The clock is the audio player's; every surface passes the same closures
    /// because there is exactly one audio player.
    func configure(clock: VideoClock) {
        controller.setClock(clock)
    }

    func setEnabled(_ on: Bool) {
        guard enabled != on else { return }
        enabled = on
        reconcile()
    }

    func setSurfaceVisible(_ visible: Bool, videoId: String?, active: Bool) {
        if visible {
            surfaces += 1
            if let videoId { trackId = videoId }
        } else {
            surfaces = max(0, surfaces - 1)
        }
        self.active = active
        reconcile()
    }

    func setTrack(_ videoId: String?, active: Bool) {
        trackId = videoId
        self.active = active
        reconcile()
    }

    func setActive(_ active: Bool) {
        self.active = active
        reconcile()
    }

    /// A surface asked to show a track: resolve only the one it needs, and only
    /// once. A nil side (an art track) or a failure is remembered for this
    /// track, so a body pass cannot turn into a request loop.
    private func reconcile() {
        guard enabled, surfaces > 0, active, let id = trackId, !id.isEmpty else {
            stop()
            return
        }
        if mountedId == id, resolvedId == id { return }
        if mountedId == id, resolveTask != nil { return }
        start(id)
    }

    private func start(_ id: String, forceRefresh: Bool = false) {
        if mountedId != id { retriedId = nil }
        resolveTask?.cancel()
        resolveTask = nil
        mountedId = id
        resolvedId = nil
        side = nil
        controller.detach()

        resolveTask = Task { [weak self] in
            guard let self else { return }
            do {
                let side = try await InnertubeStreamResolver.shared.resolveVideo(videoId: id, forceRefresh: forceRefresh)
                guard !Task.isCancelled, self.mountedId == id,
                      self.enabled, self.surfaces > 0, self.active else { return }
                self.resolvedId = id
                self.resolveTask = nil
                // The audit's art-track guard: a square picture is the sleeve,
                // not a music video, even when it happens to be AVC/MP4.
                if side.url != nil, side.looksLikeMusicVideo {
                    self.side = side
                    self.controller.attach(side)
                } else {
                    self.side = nil
                    self.controller.detach()
                }
            } catch {
                guard !Task.isCancelled, self.mountedId == id else { return }
                self.resolvedId = id
                self.resolveTask = nil
                self.side = nil
                self.controller.detach()
            }
        }
    }

    /// The picture failed (usually an expired signed URL): ask once more,
    /// bypassing the shared cache, then let the artwork stay.
    func videoFailed() {
        guard enabled, surfaces > 0, active,
              let id = trackId, mountedId == id, resolvedId == id,
              retriedId != id else { return }
        retriedId = id
        start(id, forceRefresh: true)
    }

    private func stop() {
        resolveTask?.cancel()
        resolveTask = nil
        mountedId = nil
        resolvedId = nil
        side = nil
        controller.detach()
    }
}

/// One place the picture can appear. The fallback is what the surface shows
/// when the switch is off, the track has no video side, or the picture failed;
/// the video is an overlay on top, never a replacement for the artwork view
/// that already owns its corner, shadow and tap behaviour.
struct VideoSurface<Fallback: View>: View {
    var cornerRadius: CGFloat = 12
    /// Fill the surface's box with black behind a 16:9 picture. False for a
    /// full-bleed hero, where the artwork must stay visible around the picture.
    var fillSlot = true
    @ViewBuilder var fallback: () -> Fallback

    @Environment(PlaybackController.self) private var controller
    @Environment(\.scenePhase) private var scenePhase
    @AppStorage("show_music_videos") private var showMusicVideos = false
    @State private var registered = false

    var body: some View {
        ZStack {
            fallback()
            if VideoPlayback.shared.showsVideo {
                if fillSlot { Color.black }
                VideoLayerView(player: VideoPlayback.shared.player)
                    .aspectRatio(16.0 / 9.0, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
                    .allowsHitTesting(false)
                    .transition(.opacity)
            }
        }
        .animation(.easeInOut(duration: 0.25), value: VideoPlayback.shared.showsVideo)
        .onAppear {
            guard !registered else { return }
            registered = true
            VideoPlayback.shared.configure(clock: VideoClock(
                position: { controller.livePosition(at: Date()) },
                isPlaying: { controller.isPlaying },
                rate: { controller.effectivePlaybackRate }
            ))
            VideoPlayback.shared.setEnabled(showMusicVideos)
            VideoPlayback.shared.setSurfaceVisible(
                true, videoId: controller.current?.videoId, active: scenePhase == .active
            )
        }
        .onDisappear {
            guard registered else { return }
            registered = false
            VideoPlayback.shared.setSurfaceVisible(false, videoId: nil, active: false)
        }
        .onChange(of: controller.current?.videoId) { _, id in
            guard registered else { return }
            VideoPlayback.shared.setTrack(id, active: scenePhase == .active)
        }
        .onChange(of: scenePhase) { _, phase in
            guard registered else { return }
            VideoPlayback.shared.setActive(phase == .active)
        }
        .onChange(of: showMusicVideos) { _, on in
            guard registered else { return }
            VideoPlayback.shared.setEnabled(on)
        }
        .onChange(of: VideoPlayback.shared.state) { _, state in
            guard registered, state == .failed else { return }
            VideoPlayback.shared.videoFailed()
        }
    }
}

#if os(iOS)
private struct VideoLayerView: UIViewRepresentable {
    let player: AVPlayer
    func makeUIView(context: Context) -> VideoLayerHost {
        let view = VideoLayerHost()
        view.player = player
        return view
    }
    func updateUIView(_ view: VideoLayerHost, context: Context) {
        view.player = player
    }
}

private final class VideoLayerHost: UIView {
    override class var layerClass: AnyClass { AVPlayerLayer.self }
    private var playerLayer: AVPlayerLayer { layer as! AVPlayerLayer }

    var player: AVPlayer? {
        get { playerLayer.player }
        set {
            playerLayer.player = newValue
            playerLayer.videoGravity = .resizeAspect
        }
    }
}
#else
private struct VideoLayerView: NSViewRepresentable {
    let player: AVPlayer
    func makeNSView(context: Context) -> VideoLayerHost {
        let view = VideoLayerHost()
        view.player = player
        return view
    }
    func updateNSView(_ view: VideoLayerHost, context: Context) {
        view.player = player
    }
}

private final class VideoLayerHost: NSView {
    private let playerLayer = AVPlayerLayer()

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        playerLayer.videoGravity = .resizeAspect
        layer?.addSublayer(playerLayer)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override func layout() {
        super.layout()
        playerLayer.frame = bounds
    }

    var player: AVPlayer? {
        get { playerLayer.player }
        set { playerLayer.player = newValue }
    }
}
#endif
