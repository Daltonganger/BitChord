import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
import AVKit
import CoreImage
import CoreImage.CIFilterBuiltins
#if os(iOS)
import UIKit
import AVFAudio
#else
import AppKit
#endif

/// Upstream `MeshGradientBackground`: four luminous radial blobs sampled from
/// the sleeve, blurred into a wash. Not SwiftUI `MeshGradient` — that warps a
/// vertex grid and is what tore the artwork's edges. Blobs drift once on a
/// track change, then rest.
struct MeshBackdrop: View {
    var seed: Int
    var artwork: Data? = nil

    @State private var phase: Double = 0
    @State private var base: Color = .black
    @State private var blob0 = Color.clear
    @State private var blob1 = Color.clear
    @State private var blob2 = Color.clear
    @State private var blob3 = Color.clear

    private var reduceBlur: Bool {
        PlatformSettings.shared.getBoolean(key: "reduce_dynamic_blur", default: false)
    }
    private var reduceAnimation: Bool {
        PlatformSettings.shared.getBoolean(key: "reduce_animation", default: false)
    }

    var body: some View {
        Group {
            if legacyMesh {
                // v1.5's backdrop behind its switch: one static wash, no
                // animated blobs. The colour still follows the artwork; only
                // the motion is gone.
                base
                    .overlay {
                        LinearGradient(
                            colors: [Color.black.opacity(0.10), Color.black.opacity(0.38)],
                            startPoint: .top,
                            endPoint: .bottom
                        )
                    }
                    .ignoresSafeArea()
                    .allowsHitTesting(false)
                    .onAppear { base = ArtworkPalette.meshBlobs(from: artwork, seed: seed).base }
                    .onChange(of: seed) { _, new in
                        base = ArtworkPalette.meshBlobs(from: artwork, seed: new).base
                    }
                    .onChange(of: artwork) { _, data in
                        base = ArtworkPalette.meshBlobs(from: data, seed: seed).base
                    }
            } else {
                animatedMesh
            }
        }
    }

    private var legacyMesh: Bool {
        PlatformSettings.shared.getBoolean(key: "legacy_mesh_gradient", default: false)
    }

    private var animatedMesh: some View {
        let blobs = [blob0, blob1, blob2, blob3]
        return Canvas { context, size in
            let anchors: [(CGFloat, CGFloat)] = [
                (0.20, 0.25), (0.80, 0.20), (0.75, 0.80), (0.25, 0.75),
            ]
            let speeds: [Double] = [1, -0.7, 0.85, -1.15]
            let radius = max(size.width, size.height) * 0.62
            for i in 0..<4 {
                let color = blobs[i]
                let x = (anchors[i].0 + 0.16 * cos(phase * speeds[i] + Double(i) * 1.7)) * size.width
                let y = (anchors[i].1 + 0.16 * sin(phase * speeds[i] * 0.9 + Double(i) * 2.3)) * size.height
                let rect = CGRect(x: x - radius, y: y - radius, width: radius * 2, height: radius * 2)
                context.fill(
                    Path(ellipseIn: rect),
                    with: .radialGradient(
                        Gradient(colors: [color.opacity(0.85), color.opacity(0)]),
                        center: CGPoint(x: x, y: y),
                        startRadius: 0,
                        endRadius: radius
                    )
                )
            }
            context.fill(
                Path(CGRect(origin: .zero, size: size)),
                with: .linearGradient(
                    Gradient(colors: [Color.black.opacity(0.10), Color.black.opacity(0.38)]),
                    startPoint: .zero,
                    endPoint: CGPoint(x: 0, y: size.height)
                )
            )
        }
        .background(base)
        .blur(radius: reduceBlur ? 0 : 64)
        .scaleEffect(1.3)
        .clipped()
        .ignoresSafeArea()
        .allowsHitTesting(false)
        .onAppear { settle(seed: seed, artwork: artwork, first: true) }
        .onChange(of: seed) { _, new in settle(seed: new, artwork: artwork, first: false) }
        .onChange(of: artwork) { _, data in apply(ArtworkPalette.meshBlobs(from: data, seed: seed), animated: true) }
    }

    private func settle(seed: Int, artwork: Data?, first: Bool) {
        apply(ArtworkPalette.meshBlobs(from: artwork, seed: seed), animated: !first)
        guard !reduceAnimation else { return }
        withAnimation(.timingCurve(0.4, 0.0, 0.2, 1.0, duration: 8)) {
            phase += .pi * 0.45
        }
    }

    private func apply(_ mesh: ArtworkPalette.MeshBlobs, animated: Bool) {
        let run = {
            base = mesh.base
            blob0 = mesh.blobs[0]
            blob1 = mesh.blobs[1]
            blob2 = mesh.blobs[2]
            blob3 = mesh.blobs[3]
        }
        if animated && !reduceAnimation {
            withAnimation(.easeInOut(duration: 1.4), run)
        } else {
            run()
        }
    }
}
