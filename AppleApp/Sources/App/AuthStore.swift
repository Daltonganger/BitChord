import Foundation
import Security
import BitChordShared

/// Encrypted-at-rest storage for the YouTube Music session cookie.
///
/// Upstream uses `EncryptedSharedPreferences`. On Apple the equivalent is the
/// Keychain, not `UserDefaults` — see [Keychain], which this is the cookie's
/// caller of rather than a second implementation of.
enum AuthStore {
    private static let account = "ytmusic.cookie"
    private static let sharedAccount = "ytmusic.session.shared"

    static var cookie: String? {
        get { Keychain.get(account) }
    }

    @discardableResult
    static func save(_ cookie: String) -> Bool { Keychain.put(account, cookie) }

    /// Whether a cookie exists but cannot be read yet — a device that has not
    /// been unlocked since boot. Distinguishable from "not signed in", and worth
    /// distinguishing: they call for opposite actions.
    static var isLocked: Bool {
        Keychain.isLocked(account) || Keychain.isLocked("account_sessions")
    }

    /// Drop the legacy mirror before removing the account record at sign-out.
    /// A failed delete must not masquerade as a durable sign-out.
    @discardableResult
    static func clear() -> Bool { Keychain.clear(account) }

    static var sharedSession: SharedSessionCredential? {
        SharedSessionCredential.decode(Keychain.getSynchronizable(sharedAccount))
    }

    @discardableResult
    static func saveSharedSession(_ credential: SharedSessionCredential) -> Bool {
        guard let encoded = credential.encoded() else { return false }
        guard Keychain.putSynchronizable(sharedAccount, encoded) else { return false }
        // Read back through the synchronizable namespace before treating the
        // upsert as durable; unavailable Security status leaves sharing inert.
        return Keychain.getSynchronizable(sharedAccount) == encoded
    }

    @discardableResult
    static func clearSharedSession() -> Bool {
        Keychain.clearSynchronizable(sharedAccount)
    }
}

/// Wires the shared module's Keychain seam to [Keychain] at launch.
///
/// The Discord bearer token, the WebDAV and SMB passwords, and an addon's base
/// URL — which on this protocol can carry a token in its path — all live here
/// rather than in `UserDefaults`. That is upstream's policy, not a preference:
/// its backup export carries a `SECRETS` exclusion list precisely because those
/// values are otherwise exported, and its `EncryptedSharedPreferences` exists so
/// they are not sitting in plain text in the first place.
enum SecretStoreWiring {
    static func install() {
        SecretStoreBridge.shared.setImpl(value: Impl())
    }

    private final class Impl: SecretStoreBridgeImpl {
        func get(key: String) -> String? { Keychain.get(key) }

        func put(key: String, value: String?) { Keychain.put(key, value) }
    }
}
