import SwiftUI

/// Full-screen picture presentation. Lyrics remain in the player's existing
/// pane; opening the theater never changes which pane the listener chose.
struct VideoTheaterView: View {
    let controller: PlaybackController

    @Environment(\.dismiss) private var dismiss
    @State private var controlsVisible = true
    @State private var controlActivity = 0

    private var duration: Double {
        let value = controller.duration
        return value.isFinite && value > 0 ? value : 1
    }

    private var position: Double {
        min(max(controller.livePosition(at: Date()), 0), duration)
    }

    private var seekPosition: Binding<Double> {
        Binding(
            get: { position },
            set: { controller.seek(to: $0); revealControls() }
        )
    }

    var body: some View {
        GeometryReader { geometry in
            ZStack {
                Color.black
                VideoLayerView(player: VideoPlayback.shared.player)
                    .frame(width: geometry.size.width, height: geometry.size.height)
                    .contentShape(Rectangle())
                    .onTapGesture { revealControls() }

                VStack(spacing: 0) {
                    HStack {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(controller.current?.title ?? "Music Video")
                                .font(.headline)
                                .lineLimit(1)
                            Text(controller.current?.artist ?? "")
                                .font(.subheadline)
                                .foregroundStyle(.white.opacity(0.75))
                                .lineLimit(1)
                        }
                        Spacer()
                        Button {
                            dismiss()
                        } label: {
                            Image(systemName: "xmark")
                                .font(.headline.weight(.semibold))
                                .frame(width: 40, height: 40)
                                .background(.ultraThinMaterial, in: Circle())
                        }
                        .accessibilityLabel("Close Video Theater")
                    }

                    Spacer()

                    VStack(spacing: 12) {
                        HStack(spacing: 36) {
                            Button {
                                controller.seek(to: max(0, position - 10))
                                revealControls()
                            } label: {
                                Image(systemName: "gobackward.10")
                            }
                            .accessibilityLabel("Back 10 seconds")

                            Button {
                                controller.togglePlayPause()
                                revealControls()
                            } label: {
                                Image(systemName: controller.isPlaying ? "pause.fill" : "play.fill")
                                    .font(.title2)
                                    .frame(width: 48, height: 48)
                                    .background(.ultraThinMaterial, in: Circle())
                            }
                            .accessibilityLabel(controller.isPlaying ? "Pause" : "Play")

                            Button {
                                controller.seek(to: min(duration, position + 10))
                                revealControls()
                            } label: {
                                Image(systemName: "goforward.10")
                            }
                            .accessibilityLabel("Forward 10 seconds")
                        }

                        HStack(spacing: 10) {
                            Text(timeLabel(position))
                                .monospacedDigit()
                            Slider(value: seekPosition, in: 0...duration)
                            Text(timeLabel(duration))
                                .monospacedDigit()
                        }
                        .font(.caption)
                    }
                }
                .foregroundStyle(.white)
                .padding(24)
                .background {
                    LinearGradient(
                        colors: [.black.opacity(0.56), .clear, .black.opacity(0.76)],
                        startPoint: .top,
                        endPoint: .bottom
                    )
                    .ignoresSafeArea()
                }
                .opacity(controlsVisible ? 1 : 0)
                .allowsHitTesting(controlsVisible)
                .animation(.easeOut(duration: 0.22), value: controlsVisible)
            }
            .frame(width: geometry.size.width, height: geometry.size.height)
            .ignoresSafeArea()
        }
        .background(.black)
        .onAppear {
            VideoPlayback.shared.setSurfaceVisible(true, videoId: controller.current?.videoId)
        }
        .onDisappear {
            VideoPlayback.shared.setSurfaceVisible(false, videoId: nil)
        }
        .task(id: controlActivity) {
            guard controlsVisible else { return }
            do {
                try await Task.sleep(for: .seconds(3))
            } catch {
                return
            }
            guard !Task.isCancelled else { return }
            controlsVisible = false
        }
    }

    private func revealControls() {
        controlsVisible = true
        controlActivity += 1
    }

    private func timeLabel(_ seconds: Double) -> String {
        let value = max(0, Int(seconds.rounded(.down)))
        return "\(value / 60):\(String(format: "%02d", value % 60))"
    }
}
