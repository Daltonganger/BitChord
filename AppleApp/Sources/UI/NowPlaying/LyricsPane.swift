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

struct LyricsPane: View {
    var lines: [LyricLineDto]
    /// The lyric before translation, when the caller has it. The pane shows
    /// the translation in place; the share card needs both, original above and
    /// translation underneath.
    var originalLines: [LyricLineDto]? = nil
    var loading: Bool
    var position: Double
    var hasTrack: Bool
    var sourceLabel: String? = nil
    var alignmentInProgress: Bool = false
    /// The word timings were added by this app's on-device alignment, not
    /// supplied by the provider — the attribution says so.
    var alignedLocally: Bool = false
    /// Opens the per-track provider picker — shown with the no-lyrics state,
    /// where it is most needed and previously unreachable.
    var onChooseSource: (() -> Void)? = nil
    var onSeek: ((Double) -> Void)? = nil
    /// Absent where the caller has no track to translate, which is the case that
    /// matters: an empty lyric has nothing to translate and offering the control
    /// anyway is an invitation to a request that cannot succeed.
    var translator: LyricsTranslator?
    var trackId: String = ""
    /// The listener's timing correction, in milliseconds. See [LyricsOffsetBridge].
    var offsetMs: Int32 = 0
    var sourceVisible: Bool = true
    /// True while the header's share entry has picking armed. The pane shows
    /// the quote bar for an empty pick so there is somewhere to aim, and
    /// reports back so the header can stop being the way out.
    var sharePicking: Bool = false
    var onSharePicking: ((Bool) -> Void)? = nil
    var onChangeSource: (() -> Void)? = nil
    var onRevealControls: (() -> Void)? = nil
    var onFocusLyrics: (() -> Void)? = nil
    /// Whether lines are being picked for a share card: armed from the header,
    /// or merely because something is already picked. While it holds, a line
    /// tap toggles the pick instead of seeking.
    var picking: Bool { sharePicking || !selection.isEmpty }
    /// What the share card names. The pane draws lines; the card has to say
    /// whose song they are, and only the caller knows.
    var trackTitle: String = ""
    var trackArtist: String = ""
    var artworkData: Data? = nil
    var artworkUrl: String? = nil
    /// Lyric lines picked for a share card, at most [maxSharedLines].
    @State private var selection: Set<Int> = []
    @State private var shareImage: Image?
    /// True while the listener is scrolling the list themselves, which stands the
    /// auto-scroll down until they have been still for a moment.
    @State private var reading = false
    /// A share card is a quote, not a transcript: four lines read as a lyric,
    /// twenty read as a wall.
    private let maxSharedLines = 4

    /// The clock the lyrics are judged against, which runs `offsetMs` behind the
    /// transport.
    ///
    /// Upstream's `adjustedLyricsPosition`, and the offset is applied *here*
    /// rather than by rewriting every line's timestamp on the way in. That is the
    /// whole design: the timings a source supplied stay exactly as supplied, so
    /// the transcript is the real one and a later fix to the offset is a change
    /// to one number rather than a re-fetch. A positive offset makes the adjusted
    /// clock smaller, so a line is reached later — which is what "positive shows
    /// lyrics later" means.
    private var adjustedPositionMs: Swift.Int64 {
        max(0, Swift.Int64(position * 1000) - Swift.Int64(offsetMs))
    }

    /// Every line being sung right now, which is usually one and is two across a
    /// duet.
    ///
    /// This used to be `lastIndex { $0.timeMs <= position }`, and that was wrong
    /// in a way nobody could report: a line that says when it ends lost its
    /// highlight the moment the *next* line's timestamp arrived, so the tail of a
    /// long line was never shown as sung — and a duet, where the answering vocal
    /// overlaps the lead, showed only one of the two. See [LyricFocus].
    private var activeRows: [Int] {
        // A Kotlin `List<Int>` arrives as `[KotlinInt]`, which Swift will not
        // index a `ForEach` with directly. Mapped here rather than by changing
        // the shared signature, because the shared side should say what it means
        // — a list of row numbers — and not what Swift can index.
        LyricFocus.shared
            .activeRows(lines: lines, positionMs: adjustedPositionMs)
            .map { Int(truncating: $0) }
    }

    /// Whether this document has real line or word timings. Plain-text lyric
    /// providers still have value; treating every line as an inactive timed row
    /// dimmed and blurred the entire transcript as if it were out of sync.
    private var hasSyncedTimings: Bool {
        LyricFocus.shared.isSynced(lines: lines)
    }

    /// Keep untimed rows crisp while incremental alignment is filling later
    /// lines in the same document.
    private func lineHasSyncedTimings(_ line: LyricLineDto, at index: Int) -> Bool {
        if line.timeMs > 0 || line.sungUntilMs != nil ||
            line.words.contains(where: { $0.startMs > 0 || $0.endMs > 0 }) ||
            (line.background?.timeMs ?? 0) > 0 ||
            line.background?.words.contains(where: { $0.startMs > 0 || $0.endMs > 0 }) == true
        {
            return true
        }
        // The first line may start at zero, which is also the DTO default for
        // unsynced text. A timed document gives that row a real zero timestamp.
        return index == 0 && hasSyncedTimings
    }

    /// The line the list scrolls to, and the one that is scaled up.
    private var leadIndex: Int {
        let lead = LyricFocus.shared.leadRow(lines: lines, positionMs: adjustedPositionMs)
        return lead < 0 ? 0 : min(Int(lead), max(0, lines.count - 1))
    }

    /// Where tapping a line should actually seek.
    ///
    /// The inverse of [adjustedPositionMs], and it has to be the inverse: a line
    /// whose nominal time is `t` is shown when the adjusted clock reaches `t`,
    /// which is transport position `t + offset`. Seeking to `t` instead would put
    /// the listener a line behind every time they tapped one.
    private func seekTarget(for line: LyricLineDto) -> Double {
        let ms = max(0, line.timeMs + Swift.Int64(offsetMs))
        return Double(ms) / 1000.0
    }

    var body: some View {
        Group {
            if !hasTrack {
                Text("Play a song to see lyrics here.")
                    .font(.title3)
                    .foregroundStyle(.white.opacity(0.6))
            } else if loading {
                ProgressView()
                    .controlSize(.regular)
                    .tint(.white)
            } else if lines.isEmpty {
                VStack(spacing: 14) {
                    Text("No lyrics for this track.")
                        .font(.title3)
                        .foregroundStyle(.white.opacity(0.6))
                    if let onChooseSource {
                        // Upstream's status row stays tappable when a track has
                        // no lyric: the sources that lost the race are still
                        // there, and "search again" can still answer.
                        Button("Choose a Source…") { onChooseSource() }
                            .buttonStyle(.bordered)
                            .tint(.white)
                    }
                }
            } else {
                VStack(alignment: .leading, spacing: 10) {
                    if let translator {
                        LyricsTranslationNote(outcome: translator.outcome)
                            .padding(.horizontal, 12)
                    }
                    ScrollViewReader { proxy in
                        ScrollView {
                            VStack(alignment: .leading, spacing: 18) {
                                ForEach(Array(lines.enumerated()), id: \.offset) { index, line in
                                    WordSyncedLine(
                                        line: line,
                                        hasSyncedTimings: lineHasSyncedTimings(line, at: index),
                                        active: activeRows.contains(index),
                                        distance: abs(index - leadIndex),
                                        // The *adjusted* clock, the one the line
                                        // focus above already uses. Handing the
                                        // sweep the raw transport meant the
                                        // offset moved the line but not the
                                        // bright word, so touching the offset
                                        // control — the one gesture meant to
                                        // fix sync — put the two out of step.
                                        position: Double(adjustedPositionMs) / 1000,
                                        onTap: {
                                            if !sourceVisible {
                                                onRevealControls?()
                                            } else if picking {
                                                toggleShareSelection(index)
                                            } else {
                                                onSeek?(seekTarget(for: line))
                                            }
                                        }
                                    )
                                    .overlay(alignment: .topTrailing) {
                                        if selection.contains(index) {
                                            Image(systemName: "checkmark.circle.fill")
                                                .foregroundStyle(.white)
                                                .padding(.top, 4)
                                                .accessibilityHidden(true)
                                        }
                                    }
                                    .contextMenu {
                                        Button(selection.contains(index) ? "Remove from Share" : "Add to Share") {
                                            toggleShareSelection(index)
                                        }
                                    }
                                    // While picking, the line is its own
                                    // toggle: a tap adds or removes it rather
                                    // than seeking, which is what the bar's
                                    // hint promises (upstream L14). With the
                                    // controls hidden and nothing being
                                    // picked, the tap still belongs to
                                    // revealing them.
                                    .accessibilityElement(children: .combine)
                                    .accessibilityAddTraits(picking && selection.contains(index) ? [.isSelected] : [])
                                    .accessibilityHint(picking ? "Double-tap to add to the share card" : "")
                                    .id(index)
                                }
                            }
                            .padding(.horizontal, 12)
                            .padding(.vertical, 20)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .scrollIndicators(.never)
                        // Following the *lead* line rather than the last one that
                        // has started: across a duet those differ, and scrolling
                        // to the answer would push the lead off the top while it
                        // was still the line being sung.
                        .onAppear {
                            proxy.scrollTo(leadIndex, anchor: .center)
                        }
                        .onChange(of: trackId) { _, _ in
                            reading = false
                            proxy.scrollTo(leadIndex, anchor: .center)
                        }
                        .onChange(of: leadIndex) { _, index in
                            // Not while the listener is reading. A list that
                            // snaps back to the playhead as soon as they let go
                            // is a list nobody can look anything up in.
                            guard !reading else { return }
                            withAnimation(.easeInOut(duration: 0.28)) {
                                proxy.scrollTo(index, anchor: .center)
                            }
                        }
                        // The reader's own scroll stands the auto-scroll down
                        // until they have been still for a moment. This is a
                        // separate gesture rather than a `DragGesture` on the
                        // ScrollView so it does not compete with the scroll
                        // itself — simultaneous, and only asked whether a finger
                        // is down.
                        .simultaneousGesture(
                            DragGesture(minimumDistance: 4)
                                .onChanged { value in
                                    reading = true
                                    // Advancing through the transcript (finger
                                    // moves up) gives the lyrics the whole view;
                                    // scrolling back reveals the deck again.
                                    if value.translation.height < -20, sourceVisible {
                                        onFocusLyrics?()
                                    } else if value.translation.height > 20, !sourceVisible {
                                        onRevealControls?()
                                    }
                                }
                                .onEnded { _ in
                                    withAnimation(.easeOut(duration: 1.6).delay(2.5)) {
                                        reading = false
                                    }
                                }
                        )
                    }
                    if !selection.isEmpty || sharePicking {
                        shareBar
                            .padding(.horizontal, 12)
                    }
                    if sourceVisible, let credit = attribution {
                        HStack(spacing: 8) {
                            Text(credit)
                                .font(.subheadline.weight(.medium))
                                .foregroundStyle(.white.opacity(0.72))
                            if let onChangeSource {
                                Button("Change", action: onChangeSource)
                                    .font(.subheadline.weight(.semibold))
                                    .underline()
                                    .foregroundStyle(.white)
                            }
                            Spacer(minLength: 0)
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 4)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        // A share card belongs to the song and the lyric it quoted. A new track,
        // or a different lyric source for the same one, makes the old card
        // describe something else — and it would still be sitting in the share
        // preview under the new song's name. The armed pick ends with them:
        // an armed bar that follows you into the next song is a trap.
        .onChange(of: trackId) { _, _ in
            clearShareSelection()
            onSharePicking?(false)
        }
        .onChange(of: lyricsSignature) { _, _ in
            clearShareSelection()
            onSharePicking?(false)
        }
        // Leaving pick mode — from the header's close or the bar's Done —
        // empties the pick with it. Arriving never has to: a fresh pick starts
        // from the same empty state these clearings leave behind.
        .onChange(of: sharePicking) { _, picking in
            if !picking { clearShareSelection() }
        }
    }

    /// What makes one lyric document different from another.
    ///
    /// A digest of every line, not of its count and ends: a re-fetch can return
    /// the same number of lines at the same timestamps with different words
    /// (a translation, a corrected transcript), and a signature that only looked
    /// at the shape would keep the old card. Hashing the document is O(lines) on
    /// a body that already lays every line out.
    private var lyricsSignature: Int {
        var hasher = Hasher()
        hasher.combine(sourceLabel)
        hasher.combine(lines.count)
        for line in lines {
            hasher.combine(line.timeMs)
            hasher.combine(line.text)
        }
        return hasher.finalize()
    }

    private func clearShareSelection() {
        guard !selection.isEmpty || shareImage != nil else { return }
        selection.removeAll()
        shareImage = nil
    }

    /// The quote bar: what has been picked, and the way to share it.
    private var shareBar: some View {
        HStack(spacing: 10) {
            Text(selection.count == 1 ? "1 line selected" : "\(selection.count) lines selected")
                .font(.subheadline.weight(.medium))
                .foregroundStyle(.white.opacity(0.8))
            Spacer(minLength: 0)
            if let shareImage {
                ShareLink(
                    item: shareImage,
                    preview: SharePreview(
                        trackTitle.isEmpty ? "Lyrics" : "\(trackTitle) — lyrics",
                        image: shareImage
                    )
                ) {
                    Label("Share", systemImage: "square.and.arrow.up")
                        .font(.subheadline.weight(.semibold))
                }
                .foregroundStyle(.white)
            } else if selection.isEmpty {
                // An armed pick with nothing in it yet: say what to do rather
                // than spin a card that does not exist.
                Text("Pick lines to share")
                    .font(.subheadline)
                    .foregroundStyle(.white.opacity(0.6))
            } else {
                ProgressView().controlSize(.small).tint(.white)
            }
            Button("Done") {
                selection.removeAll()
                shareImage = nil
                onSharePicking?(false)
            }
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(.white)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.white.opacity(0.10), in: Capsule())
        .task(id: shareCardKey) { renderShareCard() }
    }

    /// What makes the card worth re-rendering: the pick, and whether the
    /// translation it would quote underneath has arrived or changed.
    private var shareCardKey: ShareCardKey {
        ShareCardKey(selection: selection, translated: translator?.translatedLines != nil)
    }

    private struct ShareCardKey: Equatable {
        var selection: Set<Int>
        var translated: Bool
    }

    private func toggleShareSelection(_ index: Int) {
        if selection.contains(index) {
            selection.remove(index)
        } else if selection.count < maxSharedLines {
            selection.insert(index)
        }
        Haptics.play(.select)
    }

    /// The picked lines as the card draws them. A jump over unchosen lines
    /// becomes the omission mark, so the picture says the verse was cut rather
    /// than letting two distant halves read as if they ran on (upstream
    /// LyricsShare.kt:249). The line quoted is the original and the translation
    /// travels as its sub-line — the pane shows the translation in place, but a
    /// card that never says what was actually sung is not a quote.
    private var pickedCardLines: [LyricShareCard.Line] {
        let subLines = translator?.translatedLines
        let subbed = subLines?.count == lines.count
        var out: [LyricShareCard.Line] = []
        var previous = -1
        for index in selection.sorted() {
            guard lines.indices.contains(index) else { continue }
            if previous >= 0, index > previous + 1 {
                out.append(LyricShareCard.Line(text: "", isGap: true))
            }
            let original = originalLines?.indices.contains(index) == true
                ? originalLines![index].text
                : lines[index].text
            let sub = subbed ? subLines?[index].text : nil
            out.append(LyricShareCard.Line(
                text: original,
                subText: sub.flatMap {
                    let trimmed = $0.trimmingCharacters(in: .whitespacesAndNewlines)
                    return trimmed.isEmpty || $0 == original ? nil : $0
                }
            ))
            previous = index
        }
        return out
    }

    /// Draws the card once per selection. `ImageRenderer` is main-actor work, so
    /// this is an `.task`, and the result is kept until the selection changes.
    private func renderShareCard() {
        guard !selection.isEmpty else {
            shareImage = nil
            return
        }
        let tint = ArtworkPalette.pageTint(
            from: artworkData,
            seed: trackTitle.hashValue,
            dark: true
        )
        let card = LyricShareCard(
            lines: pickedCardLines,
            title: trackTitle,
            artist: trackArtist,
            tint: tint,
            artwork: artworkData,
            artworkUrl: artworkUrl
        )
        let renderer = ImageRenderer(content: card)
        renderer.scale = PlatformScale.current
        #if os(iOS)
        shareImage = renderer.uiImage.map { Image(uiImage: $0) }
        #else
        shareImage = renderer.nsImage.map { Image(nsImage: $0) }
        #endif
    }

    private var attribution: String? {
        guard let sourceLabel, !sourceLabel.isEmpty else { return nil }
        if alignmentInProgress { return "Lyrics by \(sourceLabel) · aligning locally…" }
        if alignedLocally { return "Lyrics by \(sourceLabel) · Synced by BitChord" }
        guard hasSyncedTimings else { return "Lyrics by \(sourceLabel) · unsynced" }
        let lyricRows = lines.filter { !$0.isGap }
        let timedRows = lyricRows.enumerated().filter { lineHasSyncedTimings($0.element, at: $0.offset) }
        return timedRows.count == lyricRows.count
            ? "Lyrics by \(sourceLabel)"
            : "Lyrics by \(sourceLabel) · partially synced"
    }
}

private struct WordSyncedLine: View {
    @AppStorage("lyrics_blur") private var lyricsBlur = true
    let line: LyricLineDto
    let hasSyncedTimings: Bool
    let active: Bool
    let distance: Int
    let position: Double
    var onTap: (() -> Void)? = nil

    /// Which side of the panel this line is sung from.
    ///
    /// A duet reads as two people because the voices are on opposite sides, not
    /// because the words say so. A call-and-response laid out down one side is one
    /// long verse with no idea who is singing it.
    private var isSecondVoice: Bool {
        line.alignment == .end
    }

    var body: some View {
        VStack(alignment: isSecondVoice ? .trailing : .leading, spacing: 4) {
            Text(leadRendered)
                .scaledFont(size: 34, weight: .bold, design: .default)
                .multilineTextAlignment(isSecondVoice ? .trailing : .leading)
                .fixedSize(horizontal: false, vertical: true)
                // The frame and the scale anchor follow the side as well. A second
                // voice that grew towards the left would lean out of its own column
                // the moment it was sung, which is the one moment the reader is
                // looking at it.
                .shadow(color: active ? .white.opacity(0.35) : .clear, radius: active ? 8 : 0, y: 0)
                .scaleEffect(active ? 1.04 : 1, anchor: isSecondVoice ? .trailing : .leading)
                .contentShape(Rectangle())
                .onTapGesture { onTap?() }
            if let backing = backingRendered {
                Text(backing)
                    .scaledFont(size: 24, weight: .semibold, design: .default)
                    .multilineTextAlignment(isSecondVoice ? .trailing : .leading)
                    .fixedSize(horizontal: false, vertical: true)
                    .opacity(0.45)
                    .contentShape(Rectangle())
                    .onTapGesture { onTap?() }
            }
        }
        .opacity(hasSyncedTimings ? (active ? 1 : max(0.18, 0.58 - Double(distance) * 0.12)) : 0.9)
        .blur(radius: lyricsBlur && hasSyncedTimings && !active ? min(CGFloat(distance) * 1.15, 4) : 0)
        .animation(.easeInOut(duration: 0.2), value: active)
        .animation(.easeInOut(duration: 0.2), value: distance)
        .animation(.easeInOut(duration: 0.12), value: Int(position * 10))
    }

    private var split: (lead: String, backing: String?) {
        if let background = line.background, !background.text.isEmpty {
            return (line.text, background.text)
        }
        return Self.splitBackground(line.text)
    }

    private var leadRendered: AttributedString {
        render(text: split.lead, words: leadWords, glowing: hasSyncedTimings && active)
    }

    private var backingRendered: AttributedString? {
        if let background = line.background, !background.text.isEmpty {
            return render(text: background.text, words: background.words, glowing: false)
        }
        guard let backing = split.backing, !backing.isEmpty else { return nil }
        return render(text: backing, words: backingWords, glowing: false)
    }

    private var leadWords: [LyricWordDto] {
        if line.background != nil { return line.words }
        return line.words.filter { !Self.isBackingToken($0.text) }
    }

    private var backingWords: [LyricWordDto] {
        line.words.filter { Self.isBackingToken($0.text) }
    }

    private func render(text: String, words: [LyricWordDto], glowing: Bool) -> AttributedString {
        if words.isEmpty {
            var s = AttributedString(text.isEmpty ? "♪" : text)
            s.font = .system(size: 34, weight: active ? .bold : .semibold, design: .default)
            s.foregroundColor = Color.white.opacity(active ? 1 : max(0.22, 0.55 - Double(distance) * 0.12))
            return s
        }
        let ms = Swift.Int64(position * 1000)
        var result = AttributedString()
        for (i, word) in words.enumerated() {
            var run = AttributedString(word.text.replacingOccurrences(of: "(", with: "").replacingOccurrences(of: ")", with: ""))
            let sung = ms >= word.startMs
            let current = sung && ms < word.endMs
            run.font = .system(size: 34, weight: current ? .bold : .semibold, design: .default)
            if glowing && current {
                run.foregroundColor = Color.white
                run.underlineStyle = .single
            } else {
                run.foregroundColor = Color.white.opacity(sung ? 1 : 0.38)
            }
            result.append(run)
            if i < words.count - 1 {
                result.append(AttributedString(" "))
            }
        }
        return result
    }

    private static func isBackingToken(_ text: String) -> Bool {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        return trimmed.hasPrefix("(") || trimmed.hasSuffix(")")
    }

    /// Display-only split matching upstream `withBackgroundVocals`.
    static func splitBackground(_ text: String) -> (lead: String, backing: String?) {
        guard let open = text.firstIndex(of: "("), let close = text.lastIndex(of: ")"), close > open else {
            return (text, nil)
        }
        let inner = text[text.index(after: open)..<close]
            .trimmingCharacters(in: .whitespaces)
        guard !inner.isEmpty else { return (text, nil) }
        let lead = (text[..<open] + text[text.index(after: close)...])
            .trimmingCharacters(in: .whitespaces)
        return (lead.isEmpty ? "♪" : String(lead), String(inner))
    }
}

/// Upstream's lyrics-sources sheet lives in [LyricsLanguageSheet.swift] as
/// [LyricsSourcesSheet] (the fuller superset: try-order, PaxSeniX key,
/// word-sync preference, reset). The player's More menu presents that same
/// sheet rather than a local copy, so the two can never drift.
