import Foundation

/// The queue identifies a recording; the resolved origin identifies the stream actually used.
enum PlaybackSourceLabel {
    static func name(origin: String?, source: String, isLocal: Bool) -> String? {
        switch origin {
        case "substitute": return "Alternative source"
        case "youtube": return "YouTube"
        case "cache": return "Cached audio"
        case "local": return "Local files"
        default:
            if isLocal { return "Local files" }
            if source.hasPrefix("yt:") { return "YouTube" }
            if source.hasPrefix("src:") { return "Add-on" }
            if source.hasPrefix("saavn:") { return "JioSaavn" }
            return nil
        }
    }
}
