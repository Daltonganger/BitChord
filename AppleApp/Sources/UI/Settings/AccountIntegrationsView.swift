import SwiftUI
import BitChordShared
import UniformTypeIdentifiers
#if os(iOS)
import UIKit
#else
import AppKit
#endif

struct AccountIntegrationsView: View {
    @Environment(AuthController.self) private var auth
    @Environment(PlaybackController.self) private var controller
    @Binding var loginPresented: Bool
    @Binding var discordPresented: Bool

    @State private var lastFmUser = PlatformSettings.shared.getString(key: "lastfm_username", default: "")
    @State private var lastFmSession = PlatformSettings.shared.getSecret(key: "lastfm_session") ?? ""
    @State private var lastFmEnabled = PlatformSettings.shared.getBoolean(key: "lastfm_enabled", default: false)
    @State private var lastFmScrobble = PlatformSettings.shared.getBoolean(key: "lastfm_scrobble", default: true)
    @State private var lastFmNowPlaying = PlatformSettings.shared.getBoolean(key: "lastfm_nowplaying", default: true)
    @State private var lastFmKey = PlatformSettings.shared.getSecret(key: "lastfm_api_key") ?? ""
    @State private var lastFmSecret = PlatformSettings.shared.getSecret(key: "lastfm_secret") ?? ""
    @State private var listenToken = PlatformSettings.shared.getSecret(key: "listenbrainz_token") ?? ""
    @State private var listenEnabled = PlatformSettings.shared.getBoolean(key: "listenbrainz_enabled", default: false)
    @State private var discordToken = PlatformSettings.shared.getSecret(key: "discord_token") ?? ""
    @State private var discordName = PlatformSettings.shared.getString(key: "discord_username", default: "")
    @State private var discordRpc = PlatformSettings.shared.getBoolean(key: "discord_rpc_enabled", default: true)
    @State private var discordStatus = PlatformSettings.shared.getString(key: "discord_status", default: "online")
    @State private var discordActivity = PlatformSettings.shared.getString(key: "discord_activity_type", default: "listening")
    @State private var discordNameCustom = PlatformSettings.shared.getString(key: "discord_activity_name", default: "")
    @State private var discordSwap = PlatformSettings.shared.getBoolean(key: "discord_swap_title", default: false)
    @State private var discordUseDetails = PlatformSettings.shared.getBoolean(key: "discord_use_details", default: false)
    @State private var discordAdvanced = PlatformSettings.shared.getBoolean(key: "discord_advanced_mode", default: false)
    @State private var discordButton1Text = PlatformSettings.shared.getString(key: "discord_button_1_text", default: "")
    @State private var discordButton1Visible = PlatformSettings.shared.getBoolean(key: "discord_button_1_visible", default: true)
    @State private var discordButton2Text = PlatformSettings.shared.getString(key: "discord_button_2_text", default: "")
    @State private var discordButton2Visible = PlatformSettings.shared.getBoolean(key: "discord_button_2_visible", default: true)
    @State private var discordInfoDismissed = PlatformSettings.shared.getBoolean(key: "discord_info_dismissed", default: false)
    @State private var scrobbleMin = Double(PlatformSettings.shared.getInt(key: "scrobble_min_duration", default: 30))
    @State private var scrobblePercent = Double(PlatformSettings.shared.getFloat(key: "scrobble_delay_percent", default: 0.5))
    @State private var scrobbleMax = Double(PlatformSettings.shared.getInt(key: "scrobble_delay_seconds", default: 180))
    @State private var scrobblePrimary = PlatformSettings.shared.getString(key: "scrobble_primary_artist", default: "track")

    var body: some View {
        Form {
            // A session that is saved but could not be put back, said out loud.
            //
            // This is the only place it can be seen, and it matters: the failure
            // is invisible everywhere else. The account row says "Not signed in",
            // the library says signed out, and every request goes out anonymous —
            // with nothing on screen to say that a session is *there* and merely
            // unrestored, which is a different thing from never having signed in
            // and the one thing the listener cannot work out for themselves.
            if let reason = auth.sessionUnavailableReason {
                Section {
                    HStack(alignment: .top, spacing: 10) {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundStyle(.orange)
                        Text(reason)
                            .font(.callout)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.vertical, 2)
                }
            }
            Section {
                if auth.signedIn {
                    HStack(spacing: 14) {
                        accountAvatar
                        VStack(alignment: .leading, spacing: 3) {
                            Text(auth.accountName ?? "Signed in")
                                .font(.headline)
                            if let email = auth.accountEmail, !email.isEmpty {
                                Text(email)
                                    .font(.subheadline)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(.vertical, 6)

                    // Upstream's `listen_as`, and the reason the account avatar
                    // is not just a picture: a Google account can own several
                    // YouTube channels and they have separate libraries,
                    // histories and scrobbles. Worth a row of its own even with
                    // one channel, because it says which one is in effect —
                    // and with one channel there is nothing to choose, so the
                    // row is hidden rather than shown as a dead end.
                    if (auth.listeningAs?.profiles.count ?? 0) > 1 {
                        PushLink {
                            AccountProfileSheet(scopedAccountId: auth.listeningAs?.id)
                        } label: {
                            SettingsLine(
                                glyph: .listenAs,
                                title: "Listen As",
                                subtitle: auth.listeningAs?.activeProfile?.subtitle
                                    ?? auth.listeningAs?.displayName
                                    ?? "Signed in"
                            ) {
                                EmptyView()
                            }
                        }
                    }
                } else {
                    Button {
                        loginPresented = true
                    } label: {
                        SettingsLine(
                            glyph: .person,
                            title: "Sign In",
                            subtitle: "Get personalized recommendations"
                        ) {
                            EmptyView()
                        }
                    }
                    .buttonStyle(.plain)
                }
            }

            if auth.signedIn {
                Section {
                    Button("Sign Out", role: .destructive, action: auth.signOut)
                }
            }

            Section {
                Button {
                    discordPresented = true
                } label: {
                    SettingsLine(
                        glyph: .discord,
                        title: "Discord",
                        subtitle: discordSubtitle
                    ) {
                        EmptyView()
                    }
                }
                .buttonStyle(.plain)
                if !discordToken.isEmpty {
                    if !discordInfoDismissed {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("Discord has no API for an app to set your presence, so this signs in as your account and speaks its protocol. Your token is stored on this device and only ever sent to Discord — but it is your whole account, and automating one is against Discord's terms of service. Bans for presence alone aren't a thing anyone reports; it's still your call.")
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                            Button("Dismiss") { discordInfoDismissed = true }
                                .font(.footnote.weight(.semibold))
                        }
                    }
                    Toggle("Rich Presence", isOn: $discordRpc)
                    Toggle("Lead with the song", isOn: $discordUseDetails)
                    Toggle("Customise the card", isOn: $discordAdvanced)
                    if discordAdvanced {
                        Picker("Status", selection: $discordStatus) {
                            Text("Online").tag("online")
                            Text("Idle").tag("idle")
                            Text("Do Not Disturb").tag("dnd")
                            Text("Invisible").tag("invisible")
                        }
                        Picker("Activity", selection: $discordActivity) {
                            Text("Listening").tag("listening")
                            Text("Playing").tag("playing")
                            Text("Watching").tag("watching")
                            Text("Competing").tag("competing")
                        }
                        TextField("Activity name", text: $discordNameCustom)
                        Toggle("Swap title and artist", isOn: $discordSwap)
                        Toggle("First button", isOn: $discordButton1Visible)
                        TextField("First button text", text: $discordButton1Text)
                            .disabled(!discordButton1Visible)
                        Toggle("Second button", isOn: $discordButton2Visible)
                        TextField("Second button text", text: $discordButton2Text)
                            .disabled(!discordButton2Visible)
                    } else {
                        Toggle("Swap title and artist", isOn: $discordSwap)
                    }
                }
            } header: {
                Text("Rich Presence")
            } footer: {
                Text(discordAdvanced
                     ? "{song_name}, {artist_name} and {album_name} are replaced with the track. The first button opens the song on YouTube Music, the second this project."
                     : "Show what you’re playing on your Discord profile, updating as the track does.")
            }

            Section {
                Toggle(isOn: $listenEnabled) {
                    SettingsLine(
                        glyph: .listenBrainz,
                        title: "ListenBrainz",
                        subtitle: listenEnabled && !listenToken.isEmpty
                            ? "Connected"
                            : "Enter a token to enable"
                    ) {
                        EmptyView()
                    }
                }
                SecureField("User token", text: $listenToken)
                    .textContentType(.password)
                SettingsSliderRow(
                    glyph: .lastFm,
                    title: "Minimum duration",
                    subtitle: "Songs shorter than this won't scrobble",
                    valueText: "\(Int(scrobbleMin))s",
                    value: $scrobbleMin,
                    range: 10...120,
                    step: 5
                )
                SettingsSliderRow(
                    glyph: .lastFm,
                    title: "Scrobble after",
                    subtitle: "Fraction of the track before a scrobble is sent",
                    valueText: "\(Int(scrobblePercent * 100))%",
                    value: $scrobblePercent,
                    range: 0.25...0.9,
                    step: 0.05
                )
                SettingsSliderRow(
                    glyph: .lastFm,
                    title: "Scrobble delay cap",
                    subtitle: "Cap on scrobble delay in seconds",
                    valueText: "\(Int(scrobbleMax))s",
                    value: $scrobbleMax,
                    range: 30...480,
                    step: 10
                )
                Picker(selection: $scrobblePrimary) {
                    Text("Track artist").tag("track")
                    Text("Album artist").tag("album")
                } label: {
                    SettingsLine(
                        glyph: .lastFm,
                        title: "Primary Artist",
                        subtitle: scrobblePrimary == "album"
                            ? "Scrobbles the lead name alone (“A, B & C” as “A”)"
                            : "Scrobbles the full credit as the catalogue gave it"
                    ) {
                        EmptyView()
                    }
                }
            } header: {
                Text("Scrobbling")
            } footer: {
                Text("Scrobble your listens to Last.fm and ListenBrainz.")
            }

            Section {
                Toggle(isOn: $lastFmEnabled) {
                    SettingsLine(
                        glyph: .lastFm,
                        title: "Last.fm",
                        subtitle: lastFmSession.isEmpty
                            ? "Tap to sign in"
                            : "Signed in as \(lastFmUser)"
                    ) {
                        EmptyView()
                    }
                }
                if lastFmEnabled && !lastFmSession.isEmpty {
                    Toggle("Scrobble Tracks", isOn: $lastFmScrobble)
                    Toggle("Now Playing", isOn: $lastFmNowPlaying)
                    Button("Sign Out of Last.fm", role: .destructive) {
                        lastFmSession = ""
                        lastFmUser = ""
                        lastFmEnabled = false
                        AppSettings.shared.setLastFmSession(value: "")
                        AppSettings.shared.setLastFmUsername(value: "")
                    }
                } else {
                    Button("Authorise Last.fm") { controller.authoriseLastFm() }
                }
                TextField("API key", text: $lastFmKey)
                SecureField("Shared secret", text: $lastFmSecret)
            } footer: {
                Text("An API key and shared secret are required to authorise. Don’t share them.")
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Account & Integrations")
        .modifier(AccountScrobblePersist(
            lastFmEnabled: $lastFmEnabled,
            lastFmScrobble: $lastFmScrobble,
            lastFmNowPlaying: $lastFmNowPlaying,
            lastFmKey: $lastFmKey,
            lastFmSecret: $lastFmSecret,
            listenToken: $listenToken,
            listenEnabled: $listenEnabled,
            scrobblePrimary: $scrobblePrimary,
            controller: controller
        ))
        .modifier(AccountDiscordPersist(
            discordRpc: $discordRpc,
            discordStatus: $discordStatus,
            discordActivity: $discordActivity,
            discordNameCustom: $discordNameCustom,
            discordSwap: $discordSwap,
            discordUseDetails: $discordUseDetails,
            discordAdvanced: $discordAdvanced,
            discordButton1Text: $discordButton1Text,
            discordButton1Visible: $discordButton1Visible,
            discordButton2Text: $discordButton2Text,
            discordButton2Visible: $discordButton2Visible,
            discordInfoDismissed: $discordInfoDismissed,
            scrobbleMin: $scrobbleMin,
            scrobblePercent: $scrobblePercent,
            scrobbleMax: $scrobbleMax
        ))
        .onAppear {
            lastFmUser = PlatformSettings.shared.getString(key: "lastfm_username", default: "")
            lastFmSession = PlatformSettings.shared.getSecret(key: "lastfm_session") ?? ""
            discordToken = PlatformSettings.shared.getSecret(key: "discord_token") ?? ""
            discordName = PlatformSettings.shared.getString(key: "discord_username", default: "")
        }
    }

    private var discordSubtitle: String {
        if discordToken.isEmpty { return "Tap to connect" }
        if !discordRpc { return "Connected, presence off" }
        if !discordName.isEmpty { return "Sharing as @\(discordName)" }
        return "Sharing your listens"
    }

    @ViewBuilder
    private var accountAvatar: some View {
        if let url = auth.accountPhotoUrl.flatMap(URL.init(string:)) {
            AsyncImage(url: url) { image in
                image.resizable().scaledToFill()
            } placeholder: {
                SettingsGlyph(kind: .person)
            }
            .frame(width: 52, height: 52)
            .clipShape(Circle())
        } else {
            SettingsGlyph(kind: .person)
                .scaleEffect(1.4)
        }
    }
}

// MARK: - Lyrics sources

private struct AccountScrobblePersist: ViewModifier {
    @Binding var lastFmEnabled: Bool
    @Binding var lastFmScrobble: Bool
    @Binding var lastFmNowPlaying: Bool
    @Binding var lastFmKey: String
    @Binding var lastFmSecret: String
    @Binding var listenToken: String
    @Binding var listenEnabled: Bool
    @Binding var scrobblePrimary: String
    var controller: PlaybackController

    func body(content: Content) -> some View {
        content
            .onChange(of: lastFmEnabled) { _, value in
                AppSettings.shared.setLastFmEnabled(value: value)
                if value { controller.authoriseLastFm() }
            }
            .onChange(of: lastFmScrobble) { _, value in AppSettings.shared.setLastFmScrobble(value: value) }
            .onChange(of: lastFmNowPlaying) { _, value in AppSettings.shared.setLastFmNowPlaying(value: value) }
            .onChange(of: lastFmKey) { _, value in AppSettings.shared.setLastFmApiKey(value: value) }
            .onChange(of: lastFmSecret) { _, value in AppSettings.shared.setLastFmSecret(value: value) }
            .onChange(of: listenToken) { _, value in AppSettings.shared.setListenBrainzToken(value: value) }
            .onChange(of: scrobblePrimary) { _, value in controller.updateScrobblePrimaryArtist(value) }
            .onChange(of: listenEnabled) { _, value in
                if value && listenToken.isEmpty { return }
                AppSettings.shared.setListenBrainzEnabled(value: value)
            }
    }
}

private struct AccountDiscordPersist: ViewModifier {
    @Binding var discordRpc: Bool
    @Binding var discordStatus: String
    @Binding var discordActivity: String
    @Binding var discordNameCustom: String
    @Binding var discordSwap: Bool
    @Binding var discordUseDetails: Bool
    @Binding var discordAdvanced: Bool
    @Binding var discordButton1Text: String
    @Binding var discordButton1Visible: Bool
    @Binding var discordButton2Text: String
    @Binding var discordButton2Visible: Bool
    @Binding var discordInfoDismissed: Bool
    @Binding var scrobbleMin: Double
    @Binding var scrobblePercent: Double
    @Binding var scrobbleMax: Double

    func body(content: Content) -> some View {
        content
            .modifier(AccountDiscordCardPersist(
                discordRpc: $discordRpc,
                discordStatus: $discordStatus,
                discordActivity: $discordActivity,
                discordNameCustom: $discordNameCustom,
                discordSwap: $discordSwap,
                discordUseDetails: $discordUseDetails,
                discordAdvanced: $discordAdvanced
            ))
            .modifier(AccountDiscordButtonsPersist(
                discordButton1Text: $discordButton1Text,
                discordButton1Visible: $discordButton1Visible,
                discordButton2Text: $discordButton2Text,
                discordButton2Visible: $discordButton2Visible,
                discordInfoDismissed: $discordInfoDismissed,
                scrobbleMin: $scrobbleMin,
                scrobblePercent: $scrobblePercent,
                scrobbleMax: $scrobbleMax
            ))
    }
}

private struct AccountDiscordCardPersist: ViewModifier {
    @Binding var discordRpc: Bool
    @Binding var discordStatus: String
    @Binding var discordActivity: String
    @Binding var discordNameCustom: String
    @Binding var discordSwap: Bool
    @Binding var discordUseDetails: Bool
    @Binding var discordAdvanced: Bool

    func body(content: Content) -> some View {
        content
            .onChange(of: discordRpc) { _, value in AppSettings.shared.setDiscordRpcEnabled(value: value) }
            .onChange(of: discordStatus) { _, value in AppSettings.shared.setDiscordStatus(value: value) }
            .onChange(of: discordActivity) { _, value in AppSettings.shared.setDiscordActivityType(value: value) }
            .onChange(of: discordNameCustom) { _, value in AppSettings.shared.setDiscordActivityName(value: value) }
            .onChange(of: discordSwap) { _, value in AppSettings.shared.setDiscordSwapTitle(value: value) }
            .onChange(of: discordUseDetails) { _, value in AppSettings.shared.setDiscordUseDetails(value: value) }
            .onChange(of: discordAdvanced) { _, value in AppSettings.shared.setDiscordAdvancedMode(value: value) }
    }
}

private struct AccountDiscordButtonsPersist: ViewModifier {
    @Binding var discordButton1Text: String
    @Binding var discordButton1Visible: Bool
    @Binding var discordButton2Text: String
    @Binding var discordButton2Visible: Bool
    @Binding var discordInfoDismissed: Bool
    @Binding var scrobbleMin: Double
    @Binding var scrobblePercent: Double
    @Binding var scrobbleMax: Double

    func body(content: Content) -> some View {
        content
            .onChange(of: discordButton1Text) { _, value in AppSettings.shared.setDiscordButton1Text(value: value) }
            .onChange(of: discordButton1Visible) { _, value in AppSettings.shared.setDiscordButton1Visible(value: value) }
            .onChange(of: discordButton2Text) { _, value in AppSettings.shared.setDiscordButton2Text(value: value) }
            .onChange(of: discordButton2Visible) { _, value in AppSettings.shared.setDiscordButton2Visible(value: value) }
            .onChange(of: discordInfoDismissed) { _, value in AppSettings.shared.setDiscordInfoDismissed(value: value) }
            .onChange(of: scrobbleMin) { _, value in AppSettings.shared.setScrobbleMinDuration(value: Swift.Int32(value)) }
            .onChange(of: scrobblePercent) { _, value in AppSettings.shared.setScrobbleDelayPercent(value: Float(value)) }
            .onChange(of: scrobbleMax) { _, value in AppSettings.shared.setScrobbleDelaySeconds(value: Swift.Int32(value)) }
    }
}
