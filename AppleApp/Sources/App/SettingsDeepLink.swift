import SwiftUI
import Combine
import BitChordShared

/// Carries an addressable Settings page from the scene URL handler to the
/// settings window, which may not exist yet when the URL arrives.
enum SettingsDeepLink {
    static let requestKey = "settings_deep_link_request"
    static let requested = Notification.Name("SettingsDeepLinkRequested")

    /// Handles any settings URL, defaulting to the root group for bare or unknown pages.
    /// Hyphens and underscores are accepted so section names remain readable in URLs.
    @discardableResult
    static func handle(_ url: URL) -> Bool {
        guard url.scheme?.lowercased() == "bitchord",
              url.host?.lowercased() == "settings" else { return false }
        let page = url.pathComponents.dropFirst().first
        let section = page.flatMap(SettingsSectionName.canonical) ?? SettingsView.SettingsSection.account.rawValue

        PlatformSettings.shared.putString(key: requestKey, value: section)
        NotificationCenter.default.post(name: requested, object: section)
        return true
    }
}

/// Opens the macOS Settings scene after the app-level URL handler has queued a
/// destination. Keeping the action in a view gives it the scene's public
/// `openSettings` environment action instead of reaching for AppKit selectors.
#if os(macOS)
struct SettingsDeepLinkOpener: View {
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        Color.clear
            .frame(width: 0, height: 0)
            .onReceive(NotificationCenter.default.publisher(for: SettingsDeepLink.requested)) { _ in
                openSettings()
            }
            .accessibilityHidden(true)
    }
}
#endif

/// URL spelling for the Settings groups. This small lookup is shared by the
/// scene intake and the view that scrolls to the selected group.
enum SettingsSectionName {
    static func canonical(_ component: String) -> String? {
        let key = component.lowercased().filter { $0.isLetter || $0.isNumber }
        let aliases = ["localmusic": "downloads", "storage": "downloads", "performance": "advanced", "miscellaneous": "playback", "audio": "audioQuality"]
        if let section = aliases[key] { return section }
        return SettingsView.SettingsSection.allCases.first {
            $0.rawValue.lowercased().filter { $0.isLetter || $0.isNumber } == key
        }?.rawValue
    }
}
