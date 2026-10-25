import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
#if os(iOS)
import UIKit
#else
import AppKit
#endif

// Account: sign-in, Listen Together and the party badge.
extension SettingsView {
    func buildAccountRows(_ rows: inout SettingsRows) {
        rows.row("Account & Integrations", "sign in", "login", "google", "account", "profile",
                 "discord", "rich presence", "scrobbling", "last.fm", "lastfm", "listenbrainz",
                 "spotify", "sign out", "integrations", "primary artist", "album artist",
                 "track artist", "scrobble artist") {
            PushLink {
                AccountIntegrationsView(loginPresented: $loginPresented, discordPresented: $discordPresented)
            } label: {
                SettingsLine(
                    glyph: .person,
                    title: "Account & Integrations",
                    subtitle: accountSubtitle
                ) {
                    EmptyView()
                }
            }
        }
    }

    private var accountSubtitle: String {
        // "Session saved, not restored" rather than "Not signed in", because the
        // two call for opposite actions and this row is where the listener looks
        // to find out which one they are in.
        if let reason = auth.sessionUnavailableReason { return reason }
        if let email = auth.accountEmail, !email.isEmpty { return email }
        if let name = auth.accountName, !name.isEmpty { return name }
        return auth.signedIn ? "Signed in" : "Not signed in"
    }

    /// What the Listen Together row says.
    ///
    /// Says what the feature is rather than how to start it, because a settings row
    /// is a label for a thing and not a call to action — the screen it opens is where
    /// the button is.
    var listenTogetherSubtitle: String {
        let party = PartyStore.shared
        if party.inParty { return "In a party · \(party.code)" }
        return "Share a code and play the same music, in time"
    }

    /// How many are listening, when it is more than one person.
    ///
    /// A count of one is not news — a party of one is a person alone, which is what
    /// they already know — so the badge appears only once somebody else is there.
    var partyBadge: String? {
        let party = PartyStore.shared
        let count = party.state.members.count
        return party.inParty && count > 1 ? "\(count)" : nil
    }

    // MARK: - Audio quality
}
