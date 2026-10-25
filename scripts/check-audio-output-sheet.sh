#!/usr/bin/env bash
# Pin the output sheet's parity structure and platform route sources.
set -euo pipefail
cd "$(dirname "$0")/.."
python3 - <<'PY'
from pathlib import Path
import json

source = Path("AppleApp/Sources/UI/AudioPipelineSheet.swift").read_text()
start = source.index("struct OutputDeviceSheet: View")
end = source.index("private struct OutputRouteRowModel", start)
sheet = source[start:end]

headings = [
    'Text("Audio Output")',
    'Text("Volume")',
    'Text("Audio Pipeline")',
    'Text("Playback Diagnostics")',
]
positions = [sheet.index(heading) for heading in headings]
assert positions == sorted(positions), "output sheet sections must remain output → volume → pipeline → diagnostics"

required = {
    "iOS route list": "AVAudioSession.sharedInstance().currentRoute.outputs",
    "macOS CoreAudio route": "MacAudioRoutes.current()",
    "AirPlay row": "AirPlayOutputRow()",
    "app volume binding": "get: { controller.volume }",
    "pipeline navigation": "AudioPipelineSheet()",
    "live negotiated format": "Text(pipelineSummary)",
    "readable USB label": 'case .usbAudio: String(localized: "USB Audio")',
    "readable CarPlay label": 'case .carAudio: String(localized: "CarPlay")',
}
for name, needle in required.items():
    assert needle in sheet, f"missing {name}: {needle}"

airplay_start = source.index("private struct AirPlayOutputRow")
airplay_end = source.index("#if os(iOS)", airplay_start)
airplay_row = source[airplay_start:airplay_end]
assert "SystemRoutePickerView()" in airplay_row, "AirPlay row must open Apple's route picker"
catalog = json.loads(Path("AppleApp/Resources/Localization/Localizable.xcstrings").read_text())
for label in ["Speaker", "Headphones", "Bluetooth", "AirPlay", "USB Audio", "HDMI", "CarPlay", "Receiver", "Output"]:
    assert label in catalog["strings"], f"missing localized output label: {label}"
print("audio output sheet structure and route sources: PASS")
PY
