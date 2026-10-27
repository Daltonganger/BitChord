#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
python3 - "$root" <<'PY'
import sys
from pathlib import Path

root = Path(sys.argv[1])
route = (root / "AppleApp/Sources/PlaybackSession/LosslessRoute.swift").read_text()
state = (root / "AppleApp/Sources/PlaybackSession/AudioRouteState.swift").read_text()
upgrade = (root / "AppleApp/Sources/PlaybackSession/PlaybackController+QualityUpgrade.swift").read_text()
source = (root / "AppleApp/Sources/PlaybackSession/PlaybackController+SourceResolution.swift").read_text()
session = (root / "AppleApp/Sources/PlaybackSession/AudioSessionManager.swift").read_text()
kotlin = (root / "shared/src/commonMain/kotlin/com/music/bitchord/data/sources/SourceResolver.kt").read_text()
preferences = (root / "AppleApp/Sources/PlaybackSession/PlaybackController+Preferences.swift").read_text()
checks = {
    "unknown Bluetooth codec is conservatively capped": "guard let codec = bluetoothCodec?.lowercased() else { return false }" in route,
    "source quality can remain lossless independently of the physical Bluetooth output": "PlaybackCodecCapabilities.allowLosslessSources" in kotlin and "StreamRequest.Best" in kotlin and "val allowLosslessSources" in (root / "shared/src/commonMain/kotlin/com/music/bitchord/data/sources/PlaybackCodecCapabilities.kt").read_text(),
    "direct wired routes can carry lossless": "guard route.contains(\"bluetooth\") else { return true }" in route,
    "bit-perfect requires lossless integer PCM and a direct route": "guard sourceIsLossless, integerPcm else { return false }" in route and "!route.contains(\"airplay\")" in route,
    "the route policy drives codec capability": "LosslessRoute.permitsLossless(routeKind: kind)" in state,
    "AirPlay output rate is clamped to 44.1–48 kHz": "cappedLosslessSampleRate" in session and "44_100" in route and "48_000" in route,
    "speaker normalization compares the AVAudioSession raw port value": "AVAudioSession.Port.builtInSpeaker.rawValue" in preferences,
    "bit-perfect remains opt-in and engine startup applies eligibility gating": 'bit_perfect_output", default: false' in (root / "AppleApp/Sources/PlaybackSession/PlaybackController.swift").read_text() and "let startBitPerfect = effectiveBitPerfectOutput" in (root / "AppleApp/Sources/PlaybackSession/PlaybackController.swift").read_text() and "setBitPerfectEnabled(enabled: startBitPerfect)" in (root / "AppleApp/Sources/PlaybackSession/PlaybackController.swift").read_text() and "guard bitPerfectOutput" in preferences,
    "Settings UI keeps automatic bit-perfect opt-in": 'bit_perfect_output", default: false' in (root / "AppleApp/Sources/UI/Settings/SettingsView.swift").read_text(),
    "route changes follow source preference and re-offer after returning to wired": "qualityUpgradeTask?.cancel()" in (root / "AppleApp/Sources/PlaybackSession/PlaybackController.swift").read_text() and "returnedFromBluetooth" in (root / "AppleApp/Sources/PlaybackSession/PlaybackController.swift").read_text(),
}
for label, passed in checks.items():
    print(f"{'PASS' if passed else 'FAIL'} {label}")
if not all(checks.values()):
    raise SystemExit(1)
PY

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
cat > "$scratch/RoutePolicySelfCheck.swift" <<'SWIFT'
import Foundation

@main
struct RoutePolicySelfCheck {
    static func main() {
        // AVAudioSession.Port.builtInSpeaker.rawValue is "Speaker"; .airPlay.rawValue is "AirPlay".
        precondition(LosslessRoute.permitsLossless(routeKind: "Speaker"))
        precondition(LosslessRoute.permitsLossless(routeKind: "USB"))
        precondition(LosslessRoute.permitsLossless(routeKind: "HDMI"))
        precondition(LosslessRoute.permitsLossless(routeKind: "AirPlay"))
        precondition(LosslessRoute.permitsLossless(routeKind: ""))
        precondition(!LosslessRoute.permitsLossless(routeKind: "BluetoothA2DP"))
        precondition(!LosslessRoute.permitsLossless(routeKind: "BluetoothA2DP", bluetoothCodec: "AAC"))
        precondition(!LosslessRoute.permitsLossless(routeKind: "BluetoothA2DP", bluetoothCodec: "SBC"))
        precondition(!LosslessRoute.permitsLossless(routeKind: "BluetoothA2DP", bluetoothCodec: "LDAC"))
        precondition(LosslessRoute.permitsLossless(routeKind: "BluetoothA2DP", bluetoothCodec: "aptX Lossless"))
        precondition(LosslessRoute.permitsBitPerfect(routeKind: "USB", sourceIsLossless: true, integerPcm: true))
        precondition(!LosslessRoute.permitsBitPerfect(routeKind: "AirPlay", sourceIsLossless: true, integerPcm: true))
        precondition(!LosslessRoute.permitsBitPerfect(routeKind: "BluetoothA2DP", sourceIsLossless: true, integerPcm: true))
        precondition(LosslessRoute.cappedLosslessSampleRate(routeKind: "AirPlay", requested: 96_000) == 48_000)
        precondition(LosslessRoute.cappedLosslessSampleRate(routeKind: "AirPlay", requested: 44_100) == 44_100)
        precondition(LosslessRoute.cappedLosslessSampleRate(routeKind: "USB", requested: 96_000) == 96_000)
        print("PASS executable route-policy self-check")
    }
}
SWIFT
swiftc "$root/AppleApp/Sources/PlaybackSession/LosslessRoute.swift" "$scratch/RoutePolicySelfCheck.swift" -o "$scratch/route-policy-self-check"
"$scratch/route-policy-self-check"
