import Foundation

/// The only sign-in data shared through iCloud Keychain. Keep this payload
/// versioned so future fields can be added without guessing at old records.
struct SharedSessionCredential: Codable, Equatable {
    static let currentSchemaVersion = 1

    let schemaVersion: Int
    let cookie: String
    let accountName: String
    let email: String
    let avatarURL: String?
    let pageId: String?
    let dataSyncId: String?
    let authUser: String?
    let deviceName: String
    let createdAt: Date

    var isAdoptable: Bool {
        schemaVersion == Self.currentSchemaVersion
            && !cookie.isEmpty
            && (!accountName.isEmpty || !email.isEmpty)
            && !deviceName.isEmpty
    }

    var signInCapture: SignInCapture {
        SignInCapture(
            cookie: cookie,
            pageId: pageId,
            dataSyncId: dataSyncId,
            authUser: authUser,
            visitorData: nil,
            clientVersion: nil,
            loggedIn: true
        )
    }

    func encoded() -> String? {
        guard isAdoptable,
              let data = try? JSONEncoder().encode(self)
        else { return nil }
        return String(data: data, encoding: .utf8)
    }

    static func decode(_ value: String?) -> Self? {
        guard let value, let data = value.data(using: .utf8),
              let credential = try? JSONDecoder().decode(Self.self, from: data),
              credential.isAdoptable
        else { return nil }
        return credential
    }
}
