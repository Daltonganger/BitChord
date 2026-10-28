import SwiftUI

struct UpdatesSettingsPage: View {
    @State private var checker = UpdateChecker.shared
    @State private var showingRelease = false
    var body: some View {
        Form {
            Section("Installed") {
                LabeledContent("Version and build", value: checker.currentVersion)
                LabeledContent("Build type", value: checker.buildChannel)
                if let revision = checker.revision { LabeledContent("Revision", value: revision).textSelection(.enabled) }
            }
            Section("Latest Published Release") {
                if let release = checker.latest {
                    LabeledContent("Version and build", value: release.identity.label)
                    if let date = release.publishedAt { LabeledContent("Published on", value: date.formatted(date: .abbreviated, time: .omitted)) }
                    Button("What’s New") { showingRelease = true }
                }
                Text(checker.status).font(.callout).foregroundStyle(.secondary)
                if let date = checker.lastCheckedAt { Text("Last successful check: \(date.formatted())").font(.caption).foregroundStyle(.secondary) }
                Button {
                    Task { await checker.check() }
                } label: {
                    if checker.checking { ProgressView("Checking…") } else { Text("Check for Updates") }
                }.disabled(checker.checking)
            }
            if let release = checker.latest {
                Section("Installation") {
                    ReleaseInstallationView(release: release)
                }
            } else {
                Section("Installation") {
                    Text("The latest release page is the single place to find the published iOS IPA and Mac DMG.")
                    Link("Open Latest Release", destination: UpdateChecker.latestReleaseURL)
                }
            }
            #if os(macOS)
            Section("Automatic Mac Updates") { MacUpdateSettings() }
            #endif
        }
        .formStyle(.grouped)
        .navigationTitle("Version & Updates")
        .task { await checker.pollOnce() }
        .sheet(isPresented: $showingRelease) {
            if let release = checker.latest { UpdateSheet(release: release) }
        }
    }
}
