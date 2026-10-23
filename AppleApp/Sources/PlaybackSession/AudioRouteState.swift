import AVFoundation
import Observation
import BitChordShared
#if os(iOS)
import UIKit
#endif

@MainActor @Observable
final class AudioRouteState {
    static let shared = AudioRouteState()
    private(set) var name = "System output"
    private(set) var kind = ""
    private(set) var spatialCapable = false
    private(set) var monoAudioEnabled = false
    /// Capability alone does not establish whether Apple is spatializing audio.
    private(set) var systemSpatialEnabled: Bool?
    private var uid = ""

    func refresh(systemSpatialEnabled reported: Bool? = nil) {
        #if os(iOS)
        monoAudioEnabled = UIAccessibility.isMonoAudioEnabled
        let port = AVAudioSession.sharedInstance().currentRoute.outputs.first
        let newUID = port?.uid ?? ""
        if newUID != uid { systemSpatialEnabled = nil; uid = newUID }
        name = port?.portName ?? "System output"
        kind = port?.portType.rawValue ?? ""
        spatialCapable = port?.isSpatialAudioEnabled ?? false
        if let reported { systemSpatialEnabled = reported }
        if !spatialCapable { systemSpatialEnabled = false }
        #else
        let route = MacAudioRoutes.current()
        name = route.name; kind = route.kind
        // macOS does not expose AVAudioSession's confirmed spatial state.
        systemSpatialEnabled = nil
        #endif
        // Addons that declare `checkValidLossless` sit out while this is false.
        PlaybackCodecCapabilities.shared.setLosslessOutput(value: carriesLossless)
    }

    /// Whether this route can carry lossless audio. Bluetooth (AAC/SBC) is the
    /// one common route that cannot; wired, USB, built-in, HDMI and AirPlay can.
    var carriesLossless: Bool {
        let lowered = kind.lowercased()
        return !lowered.contains("bluetooth")
    }

    var spatialDescription: String {
        if let systemSpatialEnabled { return systemSpatialEnabled ? "Enabled by system" : "Off" }
        return spatialCapable ? "Available · System setting unknown" : "Not reported"
    }
    var permitsCustomSpatial: Bool { systemSpatialEnabled != true && !monoAudioEnabled }
}
