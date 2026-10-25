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
    func buildAudioQualityRows(_ rows: inout SettingsRows) {
        rows.row("Sources", "webdav", "server", "addons", "jiosaavn", "source order", "priority",
                 "remote library", "nextcloud", "owncloud", "nas", "self hosted", "smb") {
            PushLink {
                SourcesView()
            } label: {
                SettingsLine(
                    glyph: .sources,
                    title: "Sources",
                    subtitle: "Where audio comes from, and in what order"
                ) {
                    EmptyView()
                }
            }
        }
        rows.row("Sound Mode", "transparent", "enhanced", "clarity", "preset", "dsp") {
            Picker("Sound Mode", selection: Binding(get: { controller.soundMode }, set: { controller.updateSoundMode($0) })) {
                Text("Transparent").tag("TRANSPARENT")
                Text("Enhanced").tag("ENHANCED")
            }
        }
        if controller.soundMode == "ENHANCED" {
            rows.row("Clarity Preset", "reference", "speaker", "headphone", "dac", "clarity") {
                Picker("Clarity Preset", selection: Binding(get: { controller.clarityPreset }, set: { controller.updateClarity(preset: $0) })) {
                    Text("Reference").tag("REFERENCE")
                    Text("Speaker").tag("SPEAKER")
                    Text("Headphone").tag("HEADPHONE")
                    Text("DAC").tag("DAC")
                }
            }
            rows.row("Clarity Mix", "wet", "clarity", "enhanced", "tone") {
                VStack(alignment: .leading) {
                    Text("Clarity Mix · \(Int(controller.clarityWet * 100))%")
                    Slider(value: Binding(get: { controller.clarityWet }, set: { controller.updateClarity(wet: $0) }), in: 0...1)
                }
            }
        }
        rows.row("Loudness Normalization", "loudness", "normalize", "track", "album", "off", "speaker") {
            VStack(alignment: .leading, spacing: 4) {
                SettingsToggleLine(
                    glyph: .loudness,
                    title: "Loudness Normalization",
                    subtitle: "Levels tracks to a consistent loudness",
                    isOn: $loudness
                )
                if loudness {
                    Picker("Normalization Mode", selection: $loudnessModeSelection) {
                        Text("Track").tag("TRACK")
                        Text("Album").tag("ALBUM")
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .padding(.leading, 41)
                }
                #if os(iOS)
                SettingsSubToggle(title: "Off on iPhone Speaker", isOn: $loudnessOffOnSpeaker)
                #endif
            }
        }
        rows.row("Sound Mode", "preserves", "recording", "transparent", "clarity", "spatial") {
            Text(controller.soundMode == "TRANSPARENT" ? "Preserves the recording. Playback speed, Automix and crossfade keep their own settings." : "Reference clarity adds tonal contour, centered bass and gentle width. Manual EQ and spatial remain optional.")
                .font(.footnote).foregroundStyle(.secondary)
        }
        qualityRow(
            &rows,
            glyph: .wifi,
            title: "On Wi-Fi",
            badge: metered ? nil : "In Use",
            selection: $wifiQuality,
            options: AudioQualityOption.stream,
            keywords: ["wifi", "wi-fi", "quality", "bitrate", "bitrate cap", "lossless", "flac", "hi-res",
                       "streaming", "unmetered", "transcode"]
        )
        qualityRow(
            &rows,
            glyph: .cellular,
            title: "On Mobile Data",
            badge: metered ? "In Use" : nil,
            selection: $cellQuality,
            options: AudioQualityOption.stream,
            keywords: ["cellular", "mobile data", "metered", "quality", "bitrate", "bitrate cap", "lossless",
                       "flac", "streaming", "transcode"]
        )
        rows.row("Lossless", "hi-res", "pipeline", "format", "quality", "bitrate") {
            Text("Lossless includes Hi-Res when an enabled source supplies it. Audio Pipeline shows the format actually decoded.")
                .font(.footnote).foregroundStyle(.secondary)
        }
        rows.row("Output Precision", "pcm", "float", "bit depth", "precision", "output", "dac", "bit perfect") {
            VStack(alignment: .leading, spacing: 10) {
                SettingsLine(
                    glyph: .precision,
                    title: "Output Precision",
                    subtitle: outputPrecisionSubtitle
                ) {
                    EmptyView()
                }
                Picker("Output Precision", selection: $outputPcm) {
                    Text("PCM 16").tag("PCM_16")
                    Text("Float 32").tag("FLOAT_32")
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .padding(.leading, 41)
            }
            .padding(.vertical, 4)
        }
        rows.row("Match Source Sample Rate", "sample rate", "clock", "route", "dac", "precision") {
            SettingsToggleLine(
                glyph: .precision,
                title: "Match Source Sample Rate",
                subtitle: "Request each track’s native rate from the output route",
                isOn: $matchSourceRate
            )
        }
        #if os(macOS)
        rows.row("Bit-perfect Output", "bit perfect", "bypass", "exclusive", "lossless", "dac", "hog", "precision") {
            SettingsToggleLine(
                glyph: .precision,
                title: "Bit-perfect Output",
                subtitle: "Bypasses app volume, DSP and transitions for supported lossless stereo",
                isOn: $bitPerfect
            )
        }
        #endif
        rows.row("Prefer USB DAC", "usb", "dac", "headphone", "output", "precision") {
            SettingsSubToggle(title: "Prefer USB DAC", isOn: $preferUsbDac)
        }
        rows.row("Dolby Atmos", "dolby", "atmos", "surround", "immersive", "spatial", "renderer") {
            SettingsToggleLine(glyph: .dolby, title: "Dolby Atmos",
                subtitle: "Use Apple’s renderer when a source provides supported Dolby audio", isOn: $dolbyAtmos)
                .disabled(!AppleDolbyRenderer.available)
        }
    }

    /// What the output is doing right now, read off the engine rather than the
    /// setting — the setting is the request, and this is the answer.
    private var outputPrecisionSubtitle: String {
        let device = controller.outputDevice
        guard device.started else { return "Requested \(Self.pcmLabel(outputPcm))" }
        let actual = Self.pcmLabel(device.sampleFormat)
        var parts = [actual]
        if device.sampleFormat != outputPcm {
            parts.append("requested \(Self.pcmLabel(outputPcm))")
        }
        if !device.name.isEmpty { parts.append(device.name) }
        if device.sampleRate > 0 {
            let khz = Double(device.sampleRate) / 1000
            parts.append(String(format: khz == khz.rounded() ? "%.0f kHz" : "%.1f kHz", khz))
        }
        return parts.joined(separator: " · ")
    }

    private static func pcmLabel(_ mode: String) -> String {
        switch mode {
        case "FLOAT_32": return "Float 32"
        case "PCM_16": return "PCM 16"
        case "PCM_24": return "PCM 24"
        default: return "PCM 16"
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
