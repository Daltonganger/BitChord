import Foundation
import BitChordShared
import os
#if os(iOS)
import UIKit
#if DEBUG
import AVFoundation
#endif
#else
import AppKit
#endif

/// Serial background analysis avoids a burst of decodes after rapid skips.
actor AutomixQueueRanker {
    static let shared = AutomixQueueRanker()
    func rank(currentPath: String, candidatePaths: [String], currentText: String,
              candidateTexts: [String], crossfadeSeconds: Double, skipVocals: Bool) -> [UInt32] {
        guard !Task.isCancelled else { return [] }
        return rankAutomixCandidates(
            currentPath: currentPath, candidatePaths: candidatePaths,
            currentText: currentText, candidateTexts: candidateTexts,
            crossfadeSeconds: crossfadeSeconds, skipVocals: skipVocals
        )
    }
}
