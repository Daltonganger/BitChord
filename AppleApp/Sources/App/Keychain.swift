import Foundation
import Security

/// One Keychain accessor, shared by the YouTube session cookie and every other
/// credential the app holds.
///
/// The port previously had [AuthStore] for the cookie and nothing for the rest,
/// so `discord_token` sat in `NSUserDefaults` — plain text, included in backups,
/// and readable by anything with access to the container. Upstream keeps the
/// Discord token, the YouTube cookie, and the WebDAV and SMB passwords together
/// in `EncryptedSharedPreferences` and excludes them from its backup export; this
/// is the same policy on the platform's equivalent facility.
///
/// Accessibility is `AfterFirstUnlockThisDeviceOnly` rather than the
/// generic-password default of `WhenUnlocked`, so a credential is still readable
/// to the background playback path on a device that has been locked since boot.
/// The `ThisDeviceOnly` half keeps it out of iCloud Keychain and out of any
/// backup that would restore it to a different device.
enum Keychain {
    private static let namespace: String = {
        #if DEBUG
        // The muted CLI diagnostic has its own container and credentials.
        if let identifier = Bundle.main.bundleIdentifier,
           identifier.hasPrefix("app.bitchord.BitChord.queuevalidation") {
            return identifier
        }
        #endif
        return "app.bitchord.BitChord"
    }()
    private static let service = namespace + ".credentials"
    private static let synchronizableService = namespace

    /// Read a value, or nil when there is none.
    ///
    /// Any failure — a locked Keychain, a malformed item, an OSStatus this code
    /// does not recognise — answers nil rather than throwing, so a credential
    /// that cannot be read degrades to "not signed in" instead of taking the app
    /// down on launch.
    static func get(_ account: String) -> String? {
        read(account).value
    }

    /// A failed read and a missing item require different persistence decisions.
    static func read(_ account: String) -> (value: String?, available: Bool) {
        var result: AnyObject?
        var query = baseQuery(account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return (nil, true) }
        guard status == errSecSuccess, let data = result as? Data,
              let value = String(data: data, encoding: .utf8) else { return (nil, false) }
        return (value, true)
    }

    /// Replace an existing value without a delete/add window that can lose the
    /// previous session if the second operation fails.
    @discardableResult
    static func put(_ account: String, _ value: String?) -> Bool {
        guard let value, !value.isEmpty else { return clear(account) }
        let data = Data(value.utf8)
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        let update = SecItemUpdate(baseQuery(account) as CFDictionary, attributes as CFDictionary)
        if update == errSecSuccess { return true }
        guard update == errSecItemNotFound else {
            NSLog("[BitChord] Keychain update failed for %@: %d", account, update)
            return false
        }
        var item = baseQuery(account)
        item[kSecValueData as String] = data
        item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        item[kSecAttrSynchronizable as String] = false
        let added = SecItemAdd(item as CFDictionary, nil)
        if added == errSecSuccess { return true }
        NSLog("[BitChord] Keychain add failed for %@: %d", account, added)
        return false
    }

    @discardableResult
    static func clear(_ account: String) -> Bool {
        let status = SecItemDelete(baseQuery(account) as CFDictionary)
        if status == errSecSuccess || status == errSecItemNotFound { return true }
        NSLog("[BitChord] Keychain delete failed for %@: %d", account, status)
        return false
    }

    /// Read the iCloud-synchronizable session item. It has a separate service
    /// and accessibility class from the local playback cookie.
    static func getSynchronizable(_ account: String) -> String? {
        var query = synchronizableQuery(account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: AnyObject?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data
        else { return nil }
        return String(data: data, encoding: .utf8)
    }

    /// Upsert a synchronizable item using an accessibility class supported by
    /// iCloud Keychain. Errors intentionally carry no credential data.
    @discardableResult
    static func putSynchronizable(_ account: String, _ value: String) -> Bool {
        let data = Data(value.utf8)
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlock,
        ]
        let update = SecItemUpdate(synchronizableQuery(account) as CFDictionary, attributes as CFDictionary)
        if update == errSecSuccess { return true }
        guard update == errSecItemNotFound else { return false }
        var item = synchronizableQuery(account)
        item[kSecValueData as String] = data
        item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
        let added = SecItemAdd(item as CFDictionary, nil)
        guard added == errSecDuplicateItem else { return added == errSecSuccess }
        return SecItemUpdate(synchronizableQuery(account) as CFDictionary, attributes as CFDictionary) == errSecSuccess
    }

    @discardableResult
    static func clearSynchronizable(_ account: String) -> Bool {
        let status = SecItemDelete(synchronizableQuery(account) as CFDictionary)
        // No iCloud Keychain capability means no shared item exists to clear;
        // local sign-out must continue normally in that case.
        return status == errSecSuccess || status == errSecItemNotFound
            || status == errSecNotAvailable || status == errSecMissingEntitlement
    }

    /// Whether an item exists but cannot be read yet — a device that has not been
    /// unlocked since boot.
    ///
    /// Worth distinguishing from "no credential": they are different states, and
    /// treating a locked device as a signed-out one silently drops a perfectly
    /// good session and browses as a guest until the next launch.
    static func isLocked(_ account: String) -> Bool {
        var query = baseQuery(account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: AnyObject?
        return SecItemCopyMatching(query as CFDictionary, &result) == errSecInteractionNotAllowed
    }

    private static func baseQuery(_ account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    private static func synchronizableQuery(_ account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: synchronizableService,
            kSecAttrAccount as String: account,
            kSecAttrSynchronizable as String: true,
        ]
    }
}
