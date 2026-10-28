import SwiftUI
import BitChordShared
import AVKit
import AVFoundation
#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// Upstream `AudioPipelineDialog`: the live signal path, decoder to output.
///
/// ## Why this exists on a platform with less to report
///
/// Upstream builds it on Android's `AudioOutputStatus` — an AAudio sink name, an
/// `AudioFormat` encoding, a route kind, a transport type. None of those have an
/// Apple equivalent, and inventing one would be worse than the gap: a panel that
/// prints "AAudio" on an iPhone is a lie with a nice animation on it.
///
/// So the sections are upstream's and the rows are the ones this platform can
/// actually answer. What the system will not tell us is shown as "—", which is
/// the same thing upstream prints for a value it does not have yet, rather than
/// filled in with a plausible guess. Every filled row is read from the engine or
/// from a setting that is actually in effect.
///
/// The sample-processing verdict is computed from the format and DSP state.
/// Bit-perfect is shown as active only after macOS grants exclusive device
/// access and CoreAudio reports the requested physical integer format and rate.
/// This still cannot inspect processing inside a DAC after its input stream.
struct AudioPipelineSheet: View {
    @Environment(PlaybackController.self) private var controller
    @Environment(\.dismiss) private var dismiss
    @State private var captureRunning = false
    @State private var captureMessage: String?

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    Text("Live signal path, decoder to output")
                        .font(.caption)
                        .foregroundStyle(.white.opacity(0.45))
                        .padding(.bottom, 14)
                    stage(0, "doc.text", "Track Info") {
                        row("Requested Source Quality", claimedQuality)
                        row("Measured Stream", measuredStream)
                        row("Source", sourceName)
                        if let id = controller.current?.id, let note = controller.sourceDecisionNotes[id] {
                            row("Source decision", note)
                        }
                        row("Format", codec)
                        row("Bit Depth", bitDepth)
                        row("Sample Rate", sourceRate)
                        row("Bitrate", bitrate)
                        row("Channels", channels)
                    }
                    rule
                    stage(1, "waveform", "Decoder") {
                        row("Decoder Name", decoderName)
                        row("Sample Rate", sourceRate)
                        row("Format", codec)
                        row("Bit Depth", bitDepth)
                    }
                    rule
                    stage(2, "arrow.up.arrow.down", "Resampler") {
                        row("I/O Rate", ioRate)
                        row("Type", resamplerType)
                        row("Quality", resamplerQuality)
                    }
                    rule
                    stage(3, "slider.horizontal.3", "DSP") {
                        row("PCM Format", pcmFormat)
                        row("Sample Rate", deviceRate)
                        row("Bit-perfect", bitPerfectStatus)
                        row("Loudness", loudness)
                        row("EQ Preset", eqPreset)
                        row("BitChord spatial effect", spatial && AudioRouteState.shared.permitsCustomSpatial ? "On" : "Off")
                        row("Apple spatial playback", AudioRouteState.shared.spatialDescription)
                        row("Output API", "CoreAudio")
                        // Reports engine-side sample changes; a clean engine path
                        // still does not prove CoreAudio's final hardware output.
                        row("Sample Path", samplePathVerdict)
                    }
                    rule
                    stage(4, "speaker.wave.2", "Output Device") {
                        row("Device Name", deviceName)
                        row("Sample Rate", deviceRate)
                        row("Channels", deviceChannels)
                        row("Bit-perfect", bitPerfectStatus)
                    }
                    rule
                    stage(5, "shuffle", "Automix") {
                        row("Next Transition", controller.analysisTier ?? "—")
                        row("Why", controller.transitionReason ?? "—")
                        row("Analysis", controller.analysisSources ?? "—")
                    }
                    rule
                    TimelineView(.periodic(from: .now, by: 1)) { _ in
                        stage(6, "waveform.path.ecg", "Playback Diagnostics") {
                            row("Buffered", "\(controller.outputHealth.bufferedFrames) frames")
                            row("Silent callbacks", "\(controller.outputHealth.callbackUnderruns)")
                            row("Output rebuilds", "\(controller.outputHealth.outputRebuilds)")
                            row("Device xruns", "\(controller.outputHealth.outputXruns)")
                            row("Output peak", String(format: "%.4f", controller.outputHealth.outputPeak))
                            // A quality upgrade fades one recording into another
                            // copy of itself, and how alike the two encodings are
                            // is what decides whether that fade is level-flat.
                            // Measured on the blended samples; “—” until one runs.
                            row("Upgrade correlation", upgradeCorrelation)
                            row("Active Processing", nerd?.activeStages.joined(separator: " · ") ?? "—")
                            row("Converter Delay", String(format: "%.1f frames", nerd?.converterDelayFrames ?? 0))
                            row("Protection Reduction", String(format: "%.2f dB", nerd?.protectionReductionDb ?? 0))
                            row("Protection Interventions", "\(nerd?.protectionInterventions ?? 0)")
                            row("Decoded Layout", nerd?.decodedLayout ?? "—")
                            row("Loudness Source", nerd?.loudnessOrigin ?? "—")
                            row("Queue Target", "\(nerd?.queuedTargetMs ?? 0) ms")
                            row("Engine Build", nerd?.buildRevision ?? "—")
                            Button(captureRunning ? "Capturing 30 seconds…" : "Capture 30 seconds") {
                                capturePlayback()
                            }
                            .disabled(captureRunning || controller.current == nil)
                            .padding(.top, 10)
                            if let captureMessage { Text(captureMessage).font(.caption).textSelection(.enabled) }

                        }
                    }
                    note("Values are read from the engine as it runs. “—” is a figure the system does not report, not one this app chose to hide.")
                }
                .padding(.horizontal, 20)
                .padding(.vertical, 16)
            }
            .navigationTitle("Audio Pipeline")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    if #available(iOS 26.0, macOS 26.0, *) {
                        Button(role: .close) { dismiss() }
                    } else {
                        Button("Close", systemImage: "xmark") { dismiss() }
                    }
                }
            }
        }
    }

    private func capturePlayback() {
        let engine = controller.engine
        let identity = controller.current?.source ?? "unknown"
        let recording = controller.current?.id ?? "unknown"
        let directory = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("AudioDiagnostics", isDirectory: true)
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        captureRunning = true
        captureMessage = nil
        Task {
            do {
                try engine.startDiagnosticCapture(directory: directory.path, seconds: 30)
                try await Task.sleep(for: .seconds(30))
                // The engine writes off the audio callback and acknowledges the
                // files before this completion message is displayed.
                try await Task.detached(priority: .utility) { try engine.finishDiagnosticCapture() }.value
                let details: [String: String] = ["recording_identity": recording, "selected_source": identity]
                let data = try JSONEncoder().encode(details)
                try data.write(to: directory.appendingPathComponent("recording.json"), options: .atomic)
                captureMessage = "Saved capture: \(directory.path)"
            } catch {
                captureMessage = "Capture failed: \(error.localizedDescription)"
            }
            captureRunning = false
        }
    }

    // ---- The signal path ----------------------------------------------------

    /// One stage: an icon, a heading, and its rows. The connector between stages
    /// is [rule] rather than a drawn line, because a live flowing line is a
    /// claim that samples are moving right now — and a paused engine would then
    /// be showing motion it does not have.
    private func stage<Content: View>(
        _ index: Int, _ icon: String, _ title: String, @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                Image(systemName: icon)
                    .scaledFont(size: 15, weight: .semibold)
                    .foregroundStyle(.white.opacity(0.7))
                    .frame(width: 28, height: 28)
                    .background(.white.opacity(0.10), in: Circle())
                Text(title)
                    .font(.headline)
                    .foregroundStyle(.white)
            }
            .padding(.top, index == 0 ? 0 : 14)

            VStack(alignment: .leading, spacing: 0) {
                content()
            }
            .padding(.leading, 38)
        }
    }

    private var rule: some View {
        Rectangle()
            .fill(.white.opacity(0.14))
            .frame(width: 1, height: 18)
            .padding(.leading, 14)
    }

    private func row(_ label: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label)
                .font(.callout)
                .foregroundStyle(.white.opacity(0.55))
            Spacer(minLength: 12)
            Text(value)
                .font(.callout.monospacedDigit())
                .foregroundStyle(.white.opacity(0.9))
                .multilineTextAlignment(.trailing)
        }
        .padding(.vertical, 3)
    }

    private func note(_ text: String) -> some View {
        Text(text)
            .font(.caption)
            .foregroundStyle(.white.opacity(0.45))
            .padding(.top, 18)
    }

    // ---- The figures --------------------------------------------------------

    private var nerd: NerdStatsRec? { controller.settingsAudioReadout.loaded && controller.dolbyRenderer == nil ? controller.nerd : nil }
    private var device: OutputDeviceRec { controller.outputDevice }

    private var sourceName: String {
        controller.playbackSourceName ?? "—"
    }

    private var codec: String { nerd?.codec.isEmpty == false ? nerd!.codec : "—" }
    private var bitDepth: String { (nerd?.bitDepth ?? 0) > 0 ? "\(nerd!.bitDepth)-bit" : "—" }
    private var bitrate: String { (nerd?.kbps ?? 0) > 0 ? "\(nerd!.kbps) kbps" : "—" }
    private var sourceRate: String { (nerd?.sampleRate ?? 0) > 0 ? "\(nerd!.sampleRate) Hz" : "—" }
    private var claimedQuality: String {
        let metered = NetworkQuality.shared.metered
        let quality = AppSettings.shared.effectiveAudioQuality(metered: metered).name
        return "\(AudioQualityOption.label(quality, in: AudioQualityOption.stream)) requested on \(metered ? "Mobile Data" : "Wi-Fi")"
    }
    private var measuredStream: String {
        let values = [codec, bitrate, sourceRate, bitDepth].filter { $0 != "—" }
        return values.isEmpty ? "—" : values.joined(separator: " · ")
    }
    /// The last quality upgrade's measured correlation, and what it implies for
    /// the fade. The swap uses a linear gain pair, which is flat when the two
    /// encodings are identical; `10·log10((1+ρ)/2)` is how far from flat it sits
    /// at any other correlation (−0.11 dB at ρ=0.95, −1.25 dB at ρ=0.5).
    private var upgradeCorrelation: String {
        guard let rho = nerd?.swapCorrelation else { return "—" }
        let deviationDb = 10 * log10((1 + rho) / 2)
        return String(format: "ρ=%.3f (%.2f dB from flat)", rho, deviationDb)
    }
    private var channels: String {
        let n = nerd?.channels ?? 0
        guard n > 0 else { return "—" }
        return n > 2 ? "\(n) (Surround)" : "\(n)"
    }

    /// The decoder's own name rather than the codec label. `symphonia` is the
    /// decoder on every platform, so this row is the one that says which
    /// implementation is actually running — which is the question the codec row
    /// cannot answer.
    private var decoderName: String { nerd?.decoderImplementation.isEmpty == false ? nerd!.decoderImplementation : "—" }

    private var deviceRate: String {
        nativeOutputReady && device.sampleRate > 0 ? "\(device.sampleRate) Hz" : "—"
    }
    private var deviceChannels: String {
        nativeOutputReady && device.channels > 0 ? "\(device.channels)" : "—"
    }
    private var deviceName: String {
        nativeOutputReady && !device.name.isEmpty ? device.name : "Not open yet"
    }

    /// "44100 → 48000" when the two differ, and the single rate when they do not
    /// — because a resampler that is not resampling should say so rather than
    /// print an arrow to the same number twice.
    private var ioRate: String {
        let src = nerd?.sampleRate ?? 0
        let out = device.sampleRate
        guard src > 0, out > 0 else { return "—" }
        return src == out ? "\(out) Hz (no conversion)" : "\(src) → \(out) Hz"
    }

    private var resamplerType: String {
        let src = nerd?.sampleRate ?? 0
        guard src > 0, device.sampleRate > 0 else { return "—" }
        return src == device.sampleRate ? "Bypassed" : "libsoxr HQ"
    }

    private var resamplerQuality: String {
        let src = nerd?.sampleRate ?? 0
        guard src > 0, device.sampleRate > 0 else { return "—" }
        return src == device.sampleRate ? "Not applicable" : "Band-limited"
    }

    /// The word length the unit was actually opened as — the setting's answer,
    /// not its request. Upstream's panel names the negotiated encoding for the
    /// same reason: PCM_16 quantizes at the callback, FLOAT_32 passes through.
    private var nativeOutputReady: Bool {
        controller.settingsAudioReadout.loaded && controller.dolbyRenderer == nil && device.started
    }
    private var pcmFormat: String {
        guard nativeOutputReady else { return "—" }
        return AudioSettingsReadout.pcmLabel(device.sampleFormat)
    }

    private var bitPerfectStatus: String {
        controller.settingsAudioReadout.bitPerfectStatus(requested: controller.bitPerfectOutput)
    }

    /// Applied loudness correction, read off the engine rather than the
    /// switch: "Off" when the listener switched it off, "No figure" when a
    /// track carries none (local files, substitutes), "+x.x dB" otherwise.
    /// Upstream's panel makes the same three-way distinction.
    private var loudness: String {
        guard nerd?.activeStages.contains("Normalization") == true else { return "Off / no figure" }
        guard let gain = nerd?.loudnessGainDb else { return "No figure" }
        if gain == 0 { return "Unity (0 dB)" }
        return String(format: "%+.1f dB", gain)
    }

    private var equalizerEnabled: Bool {
        nerd?.activeStages.contains("Manual EQ / tone") == true
    }
    private var spatial: Bool {
        nerd?.activeStages.contains("Spatial / head tracking") == true
    }
    private var eqPreset: String {
        guard equalizerEnabled else { return "Off" }
        let preset = PlatformSettings.shared.getString(key: "equalizer_preset", default: "")
        return preset.isEmpty ? "Custom" : preset
    }

    /// The one computed row: engine-side sample processing and its limits.
    ///
    /// A resampler is only a lossless pass-through when the rates already agree,
    /// which is why it is asked about the two numbers rather than the setting —
    /// upstream makes the same distinction, and a panel that claimed bit-exactness
    /// because no DSP was switched on would be wrong for every 44.1 kHz track on
    /// a 48 kHz route, which is most of them.
    private var samplePathVerdict: String {
        if controller.dolbyRenderer != nil { return "Native PCM processing is bypassed by Apple’s renderer" }
        guard nativeOutputReady else { return "—" }
        if device.bitPerfectActive {
            return "Yes — exclusive integer PCM; engine DSP and system mixing bypassed"
        }
        if PlatformSettings.shared.getBoolean(key: "bit_perfect_output", default: false) {
            return "No — \(device.bitPerfectReason)"
        }
        let src = nerd?.sampleRate ?? 0
        let resampling = src > 0 && device.sampleRate > 0 && src != device.sampleRate
        if nerd?.activeStages.isEmpty == false { return "Processing active: \(nerd!.activeStages.joined(separator: ", "))" }
        if (nerd?.protectionInterventions ?? 0) > 0 { return "Overload protection intervened" }
        if let gain = nerd?.loudnessGainDb, gain != 0 { return "No — loudness correction active" }
        if device.sampleFormat == "PCM_16" { return "No — 16-bit quantization at the output" }
        if resampling { return "No — resampling \(src) → \(device.sampleRate) Hz" }
        return "No conversion in engine; CoreAudio path unverified"
    }
}

/// Upstream `AudioOutputSheet`: the summary row that opens [AudioPipelineSheet].
///
/// The summary is the part worth getting right on its own, because it is what
/// is visible without opening anything: "Float32 · 48 kHz", or the device name
/// once there is one. Upstream shows the encoding and rate, whichever it knows.
struct AudioOutputRow: View {
    @Environment(PlaybackController.self) private var controller
    let onOpen: () -> Void

    var body: some View {
        Button(action: onOpen) {
            HStack(spacing: 13) {
                Image(systemName: "slider.horizontal.3")
                    .scaledFont(size: 16, weight: .medium)
                    .foregroundStyle(.white.opacity(0.7))
                    .frame(width: 40, height: 40)
                    .background(.white.opacity(0.08), in: Circle())
                VStack(alignment: .leading, spacing: 2) {
                    Text("Audio Pipeline")
                        .font(.body)
                        .foregroundStyle(.white.opacity(0.85))
                        .lineLimit(1)
                    Text(summary)
                        .font(.caption)
                        .foregroundStyle(.white.opacity(0.55))
                        .lineLimit(1)
                }
                Spacer()
                Image(systemName: "chevron.right")
                    .scaledFont(size: 13, weight: .semibold)
                    .foregroundStyle(.white.opacity(0.35))
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    /// "PCM 16 · 48 kHz" or "Float32 · 48 kHz", or the device name once the
    /// engine is open. Whichever is actually known: upstream shows the
    /// encoding and the rate and falls back to the encoding alone, and there
    /// is no reason to show a rate the device has not reported.
    private var summary: String {
        let device = controller.outputDevice
        guard controller.settingsAudioReadout.loaded else { return "Not open for the selected track" }
        if controller.dolbyRenderer != nil { return "Apple Dolby renderer" }
        guard device.started else { return "Nothing open yet" }
        var parts: [String] = [AudioSettingsReadout.pcmLabel(device.sampleFormat)]
        if device.sampleRate > 0 {
            let khz = Double(device.sampleRate) / 1000
            parts.append(String(format: khz == khz.rounded() ? "%.0f kHz" : "%.1f kHz", khz))
        }
        return parts.joined(separator: " · ")
    }

}

/// Upstream `AudioOutputSheet`: current outputs, AirPlay, app volume, then the
/// live pipeline. Output routes are read-only because Apple's system picker
/// owns route selection.
struct OutputDeviceSheet: View {
    @Environment(PlaybackController.self) private var controller
    @Environment(\.dismiss) private var dismiss
    @State private var showPipeline = false
    @State private var routeRevision = 0

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TimelineView(.periodic(from: .now, by: 1)) { _ in
                        ForEach(routeRows) { route in
                            HStack(spacing: 12) {
                                Image(systemName: route.symbol)
                                    .foregroundStyle(.secondary)
                                    .frame(width: 24)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(route.name)
                                    if !route.kind.isEmpty {
                                        Text(route.kind)
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                    }
                                }
                                Spacer()
                                Image(systemName: "checkmark")
                                    .foregroundStyle(.tint)
                                    .accessibilityLabel("Playing")
                            }
                            .accessibilityElement(children: .combine)
                        }
                    }
                    AirPlayOutputRow()
                } header: {
                    Text("Audio Output")
                }
                Section {
                    HStack(spacing: 12) {
                        Image(systemName: controller.volume == 0 ? "speaker.slash.fill" : "speaker.fill")
                            .foregroundStyle(.secondary)
                            .frame(width: 24)
                        Slider(value: Binding(
                            get: { controller.volume },
                            set: { controller.volume = $0 }
                        ), in: 0...1)
                        .accessibilityLabel("Volume")
                        Text("\(Int(controller.volume * 100))%")
                            .monospacedDigit()
                            .foregroundStyle(.secondary)
                            .frame(minWidth: 42, alignment: .trailing)
                    }
                } header: {
                    Text("Volume")
                }
                Section {
                    Button { showPipeline = true } label: {
                        HStack(spacing: 12) {
                            Image(systemName: "slider.horizontal.3")
                                .foregroundStyle(.secondary)
                                .frame(width: 24)
                            VStack(alignment: .leading, spacing: 2) {
                                Text("Audio Pipeline")
                                    .foregroundStyle(.primary)
                                TimelineView(.periodic(from: .now, by: 1)) { _ in
                                    Text(pipelineSummary)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                        .lineLimit(1)
                                }
                            }
                            Spacer()
                            Image(systemName: "chevron.right")
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(.tertiary)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
                TimelineView(.periodic(from: .now, by: 1)) { _ in
                    Section {
                        row("Buffered", "\(controller.outputHealth.bufferedFrames) frames")
                        row("Silent callbacks", "\(controller.outputHealth.callbackUnderruns)")
                        row("Output rebuilds", "\(controller.outputHealth.outputRebuilds)")
                        row("Device xruns", "\(controller.outputHealth.outputXruns)")
                        row("Output peak", String(format: "%.4f", controller.outputHealth.outputPeak))
                        row("Upgrade correlation", upgradeCorrelation)
                    } header: {
                        Text("Playback Diagnostics")
                    } footer: {
                        Text("Compare counter changes while a track plays. The output callback also runs with an empty queue, so idle silent callbacks are expected.")
                    }
                }
            }
            .formStyle(.grouped)
            .navigationTitle("Audio Output")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
        }
        #if os(macOS)
        .frame(minWidth: 420, minHeight: 380)
        #endif
        .sheet(isPresented: $showPipeline) {
            AudioPipelineSheet()
                .environment(controller)
        }
        #if os(iOS)
        .onReceive(NotificationCenter.default.publisher(for: AVAudioSession.routeChangeNotification)) { _ in
            routeRevision &+= 1
            AudioRouteState.shared.refresh()
        }
        #endif
    }

    private func row(_ label: String, _ value: String) -> some View {
        HStack {
            Text(label).foregroundStyle(.secondary)
            Spacer(minLength: 12)
            Text(value).monospacedDigit().multilineTextAlignment(.trailing)
        }
    }

    private var device: OutputDeviceRec { controller.outputDevice }
    private var nerd: NerdStatsRec? { controller.settingsAudioReadout.loaded && controller.dolbyRenderer == nil ? controller.nerd : nil }

    private var upgradeCorrelation: String {
        guard let rho = nerd?.swapCorrelation else { return "—" }
        let deviationDb = 10 * log10((1 + rho) / 2)
        return String(format: "ρ=%.3f (%.2f dB from flat)", rho, deviationDb)
    }

    private var routeRows: [OutputRouteRowModel] {
        _ = routeRevision
        #if os(iOS)
        let outputs = AVAudioSession.sharedInstance().currentRoute.outputs
        if outputs.isEmpty {
            let route = AudioRouteState.shared
            return [OutputRouteRowModel(name: route.name, kind: route.kind, symbol: "speaker.wave.2.fill")]
        }
        return outputs.map { output in
            OutputRouteRowModel(
                name: output.portName,
                kind: outputLabel(output.portType),
                symbol: outputSymbol(output.portType)
            )
        }
        #else
        let route = MacAudioRoutes.current()
        return [OutputRouteRowModel(name: route.name, kind: route.kind, symbol: "speaker.wave.2.fill")]
        #endif
    }
    private var pipelineSummary: String {
        guard controller.settingsAudioReadout.loaded else { return "Not open for the selected track" }
        if controller.dolbyRenderer != nil { return "Apple Dolby renderer" }
        guard device.started else { return "—" }
        var parts = [AudioSettingsReadout.pcmLabel(device.sampleFormat)]
        if device.sampleRate > 0 { parts.append("\(device.sampleRate) Hz") }
        return parts.joined(separator: " · ")
    }

    #if os(iOS)
    private func outputLabel(_ type: AVAudioSession.Port) -> String {
        switch type {
        case .builtInSpeaker: String(localized: "Speaker")
        case .headphones, .headsetMic, .lineOut: String(localized: "Headphones")
        case .bluetoothA2DP, .bluetoothLE, .bluetoothHFP: String(localized: "Bluetooth")
        case .airPlay: String(localized: "AirPlay")
        case .usbAudio: String(localized: "USB Audio")
        case .HDMI: String(localized: "HDMI")
        case .carAudio: String(localized: "CarPlay")
        case .builtInReceiver: String(localized: "Receiver")
        default: String(localized: "Output")
        }
    }

    private func outputSymbol(_ type: AVAudioSession.Port) -> String {
        switch type {
        case .builtInSpeaker: "speaker.wave.2.fill"
        case .builtInReceiver: "phone"
        case .headphones, .headsetMic, .lineOut: "headphones"
        case .bluetoothA2DP, .bluetoothLE, .bluetoothHFP: "airpodspro"
        case .airPlay: "airplayaudio"
        case .usbAudio, .HDMI: "hifispeaker.fill"
        case .carAudio: "car.fill"
        default: "hifispeaker"
        }
    }
    #endif

}

private struct OutputRouteRowModel: Identifiable {
    let name: String
    let kind: String
    let symbol: String
    var id: String { "\(kind):\(name)" }
}

private struct AirPlayOutputRow: View {
    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "airplayaudio")
                .foregroundStyle(.secondary)
                .frame(width: 24)
            Text("AirPlay")
            Spacer()
            SystemRoutePickerView()
                .frame(width: 44, height: 36)
            .accessibilityLabel("AirPlay")
        }
        .accessibilityElement(children: .contain)
    }
}

#if os(iOS)
private struct SystemRoutePickerView: UIViewRepresentable {
    func makeUIView(context: Context) -> AVRoutePickerView {
        let view = AVRoutePickerView()
        view.activeTintColor = .systemBlue
        return view
    }

    func updateUIView(_ uiView: AVRoutePickerView, context: Context) {}
}

#else
private struct SystemRoutePickerView: NSViewRepresentable {
    func makeNSView(context: Context) -> AVRoutePickerView {
        AVRoutePickerView()
    }

    func updateNSView(_ nsView: AVRoutePickerView, context: Context) {}
}
#endif
