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

/// Square sleeve: still art, motion canvas cropped to fill, Apple Music pause
/// shrink inside a fixed slot so the column does not reflow. Used when the
/// full-bleed banner is off.
struct SleeveArt: View {
    var entry: QueueEntry?
    var canvasURL: URL?
    var fallbackURL: URL? = nil
    var isPlaying: Bool
    var side: CGFloat

    var body: some View {
        ZStack {
            ArtworkView(entry: entry, side: side)
            if let canvasURL {
                CanvasPlayer(url: canvasURL, fallbackURL: fallbackURL, isPlaying: isPlaying)
            }
        }
        .frame(width: side, height: side)
        .clipShape(.rect(cornerRadius: 12, style: .continuous))
        .shadow(color: .black.opacity(0.45), radius: 24, y: 10)
        .scaleEffect(isPlaying ? 1 : 0.86)
        .animation(.spring(response: 0.85, dampingFraction: 0.75), value: isPlaying)
        .frame(width: side, height: side)
    }
}

/// Upstream's full-bleed banner (`NowPlayingScreen` heroMode): the cover is
/// cropped to fill, and the bottom 42% dissolves into the mesh. That dissolve
/// is the artwork "effect" — not a warp of the pixels. A motion clip, when
/// one exists, plays in the same frame with the same mask.
struct HeroArtwork: View {
    var entry: QueueEntry?
    var canvasURL: URL?
    var fallbackURL: URL? = nil
    var isPlaying: Bool

    private let fadeFraction: CGFloat = 0.42

    var body: some View {
        GeometryReader { geo in
            ZStack {
                ArtworkView(entry: entry, side: max(geo.size.width, geo.size.height))
                    .frame(width: geo.size.width, height: geo.size.height)
                    .clipped()
                if let canvasURL {
                    CanvasPlayer(url: canvasURL, fallbackURL: fallbackURL, isPlaying: isPlaying)
                        .frame(width: geo.size.width, height: geo.size.height)
                }
                // The picture takes the artwork's place edge to edge, cropped
                // to the hero and dissolving under the same mask: a 16:9 band
                // across the middle left the cover showing above and below.
                VideoSurface(cornerRadius: 0, fillSlot: false, announcesLoading: true, presentation: .fill) { Color.clear }
            }
            .compositingGroup()
            .mask {
                LinearGradient(
                    stops: [
                        .init(color: .white, location: 0),
                        .init(color: .white, location: 1 - fadeFraction),
                        .init(color: .clear, location: 1),
                    ],
                    startPoint: .top,
                    endPoint: .bottom
                )
            }
        }
        .aspectRatio(1, contentMode: .fit)
        .allowsHitTesting(false)
    }
}

/// Upstream ArtworkMeshBackdrop: preserve the cover's bottom row above the
/// seam, then stretch its inverted, horizontally shifted colour grid below it.
struct ArtworkContinuation: View {
    var entry: QueueEntry?
    var seam: CGFloat
    @State private var texture: CGImage?

    var body: some View {
        GeometryReader { geo in
            if let texture, let edge = texture.cropping(to: CGRect(x: 0, y: 0, width: texture.width, height: 1)) {
                VStack(spacing: 0) {
                    Image(decorative: edge, scale: 1).resizable()
                        .frame(height: min(seam, geo.size.height))
                    Image(decorative: texture, scale: 1).resizable()
                        .frame(height: max(0, geo.size.height - seam))
                }
                .frame(width: geo.size.width)
            }
        }
        .allowsHitTesting(false)
        .task(id: entry?.thumbnailUrl ?? entry?.id) {
            guard let entry else { return }
            let source: PlatformImage?
            if let data = entry.artworkData {
                source = PlatformImage(data: data)
            } else if let url = entry.thumbnailUrl {
                source = await ArtworkCache.shared.load(SharedArtwork.sized(url, 120) ?? url)
            } else { source = nil }
            guard let source else { return }
            let key = entry.id
            let result = await Task.detached(priority: .utility) {
                Self.makeTexture(source, key: key)
            }.value
            guard !Task.isCancelled else { return }
            texture = result
        }
    }

    nonisolated private static func makeTexture(_ image: PlatformImage, key: String) -> CGImage? {
        #if os(iOS)
        guard let image = image.cgImage else { return nil }
        #else
        var rect = CGRect(origin: .zero, size: image.size)
        guard let image = image.cgImage(forProposedRect: &rect, context: nil, hints: nil) else { return nil }
        #endif
        let n = 120, gridSize = 6, size = 32
        var pixels = [UInt8](repeating: 0, count: n * n * 4)
        let colorSpace = CGColorSpaceCreateDeviceRGB()
        let info = CGImageAlphaInfo.premultipliedLast.rawValue
        let drawn = pixels.withUnsafeMutableBytes { bytes -> Bool in
            guard let ctx = CGContext(data: bytes.baseAddress, width: n, height: n,
                bitsPerComponent: 8, bytesPerRow: n * 4, space: colorSpace, bitmapInfo: info) else { return false }
            ctx.draw(image, in: CGRect(x: 0, y: 0, width: n, height: n))
            return true
        }
        guard drawn else { return nil }
        var grid = [[Double]](repeating: [0, 0, 0], count: 36)
        for y in 0..<n {
            for x in 0..<n {
                let cell = ((n - 1 - y) / 20) * gridSize + x / 20
                for c in 0..<3 { grid[cell][c] += Double(pixels[(y * n + x) * 4 + c]) / 400 }
            }
        }
        // Upstream's small HSL saturation lift and near-black lightness floor.
        for i in grid.indices {
            let rgb = grid[i].map { $0 / 255 }
            let hi = rgb.max()!, lo = rgb.min()!
            let light = (hi + lo) / 2, chroma = hi - lo
            let liftedLight = max(0.045, light)
            let saturation = chroma > 0 ? chroma / max(0.000001, 1 - abs(2 * light - 1)) : 0
            let liftedChroma = (1 - abs(2 * liftedLight - 1)) * min(1, saturation * 1.12)
            grid[i] = rgb.map { value in
                let lifted = liftedLight + (chroma > 0 ? (value - light) * liftedChroma / chroma : 0)
                return min(255, max(0, lifted * 255))
            }
        }
        // Stable per cover; keep neighbouring cells together and the seam intact.
        let seed = key.utf8.reduce(UInt32(0)) { ($0 &* 31) &+ UInt32($1) }
        let shift = Int(seed % 6), mirror = seed & 1 == 1
        var rotated = grid
        for y in 1..<gridSize {
            for x in 0..<gridSize {
                rotated[y * gridSize + x] = grid[y * gridSize + ((mirror ? 5 - x : x) + shift) % gridSize]
            }
        }
        func smooth(_ v: Double) -> Double {
            let t = min(1, max(0, v)); return t * t * (3 - 2 * t)
        }
        var out = [UInt8](repeating: 255, count: size * size * 4)
        for y in 0..<size {
            let fy = (Double(y) + 0.5) / Double(size) * 6 - 0.5
            let y0 = min(5, max(0, Int(floor(fy)))), y1 = min(5, y0 + 1)
            let wy = smooth(fy - Double(y0))
            for x in 0..<size {
                let fx = (Double(x) + 0.5) / Double(size) * 6 - 0.5
                let x0 = min(5, max(0, Int(floor(fx)))), x1 = min(5, x0 + 1)
                let wx = smooth(fx - Double(x0))
                for c in 0..<3 {
                    let top = rotated[y0 * 6 + x0][c] * (1 - wx) + rotated[y0 * 6 + x1][c] * wx
                    let bottom = rotated[y1 * 6 + x0][c] * (1 - wx) + rotated[y1 * 6 + x1][c] * wx
                    out[(y * size + x) * 4 + c] = UInt8(clamping: Int((top * (1 - wy) + bottom * wy).rounded()))
                }
            }
        }
        guard let provider = CGDataProvider(data: Data(out) as CFData) else { return nil }
        return CGImage(width: size, height: size, bitsPerComponent: 8, bitsPerPixel: 32,
            bytesPerRow: size * 4, space: colorSpace, bitmapInfo: CGBitmapInfo(rawValue: info),
            provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent)
    }
}

/// Upstream's `FullArtworkBlurBackdrop` for lyrics, queue, landscape and iPad:
/// a cropped copy of the cover blurred once at thumbnail resolution, then
/// scaled across the screen under a dark readability wash.
struct FullArtworkBlurBackdrop: View {
    let entry: QueueEntry?
    @State private var image: PlatformImage?

    private var cacheKey: String? {
        guard let entry else { return nil }
        return entry.thumbnailUrl ?? "embedded:\(entry.id)"
    }

    var body: some View {
        GeometryReader { geometry in
            ZStack {
                if let image {
                    #if os(iOS)
                    Image(uiImage: image)
                        .resizable()
                        .scaledToFill()
                        .frame(width: geometry.size.width, height: geometry.size.height)
                        .clipped()
                    #else
                    Image(nsImage: image)
                        .resizable()
                        .scaledToFill()
                        .frame(width: geometry.size.width, height: geometry.size.height)
                        .clipped()
                    #endif
                    LinearGradient(
                        stops: [
                            .init(color: .black.opacity(0.34), location: 0),
                            .init(color: .black.opacity(0.48), location: 0.55),
                            .init(color: .black.opacity(0.64), location: 1),
                        ],
                        startPoint: .top,
                        endPoint: .bottom
                    )
                }
            }
            .opacity(image == nil ? 0 : 1)
            .animation(.easeInOut(duration: 0.32), value: image != nil)
        }
        .task(id: cacheKey) {
            guard let entry, let cacheKey else {
                image = nil
                return
            }
            let prepared = await PlayerArtworkBlurCache.image(
                key: cacheKey,
                url: entry.thumbnailUrl,
                data: entry.artworkData
            )
            guard !Task.isCancelled else { return }
            image = prepared
        }
        .accessibilityHidden(true)
    }
}

/// Small, pre-blurred cover copies are cached just like upstream's 128px
/// bitmap. The expensive Core Image work runs away from the main thread.
private enum PlayerArtworkBlurCache {
    private static let images = NSCache<NSString, PlatformImage>()
    private static let context = CIContext(options: [.cacheIntermediates: false])
    private static let pixelLimit: CGFloat = 256

    static func image(key: String, url: String?, data: Data?) async -> PlatformImage? {
        if let cached = images.object(forKey: key as NSString) { return cached }
        let source: PlatformImage?
        if let data {
            source = PlatformImage(data: data)
        } else if let url {
            source = await ArtworkCache.shared.load(SharedArtwork.sized(url, 256) ?? url)
        } else {
            source = nil
        }
        guard let source else { return nil }
        let prepared = await Task.detached(priority: .utility) {
            renderBlurred(source)
        }.value
        if let prepared { images.setObject(prepared, forKey: key as NSString) }
        return prepared
    }

    private static func renderBlurred(_ source: PlatformImage) -> PlatformImage? {
        #if os(iOS)
        guard let cgImage = source.cgImage else { return nil }
        #else
        var proposed = CGRect(origin: .zero, size: source.size)
        guard let cgImage = source.cgImage(forProposedRect: &proposed, context: nil, hints: nil) else {
            return nil
        }
        #endif

        let input = CIImage(cgImage: cgImage)
        let sourceExtent = input.extent
        let scale = min(1, pixelLimit / max(sourceExtent.width, sourceExtent.height))
        let scaled = input.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let filter = CIFilter.gaussianBlur()
        filter.inputImage = scaled.clampedToExtent()
        filter.radius = 22
        guard let output = filter.outputImage?.cropped(to: scaled.extent),
              let blurred = context.createCGImage(output, from: scaled.extent)
        else { return nil }
        #if os(iOS)
        return UIImage(cgImage: blurred)
        #else
        return NSImage(cgImage: blurred, size: .zero)
        #endif
    }
}
