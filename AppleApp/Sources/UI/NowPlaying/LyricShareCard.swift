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

/// Upstream's lyrics strip: the current line is bright and sharp; neighbours
/// recede by opacity, the same treatment as the Android panel.
/// The picture a shared lyric becomes: the sleeve's colours, the quoted lines
/// and the song they came from.
///
/// A view rather than a drawn image so `ImageRenderer` can produce one at the
/// display's own scale — the card is rendered the way it is laid out here, which
/// is also what makes it previewable in the canvas.
struct LyricShareCard: View {
    /// One row of the card. A picked lyric, or the mark that says a passage
    /// was skipped between two picks.
    struct Line {
        var text: String
        /// The romanization or translation drawn small underneath, when the
        /// line has one that differs (upstream's `LyricsShareLine.subText`).
        var subText: String? = nil
        var isGap: Bool = false
    }

    let lines: [Line]
    let title: String
    let artist: String
    let tint: ArtworkPalette.PageTint
    let artwork: Data?
    let artworkUrl: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            HStack(spacing: 12) {
                ArtworkView(url: artworkUrl, data: artwork, side: 56)
                    .clipShape(.rect(cornerRadius: 10, style: .continuous))
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(.headline)
                        .foregroundStyle(.white)
                        .lineLimit(1)
                    Text(artist)
                        .font(.subheadline)
                        .foregroundStyle(.white.opacity(0.7))
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
            }

            VStack(alignment: .leading, spacing: 10) {
                ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                    if line.isGap {
                        // The skipped passage, as the conventional editorial
                        // omission mark — smaller and dimmer than a lyric line,
                        // so the quote says the verse was cut here rather than
                        // letting two distant halves read as if they ran on
                        // (upstream LyricsShareCard.kt:190).
                        Text("(…)")
                            .font(.title3.weight(.semibold))
                            .foregroundStyle(.white.opacity(0.4))
                    } else {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(line.text)
                                .font(.title2.weight(.semibold))
                                .foregroundStyle(.white)
                                .fixedSize(horizontal: false, vertical: true)
                            if let sub = line.subText {
                                Text(sub)
                                    .font(.subheadline.weight(.medium))
                                    .foregroundStyle(.white.opacity(0.72))
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                        }
                    }
                }
            }

            Spacer(minLength: 0)

            HStack {
                Text("BitChord")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.white.opacity(0.55))
                Spacer(minLength: 8)
                Text(artist)
                    .font(.caption)
                    .foregroundStyle(.white.opacity(0.55))
                    .lineLimit(1)
            }
        }
        .padding(28)
        .frame(width: 560, height: 700, alignment: .topLeading)
        .background {
            ZStack {
                tint.wash
                LinearGradient(
                    colors: [tint.accent.opacity(0.38), tint.wash, .black.opacity(0.86)],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )
            }
        }
    }
}
