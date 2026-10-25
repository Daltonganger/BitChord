import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
#if os(iOS)
import UIKit
#else
import AppKit
#endif

// Playback: crossfade, AutoMix and its models, and the rest of playback.
extension SettingsView {
    func buildPlaybackRows(_ rows: inout SettingsRows) {
        // Automix decides its own length from each pair of tracks, so it
        // replaces the manual slider rather than needing it set to anything
        // first — upstream's rule, which is why the slider goes away
        // entirely instead of greying out.
        if automix {
            rows.row("Crossfade", "automix", "blend", "fade", "transition", "gapless") {
                Text("Automix times every transition itself; the crossfade slider returns when Automix is off.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .padding(.vertical, 4)
            }
        } else {
            rows.row("Crossfade", "blend", "fade", "transition", "gapless", "automix") {
                SettingsSliderRow(
                    glyph: .crossfade,
                    title: "Crossfade",
                    subtitle: "Blends one track into the next",
                    valueText: crossfade == 0 ? "Off" : "\(crossfade)s",
                    value: Binding(
                        get: { Double(crossfade) },
                        set: { crossfade = Int($0.rounded()) }
                    ),
                    range: 0...12,
                    step: 1
                )
            }
        }
        rows.row("Automix", "blend", "transition", "beat", "model", "timing", "crossfade", "overheating", "lag") {
            SettingsToggleLine(
                glyph: .automix,
                title: "Automix [Beta]",
                subtitle: automix
                    ? (AutomixModelStore.shared.beatAvailable
                        ? "Blends every transition, timed automatically from each track. Turn off if facing overheating or lag."
                        : "Blends every transition automatically. The beat model is not installed, so timing comes from tempo analysis — install it below for transitions that land on the beat.")
                    : "Times and blends transitions automatically, no slider needed.",
                isOn: $automix
            )
        }
        if automix {
            rows.row("Smart Sequencing", "sequence", "order", "suggestions", "autoplay", "transition", "shuffle") {
                SettingsToggleLine(
                    glyph: .automix,
                    title: "Smart Sequencing",
                    subtitle: "Order upcoming list tracks and Autoplay suggestions for smooth transitions, listening taste and freshness. Takes priority over Shuffle; manually queued tracks keep their order.",
                    isOn: $automixSequence
                )
            }
            rows.row("Mix on Skip", "skip", "next", "transition", "echo", "beat") {
                SettingsToggleLine(
                    glyph: .automix,
                    title: "Mix on Skip",
                    subtitle: "Pressing Next blends into the next track on the beat instead of cutting. Off during a party and on Repeat One.",
                    isOn: Binding(
                        get: { controller.automixOnSkipEnabled },
                        set: { controller.setAutomixOnSkipEnabled($0) }
                    )
                )
            }
        }
        rows.row("Automix Performance", "cpu", "threads", "analysis", "budget", "efficient", "balanced", "performance", "battery") {
            PushLink {
                AutomixPerformancePage(selection: $automixPerf)
            } label: {
                SettingsLine(
                    glyph: .cpu,
                    title: "Automix Performance",
                    subtitle: "Background analysis budget — \(Self.automixPerfLabel(automixPerf)), \(controller.automixInferenceThreads) thread\(controller.automixInferenceThreads == 1 ? "" : "s")"
                ) {
                    Text(Self.automixPerfLabel(automixPerf))
                        .foregroundStyle(.secondary)
                }
            }
        }
        rows.row("Automix Models", "models", "model", "onnx", "beat", "downbeat", "vocal", "install", "download models") {
            PushLink {
                AutomixModelsPage()
            } label: {
                SettingsLine(
                    glyph: .models,
                    title: "Automix Models",
                    subtitle: Self.automixModelsSubtitle
                ) {
                    EmptyView()
                }
            }
        }
        rows.row("Show Music Videos", "video", "music video", "clip", "picture", "avc", "mv") {
            SettingsToggleLine(
                glyph: .video,
                title: "Show Music Videos",
                subtitle: "Shows the music video while the player is open. Off at the start of every session; the Now Playing pill turns it on too.",
                isOn: $showMusicVideos
            )
        }
        rows.row("Trim Leading and Trailing Silence", "silence", "trim", "edges", "gap") {
            Toggle("Trim Leading and Trailing Silence", isOn: $trimEdges)
                .onChange(of: trimEdges) { _, value in
                    PlatformSettings.shared.putBoolean(key: "trim_edge_silence", value: value)
                    controller.refreshPlaybackRegions()
                }
        }
        rows.row("Skip Non-Music Segments", "non-music", "segments", "skit", "intro", "podcast") {
            Toggle("Skip Non-Music Segments", isOn: $skipNonMusic)
                .onChange(of: skipNonMusic) { _, value in
                    PlatformSettings.shared.putBoolean(key: "skip_non_music", value: value)
                    controller.refreshPlaybackRegions()
                }
        }
        rows.row("Non-music Segments", "automix", "boundaries", "segments") {
            Text("Non-music segments and audible boundaries are always used by Automix.")
                .font(.caption).foregroundStyle(.secondary)
        }
        rows.row("Diagnostic Reports", "debug", "log", "report", "diagnostics") {
            PushLink("Diagnostic Reports") { DiagnosticReportsView() }
        }
        rows.row("Skip Silence", "silence", "gaps", "trim") {
            SettingsToggleLine(
                glyph: .skipSilence,
                title: "Skip Silence",
                subtitle: "Trim gaps longer than a second",
                isOn: $skipSilence
            )
        }
        rows.row("Playback Speed", "speed", "tempo", "rate", "pitch", "slow", "fast") {
            SettingsSliderRow(
                glyph: .speed,
                title: "Playback Speed",
                subtitle: "Slows or speeds the mix without changing pitch on the engine",
                valueText: String(format: "%.2fx", playbackSpeed),
                value: $playbackSpeed,
                range: 0.5...2.0,
                step: 0.05
            )
        }
        rows.row("BitChord spatial effect", "spatial", "widening", "head tracking", "stereo") {
            SettingsToggleLine(
                glyph: .spatial,
                title: "BitChord spatial effect",
                subtitle: "Optional stereo widening and head tracking. Suspended when Apple reports Spatial Audio is enabled.",
                isOn: $spatial
            )
        }
        rows.row("Spatial Audio", "airpods", "adaptive", "accessibility", "headphone", "accommodations") {
            Text("Apple’s Headphone Accommodations, Adaptive Audio and Conversation Awareness are managed in system Accessibility, AirPods settings and Control Center. BitChord keeps your manual EQ choices.")
                .font(.caption).foregroundStyle(.secondary)
        }
#if os(iOS)
        rows.row("Play Alongside Other Apps", "mix", "other apps", "background audio", "interrupt") {
            SettingsToggleLine(
                glyph: .musicOnly,
                title: "Play Alongside Other Apps",
                subtitle: "Keep other apps audible alongside BitChord. This can prevent iOS Lock Screen, Control Center and headphone controls from controlling BitChord. Turn off to give BitChord audio priority. Applies immediately while playing, or on the next play.",
                isOn: Binding(
                    get: { controller.mixWithOtherAudio },
                    set: { controller.setMixWithOtherAudio($0) }
                )
            )
        }
#endif
        rows.row("Equalizer", "eq", "bands", "bass", "treble") {
            PushLink {
                EqualizerView()
            } label: {
                SettingsLine(
                    glyph: .equalizer,
                    title: "Equalizer",
                    subtitle: "Ten-band EQ on the mixed output"
                ) {
                    EmptyView()
                }
            }
        }
        rows.row("Show Stats for Nerds", "nerds", "stats", "codec", "bitrate", "sample rate") {
            SettingsToggleLine(
                glyph: .nerd,
                title: "Show Stats for Nerds",
                subtitle: "Codec, bitrate and sample rate on the player",
                isOn: $nerdStats
            )
        }
        rows.row("Stop Converting Video Songs to Audio", "video", "music video", "convert", "audio") {
            SettingsToggleLine(
                glyph: .video,
                title: "Stop Converting Video Songs to Audio",
                subtitle: "Plays a music-video upload as itself instead of swapping it for its catalogue audio release",
                isOn: Binding(
                    get: { !convertVideo },
                    set: { convertVideo = !$0 }
                )
            )
        }
        rows.row("Prefer Music Only", "music video", "video", "catalogue", "upload") {
            SettingsToggleLine(
                glyph: .musicOnly,
                title: "Prefer Music Only",
                subtitle: "Starts a music-video result on its catalogue audio release instead of the video's own upload",
                isOn: $preferMusicOnly
            )
        }
    }

    private static func automixPerfLabel(_ mode: String) -> String {
        switch mode {
        case "EFFICIENT": return "Efficient"
        case "PERFORMANCE": return "Performance"
        default: return "Balanced"
        }
    }

    /// What the Automix Models row says without opening it.
    ///
    /// State rather than instruction: "Not installed" is the fact, and the row's
    /// destination is where the action is — a subtitle that also shouts "tap here"
    /// is a settings row pretending to be a button.
    private static var automixModelsSubtitle: String {
        let store = AutomixModelStore.shared
        // A verified previous graph is still a working model: say so, and say that
        // an update is waiting, rather than calling it "not installed".
        if store.isUsingPreviousGraph(.beat) {
            return "Update available. Using the previous version — \(AutomixModelStore.bytes(store.totalBytesOnDisk)) on disk"
        }
        if store.status(of: .beat).isInstalled {
            return "Beat and downbeat model installed — \(AutomixModelStore.bytes(store.totalBytesOnDisk)) on disk · vocal detection is built in"
        }
        return "Not installed — Automix falls back to tempo analysis · vocal detection is built in"
    }

    // MARK: - Appearance
}
