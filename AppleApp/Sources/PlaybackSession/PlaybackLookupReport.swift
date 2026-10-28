import Foundation

/// One lookup's explanation; concurrent provider callbacks never share a global error.
final class PlaybackLookupReport: @unchecked Sendable {
    private let lock = NSLock()
    private var note: String?
    func record(_ value: String) { lock.withLock { note = value } }
    var explanation: String? { lock.withLock { note } }
}
