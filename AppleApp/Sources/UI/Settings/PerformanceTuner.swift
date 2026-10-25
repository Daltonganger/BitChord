import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// Holds the display at a sustained high refresh rate while High Performance
/// Mode is on, by keeping a display link alive that requests the chosen
/// frame-rate range.
///
/// A retained link rather than a one-shot request: the range is a property of a
/// live link, so with nothing holding one the system falls back to its own
/// policy. Invalidated the moment the mode turns off, which is what hands the
/// policy back. The system still clamps to what the panel can do — asking for
/// 120 Hz on a 60 Hz screen is a request, not a mode switch.
final class PerformanceTuner {
    static let shared = PerformanceTuner()

    /// The rates the picker offers. Fixed rather than queried off the panel:
    /// the system clamps the request to what the hardware can do, so offering a
    /// rate the screen cannot reach degrades to the screen's own maximum.
    static let supportedRates = [60, 90, 120]

    #if os(iOS)
    private var link: CADisplayLink?
    #endif

    func apply(highPerformance: Bool, refreshRateHz: Int) {
        #if os(iOS)
        link?.invalidate()
        link = nil
        guard highPerformance else { return }
        let clamped = min(max(refreshRateHz, 30), 240)
        let link = CADisplayLink(target: self, selector: #selector(tick))
        if #available(iOS 15.0, *) {
            let max = Float(clamped)
            link.preferredFrameRateRange = CAFrameRateRange(minimum: 30, maximum: max, preferred: max)
        } else {
            link.preferredFramesPerSecond = min(clamped, 60)
        }
        link.add(to: .main, forMode: .common)
        self.link = link
        #else
        // macOS varies the refresh itself (ProMotion is the panel's own
        // policy); there is no per-app hold to request, so the persisted key is
        // the whole of the setting there.
        _ = (highPerformance, refreshRateHz)
        #endif
    }

    #if os(iOS)
    @objc private func tick() {}
    #endif
}
