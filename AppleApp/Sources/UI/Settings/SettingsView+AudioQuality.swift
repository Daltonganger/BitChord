import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
#if os(iOS)
import UIKit
#else
import AppKit
#endif

// Audio quality and downloads: stream and download quality, output precision.
extension SettingsView {
    var audioPreferences: AudioSettingsPreferences {
        AudioSettingsPreferences(wifiQuality: $wifiQuality, cellQuality: $cellQuality,
            outputPcm: $outputPcm, matchSourceRate: $matchSourceRate, bitPerfect: $bitPerfect,
            preferUsbDac: $preferUsbDac, loudness: $loudness, loudnessMode: $loudnessModeSelection,
            loudnessOffOnSpeaker: $loudnessOffOnSpeaker, spatial: $spatial, dolbyAtmos: $dolbyAtmos)
    }

    func buildAudioQualityRows(_ rows: inout SettingsRows) {
        if rows.query.isEmpty {
            for page in AudioSettingsPage.allCases where page != .technical {
                rows.row(page.title) {
                    audioLink(page: page)
                }
            }
        } else {
            for item in AudioSettingsItem.allCases where item != .sources {
                rows.row(item.title, keywords: item.keywords + [item.page.title]) {
                    audioLink(page: item == .technical ? .technical : item.page, focus: item == .technical ? nil : item)
                }
            }
        }
    }

    private func audioLink(page: AudioSettingsPage, focus: AudioSettingsItem? = nil) -> some View {
        PushLink {
            AudioSettingsPageView(page: page, preferences: audioPreferences, focus: focus)
        } label: {
            SettingsLine(glyph: page.glyph,
                title: focus?.title ?? page.title, subtitle: focus == nil ? page.detail : page.title) { EmptyView() }
        }
    }

    // MARK: - Downloads

    func buildDownloadRows(_ rows: inout SettingsRows) {
        qualityRow(
            &rows,
            glyph: .download,
            title: "Download Quality",
            subtitle: "\(AudioQualityOption.label(downloadQuality, in: AudioQualityOption.download)) per track, whatever the connection",
            selection: $downloadQuality,
            options: AudioQualityOption.download,
            keywords: ["download", "bitrate", "lossless", "flac", "offline", "saved", "quality", "transcode"]
        )
        rows.row("Download over Wi-Fi Only", "wifi", "wi-fi", "wifi only", "wi-fi only", "cellular",
                 "mobile data", "blocking", "download", "offline") {
            SettingsSubToggle(
                title: "Download over Wi-Fi Only",
                isOn: $wifiOnlyDownloads,
                badge: (wifiOnlyDownloads && metered) ? "Blocking" : nil
            )
        }
        rows.row("Export Compatible Downloads", "export", "music folder", "shared", "visible", "download") {
            SettingsToggleLine(
                glyph: .sharedFolder,
                title: "Export Compatible Downloads",
                subtitle: exportDownloads
                    ? "Music/BitChord — visible to other music apps"
                    : "Keeps downloads where other music apps can see them",
                isOn: $exportDownloads
            )
        }
    }

    // MARK: - Playback

    /// One audio-quality row: a menu on the Mac, a drill-down on iOS.
    ///
    /// Registers itself with the group like any other row, so the keywords follow
    /// the row rather than living in a per-section table.
    private func qualityRow(
        _ rows: inout SettingsRows,
        glyph: SettingsGlyph.Kind,
        title: String,
        subtitle: String? = nil,
        badge: String? = nil,
        selection: Binding<String>,
        options: [AudioQualityOption],
        keywords: [String]
    ) {
        rows.row(title, keywords: keywords) {
            #if os(macOS)
            Picker(selection: selection) {
                ForEach(options) { option in
                    Text(option.title).tag(option.id)
                }
            } label: {
                SettingsLine(glyph: glyph, title: title, subtitle: subtitle, badge: badge) {
                    EmptyView()
                }
            }
            .pickerStyle(.menu)
            #else
            PushLink {
                QualityPickerPage(title: title, selection: selection, options: options)
            } label: {
                SettingsLine(glyph: glyph, title: title, subtitle: subtitle, badge: badge) {
                    Text(AudioQualityOption.label(selection.wrappedValue, in: options))
                        .foregroundStyle(.secondary)
                }
            }
            #endif
        }
    }
}

extension AudioSettingsPage {
    var glyph: SettingsGlyph.Kind {
        switch self {
        case .source: return .sources
        case .processing: return .equalizer
        case .output, .technical: return .precision
        }
    }
}
