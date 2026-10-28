import Foundation
@main struct Check {
 static func main() {
  precondition(AudioSettingsItem.allCases.allSatisfy { !$0.title.isEmpty && !$0.keywords.isEmpty })
  precondition(Set(AudioSettingsItem.allCases.map(\.id)).count == AudioSettingsItem.allCases.count)
  var output = AudioSettingsReadout()
  precondition(!output.bitPerfectStatus(requested: true).contains("Active ·"))
  output.loaded = true; output.outputStarted = true; output.sourceRate = 44100; output.outputRate = 48000
  precondition(output.sampleRateStatus(requested: true).contains("44100 → 48000"))
  precondition(output.sampleRateStatus(requested: true).contains("Requested"))
  precondition(output.processingBypass == nil)
  output.bitPerfectActive = true
  precondition(output.processingBypass != nil && output.bitPerfectStatus(requested: true).contains("Active ·"))
  output.appleRenderer = true
  precondition(output.processingBypass!.contains("Apple") && !output.sampleRateStatus(requested: true).contains("Currently matched"))
  precondition(AudioSettingsReadout.pcmLabel("UNKNOWN") != "PCM 16")
  precondition(ReleaseIdentity(version: "1.0.3", build: 13).compared(to: .init(version:"1.0.3",build:12)) == .newer)
  precondition(ReleaseIdentity(version: "1.0.3", build: 7).compared(to: .init(version:"1.0.3",build:12)) == .older)
  precondition(ReleaseIdentity(version: "1.0.3", build: nil).compared(to: .init(version:"1.0.3",build:12)) == .unknownBuild)
  precondition(ReleaseIdentity(version: "1.0.10", build: 1).compared(to: .init(version:"1.0.3",build:12)) == .newer)
  precondition(ReleaseIdentity(version: "nightly", build: nil).compared(to: .init(version:"1.0.3",build:12)) == nil)
  let good = ReleaseAsset(name: "BitChord-1.0.4-build13-ios-unsigned.ipa", browser_download_url: "https://github.com/bagumamartin/BitChord/releases/download/v1.0.4/app.ipa", size: 10)
  let bad = ReleaseAsset(name: good.name, browser_download_url: "https://example.invalid/app.ipa", size: 10)
  precondition(bad.url == nil)
  precondition(AppReleaseUpdate.legacyAsset(platform: "ios", assets: [good])?.name == good.name)
  precondition(AppReleaseUpdate.legacyAsset(platform: "ios", assets: [good,good]) == nil)
  precondition(AppReleaseUpdate.legacyAsset(platform: "macos", assets: [good]) == nil)
  precondition(AppReleaseUpdate.legacyAsset(platform: "ios", assets: [good], version: "1.0.5") == nil)
  let artifact = ReleaseManifest.Artifact(platform: "ios", name: good.name, sha256: String(repeating: "a", count: 64), minimumOS: "iOS 18", installation: "sign-and-install")
  let manifest = ReleaseManifest(schema: 1, version: "1.0.4", build: 13, revision: "abc1234", artifacts: [artifact])
  precondition(manifest.artifact(for: "ios", assets: [good]) != nil)
  precondition(manifest.artifact(for: "ios", assets: [bad]) == nil)
  precondition(ReleaseManifest(schema: 2, version: "1.0.4", build: 13, revision: "abc1234", artifacts: [artifact]).artifact(for: "ios", assets: [good]) == nil)
  precondition(ReleaseIdentity(version: "1.0.4-rc1", build: 13).compared(to: .init(version:"1.0.3",build:12)) == nil)
  precondition(ReleaseIdentity(version: "1.0.4", build: 13).key != ReleaseIdentity(version: "1.0.4", build: 14).key)
  let release = AppReleaseUpdate(identity: .init(version: "1.0.4", build: 13), releaseUrl: URL(string: "https://github.com/bagumamartin/BitChord/releases/tag/v1.0.4")!, notes: "![banner](https://example.invalid/image.png)\n\nPlayback fixes", publishedAt: nil, download: good, artifact: artifact, revision: "abc1234")
  precondition(release.displayNotes == "Playback fixes")
  print("PASS audio request/actual distinctions, bypass states, unknown formats and version/build comparison")
 }
}
