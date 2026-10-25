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

/// Bridge callback adapter for StreamDownloadBridge.
final class DownloadCallbackAdapter: StreamDownloadBridgeDownloadCallback {
    private let onResult: (String?, String?) -> Void

    init(onResult: @escaping (String?, String?) -> Void) {
        self.onResult = onResult
    }

    func onResult(path: String?, message: String?) {
        onResult(path, message)
    }
}

/// Bridge callback adapter for LyricsBridge.fetchAttributed.
final class AttributedLyricsAdapter: LyricsBridgeAttributedLyricsCallback {
    private let handler: (String, String, [LyricLineDto]) -> Void
    init(_ handler: @escaping (String, String, [LyricLineDto]) -> Void) {
        self.handler = handler
    }
    func onResult(source: String, sourceLabel: String, lines: [LyricLineDto]) {
        handler(source, sourceLabel, lines)
    }
}

/// Bridge callback adapter for LyricsBridge.fetchWithSourceStates.
final class SourceStateAdapter: LyricsBridgeSourceStateCallback {
    private let handler: (String, String, LyricsBridge.LyricsProviderState, [LyricLineDto]?) -> Void
    init(_ handler: @escaping (String, String, LyricsBridge.LyricsProviderState, [LyricLineDto]?) -> Void) {
        self.handler = handler
    }
    func onSourceState(source: String, sourceLabel: String, state: LyricsBridge.LyricsProviderState, lines: [LyricLineDto]) {
        handler(source, sourceLabel, state, lines.isEmpty ? nil : lines)
    }
}

final class CustomStreamAdapter: SourceBridgeStreamCallback {
    private let handler: (String?, String?) -> Void
    init(_ handler: @escaping (String?, String?) -> Void) { self.handler = handler }
    func onResult(json: String?, message: String?) { handler(json, message) }
}

final class AuthAdapter: ScrobbleBridgeAuthCallback {
    private let handler: (Bool, String?) -> Void
    init(_ handler: @escaping (Bool, String?) -> Void) { self.handler = handler }
    func onResult(ok: Bool, message: String?) { handler(ok, message) }
}

final class CanvasAdapter: CanvasBridgeCanvasCallback {
    private let handler: (String?) -> Void
    init(_ handler: @escaping (String?) -> Void) { self.handler = handler }
    func onResult(json: String?) { handler(json) }
}

final class AutoPlayAdapter: AutoPlayBridgeAutoPlayCallback {
    private let handler: (String?, String?) -> Void
    init(_ handler: @escaping (String?, String?) -> Void) { self.handler = handler }
    func onResult(json: String?, message: String?) { handler(json, message) }
}
