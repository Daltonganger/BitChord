import Foundation
import BitChordShared

// The session-expiry matcher, through the same types the screens report with.
//
// The host flips to signed-out only when a bridge failure is the store's
// "session is gone" refusal, and it reads that off `localizedDescription`. A
// plain `Error` does not put its message there — it reports "The operation
// couldn't be completed… (error 1.)" — which is exactly how the search site
// silently stopped matching. This exercises the real `SearchError` and the
// shared matcher, and pins the plain-Error behaviour as the thing not to
// reintroduce.

var failures = 0
var checks = 0

func check(_ label: String, _ ok: Bool, _ detail: String = "") {
    checks += 1
    if ok {
        print("  ok   \(label)\(detail.isEmpty ? "" : " — \(detail)")")
    } else {
        failures += 1
        print("  FAIL \(label)\(detail.isEmpty ? "" : " — \(detail)")")
    }
}

/// The exact text the store's `AuthenticationException` carries.
let expired = "Your YouTube Music session has expired. Sign in again."

// The shared item contains a bearer credential, so coverage uses only this
// synthetic value and reports equality as a Boolean; it never prints payloads.
let sharedFixture = SharedSessionCredential(
    schemaVersion: SharedSessionCredential.currentSchemaVersion,
    cookie: "fake-session-cookie",
    accountName: "Test Listener",
    email: "listener@example.invalid",
    avatarURL: nil,
    pageId: "fake-page",
    dataSyncId: "fake-channel",
    authUser: "0",
    deviceName: "Test iPad",
    createdAt: Date(timeIntervalSinceReferenceDate: 0)
)
let encodedSharedFixture = sharedFixture.encoded()
let decodedSharedFixture = SharedSessionCredential.decode(encodedSharedFixture)
check("shared session payload round-trips a synthetic credential",
      decodedSharedFixture == sharedFixture)
check("shared payload can be adopted as a validated capture",
      decodedSharedFixture?.signInCapture.cookie == "fake-session-cookie"
        && decodedSharedFixture?.signInCapture.loggedIn == true)
check("unsupported shared payload schema is rejected",
      SharedSessionCredential.decode("{\"schemaVersion\":99,\"cookie\":\"fake\"}") == nil)
check("empty shared credentials are not adoptable",
      !SharedSessionCredential(
        schemaVersion: SharedSessionCredential.currentSchemaVersion,
        cookie: "", accountName: "Test Listener", email: "listener@example.invalid",
        avatarURL: nil, pageId: nil, dataSyncId: nil, authUser: nil,
        deviceName: "Test iPad", createdAt: Date(timeIntervalSinceReferenceDate: 0)
      ).isAdoptable)

// 1. The app's search error carries its message once it is a LocalizedError.
let searchError = InnertubeSearch.SearchError(message: expired)
check("SearchError.localizedDescription carries the message",
      searchError.localizedDescription == expired,
      searchError.localizedDescription)

// 2. The shared matcher accepts exactly that text.
check("the shared matcher accepts the store's refusal",
      AuthBridge.shared.isSessionExpired(message: searchError.localizedDescription))
check("the shared matcher accepts the raw text",
      AuthBridge.shared.isSessionExpired(message: expired))

// 3. An ordinary failure is not a refusal.
let network = NSError(
    domain: "BitChordTests", code: -1009,
    userInfo: [NSLocalizedDescriptionKey: "Couldn't reach the music service. Check your connection and try again."]
)
check("a network failure does not match",
      !AuthBridge.shared.isSessionExpired(message: network.localizedDescription),
      network.localizedDescription)

// 4. The guard for the bug: a plain Error does not carry its message, which is
//    why every bridge error the screens report has to be a LocalizedError.
struct PlainError: Error { let message: String }
let plain = PlainError(message: expired)
check("a plain Error would not carry the text (why LocalizedError matters)",
      plain.localizedDescription != expired,
      plain.localizedDescription)

print(failures == 0 ? "\nall \(checks) checks passed" : "\n\(failures) of \(checks) checks FAILED")
exit(failures == 0 ? 0 : 1)
