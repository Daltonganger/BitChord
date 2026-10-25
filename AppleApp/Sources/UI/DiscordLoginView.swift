import SwiftUI
import WebKit
import BitChordShared

/// Captures a Discord user token the same unofficial way upstream's Kizzy login does.
struct DiscordLoginView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var status = "Sign in to Discord. BitChord reads the token Discord stores locally — never your password."
    @State private var username: String?

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                Text(status)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(8)
                DiscordWebView { token in
                    DiscordBridge.shared.validateToken(token: token, callback: TokenAdapter { ok, name in
                        Task { @MainActor in
                            if ok {
                                username = name
                                status = "Signed in as \(name ?? "Discord")"
                                DiscordGateway.shared.connect(token: token)
                            } else {
                                status = "Token rejected."
                            }
                        }
                    })
                }
            }
            .navigationTitle("Discord")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
                if username != nil {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Done") { dismiss() }
                    }
                }
            }
        }
        #if os(macOS)
        .frame(minWidth: 720, minHeight: 640)
        #endif
    }
}

#if os(macOS)
private struct DiscordWebView: NSViewRepresentable {
    var onToken: (String) -> Void
    func makeNSView(context: Context) -> WKWebView { context.coordinator.attach(WKWebView()) }
    func updateNSView(_ view: WKWebView, context: Context) {}
    func makeCoordinator() -> DiscordTokenHarvester { DiscordTokenHarvester(onToken: onToken) }
}
#else
private struct DiscordWebView: UIViewRepresentable {
    var onToken: (String) -> Void
    func makeUIView(context: Context) -> WKWebView { context.coordinator.attach(WKWebView()) }
    func updateUIView(_ view: WKWebView, context: Context) {}
    func makeCoordinator() -> DiscordTokenHarvester { DiscordTokenHarvester(onToken: onToken) }
}
#endif

/// Lifts the session token out of discord.com once the page has signed in —
/// upstream's `DiscordLoginScreen`, on WebKit.
///
/// Two things kept the old version on the login page forever. Discord routes from
/// the login form to the app with `pushState`, so no navigation finishes after
/// sign-in and a `didFinish` hook only ever ran on the form itself. And once the
/// client boots it takes `localStorage` away from the top-level window, so reading
/// it there finds nothing even at the right moment. Watching `url` catches the
/// `pushState`, and a fresh same-origin iframe still has an untouched handle on
/// the store where the token sits.
private final class DiscordTokenHarvester: NSObject, WKNavigationDelegate {
    /// Returns the token, or `''` while the client is still booting so the caller
    /// retries rather than waiting on a snippet that threw.
    private static let tokenSnippet = """
        (function () {
          try {
            var frame = document.createElement('iframe');
            document.body.appendChild(frame);
            var token = frame.contentWindow.localStorage.token;
            document.body.removeChild(frame);
            return token ? JSON.parse(token) : '';
          } catch (e) {
            return '';
          }
        })()
        """
    /// The token lands a moment after the client starts for `/app`, so the first
    /// read is usually empty. Twelve tries half a second apart covers a slow boot.
    private static let maxAttempts = 12

    private let onToken: (String) -> Void
    private weak var webView: WKWebView?
    private var urlObservation: NSKeyValueObservation?
    private var retry: Timer?
    private var attempts = 0
    private var captured = false

    init(onToken: @escaping (String) -> Void) { self.onToken = onToken }

    deinit {
        retry?.invalidate()
        urlObservation?.invalidate()
    }

    func attach(_ view: WKWebView) -> WKWebView {
        webView = view
        view.navigationDelegate = self
        urlObservation = view.observe(\.url, options: [.new]) { [weak self] view, _ in
            let url = view.url
            DispatchQueue.main.async { self?.pageMoved(to: url) }
        }
        view.load(URLRequest(url: URL(string: "https://discord.com/login")!))
        return view
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        pageMoved(to: webView.url)
    }

    /// Past the sign-in flow, wherever it landed: `/app` or `/channels/@me`
    /// depending on how the session was established.
    private static func isSignedIn(_ url: URL?) -> Bool {
        guard let url, url.host == "discord.com" else { return false }
        return !url.path.hasPrefix("/login") && !url.path.hasPrefix("/register")
    }

    private func pageMoved(to url: URL?) {
        guard !captured, Self.isSignedIn(url) else { return }
        // A fresh run of attempts per arrival: a 2FA step can bounce through more
        // than one signed-in-looking address before the store is written.
        attempts = 0
        harvest()
    }

    private func harvest() {
        retry?.invalidate()
        retry = nil
        guard !captured, attempts < Self.maxAttempts, let webView else { return }
        attempts += 1
        webView.evaluateJavaScript(Self.tokenSnippet) { [weak self] result, _ in
            guard let self, !self.captured else { return }
            if let token = result as? String, token.count > 20 {
                self.captured = true
                self.onToken(token)
            } else {
                self.retry = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: false) { [weak self] _ in
                    self?.harvest()
                }
            }
        }
    }
}

private final class TokenAdapter: DiscordBridgeTokenCallback {
    let handler: (Bool, String?) -> Void
    init(_ handler: @escaping (Bool, String?) -> Void) { self.handler = handler }
    func onResult(ok: Bool, username: String?) { handler(ok, username) }
}
