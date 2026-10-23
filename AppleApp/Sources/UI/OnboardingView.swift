import SwiftUI
import BitChordShared

/// First run (upstream v1.8 onboarding, plus a page upstream does not have):
/// what makes the app worth choosing, a name for Listen Together, and the
/// account connection. Shown once; every step can be skipped and every choice
/// changed later in Settings.
struct OnboardingView: View {
    @Environment(AuthController.self) private var auth
    @Environment(PartyStore.self) private var party
    @Environment(\.dismiss) private var dismiss
    @AppStorage(OnboardingView.doneKey) private var done = false
    @State private var step = 0
    @State private var name = ""
    @FocusState private var nameFocused: Bool

    static let doneKey = "onboarding_done"

    var body: some View {
        VStack(spacing: 0) {
            Group {
                switch step {
                case 0: welcome
                case 1: namePage
                default: connect
                }
            }
            .frame(maxWidth: 520)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(.horizontal, 28)
            .transition(.asymmetric(insertion: .move(edge: .trailing).combined(with: .opacity),
                                    removal: .move(edge: .leading).combined(with: .opacity)))
            pageDots
                .padding(.bottom, 20)
        }
        .background(backdrop)
        .animation(.smooth(duration: 0.35), value: step)
        .onAppear { name = party.nickname }
        #if os(macOS)
        .frame(width: 560, height: 620)
        #endif
        .interactiveDismissDisabled()
    }

    // MARK: Pages

    private var welcome: some View {
        VStack(spacing: 22) {
            Spacer(minLength: 20)
            Image(.bchLogo)
                .resizable()
                .scaledToFit()
                .frame(width: 92, height: 62)
                .foregroundStyle(.white)
                .padding(26)
                .background(
                    LinearGradient(colors: [Color(red: 0.98, green: 0.36, blue: 0.45), Color(red: 0.98, green: 0.14, blue: 0.23)],
                                   startPoint: .top, endPoint: .bottom),
                    in: .rect(cornerRadius: 34, style: .continuous)
                )
                .shadow(color: .red.opacity(0.35), radius: 24, y: 10)
                .accessibilityHidden(true)
            Text("Millions of songs, played properly")
                .font(.largeTitle.weight(.bold))
                .multilineTextAlignment(.center)
            VStack(alignment: .leading, spacing: 16) {
                feature("dot.radiowaves.left.and.right", "Dolby Atmos and bit-perfect output",
                        "Apple's spatial renderer, and untouched lossless to your DAC on a Mac.")
                feature("shuffle", "Automix that DJs",
                        "Echo outs, loop rolls, cuts on the one — not just a crossfade.")
                feature("quote.bubble", "Lyrics in time, word by word",
                        "From a dozen catalogues, aligned to the audio you are hearing.")
                feature("person.2", "Listen Together",
                        "Start a party and play the same song, in sync, with friends.")
            }
            .padding(.top, 6)
            Spacer(minLength: 12)
            primaryButton("Get Started") { step = 1 }
        }
    }

    private var namePage: some View {
        VStack(spacing: 18) {
            Spacer(minLength: 20)
            Image(systemName: "person.crop.circle")
                .font(.system(size: 56, weight: .light))
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            Text("What should we call you?")
                .font(.title.weight(.bold))
                .multilineTextAlignment(.center)
            TextField("Your name", text: $name)
                .textFieldStyle(.plain)
                .font(.title3)
                .multilineTextAlignment(.center)
                .padding(.vertical, 14)
                .padding(.horizontal, 18)
                .background(.quaternary.opacity(0.6), in: .rect(cornerRadius: 14, style: .continuous))
                .focused($nameFocused)
                .submitLabel(.next)
                .onSubmit(saveNameAndContinue)
            Text("Friends see this when you Listen Together. You can change it later in Settings.")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Spacer(minLength: 12)
            primaryButton("Next", action: saveNameAndContinue)
            backButton
        }
        .onAppear { nameFocused = true }
    }

    private var connect: some View {
        VStack(spacing: 18) {
            Spacer(minLength: 20)
            Image(systemName: "music.note.house")
                .font(.system(size: 56, weight: .light))
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            Text("Connect your YouTube Music account")
                .font(.title.weight(.bold))
                .multilineTextAlignment(.center)
            Text("Bring your library, playlists and mixes with you, and start a Listen Together party with friends.")
                .font(.body)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Spacer(minLength: 12)
            primaryButton(auth.signedIn ? "Done" : "Sign In with Google") {
                if auth.signedIn {
                    finish()
                } else {
                    finish()
                    auth.loginPresented = true
                }
            }
            Button("Not Now") { finish() }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .padding(.vertical, 6)
            backButton
        }
    }

    // MARK: Parts

    private func feature(_ symbol: String, _ title: String, _ detail: String) -> some View {
        HStack(alignment: .top, spacing: 14) {
            Image(systemName: symbol)
                .font(.title3.weight(.semibold))
                .foregroundStyle(.tint)
                .frame(width: 30)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.headline)
                Text(detail).font(.subheadline).foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .combine)
    }

    private func primaryButton(_ title: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title)
                .font(.headline)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 6)
        }
        .buttonStyle(.borderedProminent)
        .controlSize(.large)
        .buttonBorderShape(.capsule)
    }

    @ViewBuilder
    private var backButton: some View {
        Button("Back") { step = max(0, step - 1) }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .font(.callout)
            .padding(.bottom, 4)
    }

    private var pageDots: some View {
        HStack(spacing: 8) {
            ForEach(0..<3, id: \.self) { index in
                Capsule()
                    .fill(index == step ? Color.primary : Color.secondary.opacity(0.35))
                    .frame(width: index == step ? 18 : 7, height: 7)
            }
        }
        .accessibilityElement()
        .accessibilityLabel("Step \(step + 1) of 3")
    }

    private var backdrop: some View {
        ZStack {
            Color.clear.background(.background)
            RadialGradient(colors: [Color.red.opacity(0.18), .clear], center: .top, startRadius: 10, endRadius: 520)
        }
        .ignoresSafeArea()
    }

    private func saveNameAndContinue() {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty { party.setNickname(trimmed) }
        step = 2
    }

    private func finish() {
        done = true
        dismiss()
    }
}
