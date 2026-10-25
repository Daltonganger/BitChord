import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
#if os(iOS)
import UIKit
#else
import AppKit
#endif

struct SettingsLine<Trailing: View>: View {
    var glyph: SettingsGlyph.Kind
    var title: String
    var subtitle: String? = nil
    var badge: String? = nil
    @ViewBuilder var trailing: () -> Trailing

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            SettingsGlyph(kind: glyph)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 8) {
                    Text(title)
                    if let badge {
                        Text(badge.uppercased())
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.white)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(Color.accentColor, in: Capsule())
                    }
                }
                if let subtitle, !subtitle.isEmpty {
                    Text(subtitle)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 8)
            trailing()
        }
        .padding(.vertical, 3)
    }
}

struct SettingsToggleLine: View {
    var glyph: SettingsGlyph.Kind
    var title: String
    var subtitle: String? = nil
    @Binding var isOn: Bool

    var body: some View {
        Toggle(isOn: $isOn) {
            SettingsLine(glyph: glyph, title: title, subtitle: subtitle) {
                EmptyView()
            }
        }
        .padding(.vertical, 2)
    }
}

struct SettingsSubToggle: View {
    var title: String
    @Binding var isOn: Bool
    var badge: String? = nil

    var body: some View {
        Toggle(isOn: $isOn) {
            HStack(spacing: 8) {
                Text(title)
                if let badge {
                    Text(badge.uppercased())
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Color.orange, in: Capsule())
                }
            }
        }
        .padding(.leading, 41)
        .padding(.vertical, 2)
    }
}

struct SettingsSliderRow: View {
    var glyph: SettingsGlyph.Kind
    var title: String
    var subtitle: String
    var valueText: String
    @Binding var value: Double
    var range: ClosedRange<Double>
    var step: Double

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SettingsLine(glyph: glyph, title: title, subtitle: subtitle) {
                Text(valueText)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
            Slider(value: $value, in: range, step: step)
                .padding(.leading, 41)
        }
        .padding(.vertical, 4)
    }
}

struct SettingsGlyph: View {
    enum Kind {
        case person, listenAs, sources, wifi, cellular, download
        case crossfade, automix, skipSilence, spatial, dolby, equalizer, nerd, video, speed
        case theme, reduceMotion, reduceBlur, fullBleed, canvas, lyrics, lyricsSources
        case storage, clearSongs, clearImages
        case swipe, dontRepeat, hideVolume
        case discord, listenBrainz, lastFm
        case replay, genres, export, importData, listenTogether
        case update
        case precision, loudness, sharedFolder, cpu, musicOnly, models
        case performance, refreshRate, folder, localMusic, filter
        case songStatus, align, mesh
    }

    var kind: Kind

    var body: some View {
        Group {
            if let asset { Image(asset).resizable().scaledToFit() }
            else { Image(systemName: symbol).scaledFont(size: 21) }
        }
        .foregroundStyle(.primary)
        .frame(width: 29, height: 29)
        .accessibilityHidden(true)
    }

    private var asset: String? {
        switch kind {
        case .dolby: "bch-dolby"
        case .performance: "bch-performance"
        case .refreshRate: "bch-frame-rate"
        case .download: "bch-download"
        case .lyrics: "bch-lyrics"
        case .localMusic: "bch-library"
        case .musicOnly: "bch-music-note"
        case .dontRepeat: "bch-clock"
        default: "bch-settings-\(symbolName)"
        }
    }
    private var symbolName: String {
        switch kind {
        case .person: "person"
        case .listenAs, .listenTogether: "groups"
        case .sources: "extension"
        case .wifi: "wifi"
        case .cellular: "signal_cellular_alt"
        case .crossfade: "waves"
        case .automix, .models: "auto_awesome"
        case .skipSilence, .hideVolume: "volume_off"
        case .dolby: "surround_sound"
        case .spatial: "surround_sound"
        case .equalizer, .align: "tune"
        case .nerd, .replay: "bar_chart"
        case .video: "smart_display"
        case .theme: "brightness_4"
        case .reduceMotion: "motion_photos_off"
        case .reduceBlur: "blur_off"
        case .fullBleed: "fullscreen"
        case .canvas: "animation"
        case .lyricsSources: "language"
        case .storage: "storage"
        case .clearSongs, .clearImages: "delete_sweep"
        case .swipe: "playlist_play"
        case .genres: "local_offer"
        case .export: "file_upload"
        case .importData, .update: "file_download"
        case .loudness, .precision: "graphic_eq"
        case .sharedFolder, .folder: "folder"
        case .filter: "filter_alt"
        case .songStatus: "visibility_off"
        case .mesh: "gradient"
        case .speed, .performance: "speed"
        case .refreshRate: "monitor"
        case .cpu: "memory"
        case .discord: "chat"
        case .listenBrainz: "cloud"
        case .lastFm: "history"
        default: "settings"
        }
    }

    private var symbol: String {
        switch kind {
        case .person: "person.fill"
        case .listenAs: "person.2.fill"
        case .sources: "square.stack.3d.up.fill"
        case .wifi: "wifi"
        case .cellular: "antenna.radiowaves.left.and.right"
        case .download: "arrow.down.circle.fill"
        case .crossfade: "waveform"
        case .speed: "gauge.with.dots.needle.67percent"
        case .automix: "sparkles"
        case .skipSilence: "speaker.slash.fill"
        case .dolby: "hifispeaker.2.fill"
        case .spatial: "hifispeaker.2.fill"
        case .equalizer: "slider.vertical.3"
        case .nerd: "chart.bar.fill"
        case .video: "play.rectangle.fill"
        case .theme: "circle.lefthalf.filled"
        case .reduceMotion: "figure.walk.motion"
        case .reduceBlur: "circle.hexagongrid.fill"
        case .fullBleed: "arrow.up.left.and.arrow.down.right"
        case .canvas: "livephoto"
        case .lyrics: "text.alignleft"
        case .lyricsSources: "globe"
        case .storage: "internaldrive.fill"
        case .clearSongs: "trash.fill"
        case .clearImages: "photo.on.rectangle.angled"
        case .swipe: "text.line.first.and.arrowtriangle.forward"
        case .dontRepeat: "arrow.uturn.backward"
        case .hideVolume: "speaker.wave.2.fill"
        case .discord: "bubble.left.and.bubble.right.fill"
        case .listenBrainz: "cloud.fill"
        case .lastFm: "clock.arrow.circlepath"
        case .replay: "chart.bar.fill"
        case .genres: "tag.fill"
        case .export: "square.and.arrow.up.fill"
        case .importData: "square.and.arrow.down.fill"
        // Two people and a sound wave, because the alternative — a single person —
        // reads as a profile picture and this row is not about a profile.
        case .listenTogether: "person.2.wave.2.fill"
        // An arrow down into a tray, which is what "a newer build is here" looks like
        // and is not the gear everybody expects an update row to be.
        case .update: "arrow.down.app.fill"
        case .precision: "waveform.path.ecg"
        case .loudness: "speaker.wave.3.fill"
        case .sharedFolder: "folder.fill.badge.plus"
        case .cpu: "cpu.fill"
        // A brain rather than a chip: this row is about what the analysis knows,
        // not about the hardware that runs it — `cpu` already sits one row above.
        case .models: "brain"
        case .musicOnly: "music.note"
        case .performance: "gauge.with.dots.needle.100percent"
        case .refreshRate: "display"
        case .folder: "folder.fill"
        case .localMusic: "music.note.list"
        case .filter: "line.3.horizontal.decrease.circle.fill"
        case .songStatus: "eye.slash.fill"
        case .align: "arrow.left.and.right"
        case .mesh: "paintbrush.pointed.fill"
        }
    }


}
