#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
python3 - "$root" <<'PY'
import sys
from pathlib import Path

root = Path(sys.argv[1])
load = (root / "AppleApp/Sources/PlaybackSession/PlaybackController+LoadResults.swift").read_text()
log = (root / "AppleApp/Sources/PlaybackSession/PlaybackDebugLog.swift").read_text()
sheet = (root / "AppleApp/Sources/UI/AudioPipelineSheet.swift").read_text()
fields = [
    "source=", "route_kind=", "claimed_quality=", "claimed_max_kbps=", "measured_codec=", "measured_rate=",
    "measured_depth=", "measured_kbps=", "output_rate=", "output_format=",
    "loudness_origin=", "bit_perfect=",
]
checks = {
    "one quality record is emitted on every successful track start": "quality-start" in load,
    "source field reports the resolved playback origin": "switch resolvedOrigin[entry.id]" in load and all(origin in load for origin in ["case .local:", "case .cache:", "case .youtube:", "case .substitute:"]),
    "quality record includes claimed and measured stream/output fields": all(field in load for field in fields),
    "pipeline sheet shows claimed quality beside measured stream": 'row("Claimed Quality", claimedQuality)' in sheet and 'row("Measured Stream", measuredStream)' in sheet,
    "loudness origin maps the engine's actual metadata": 'loudnessOrigin = "player_response"' in load and 'loudnessOrigin = "analyzed"' in load and 'loudnessOrigin = "none"' in load,
    "diagnostic persistence sanitizes and bounds each event": "Self.sanitize(" in log and "capacity" in log and "limit = 2 * 1024 * 1024" in log,
}
for label, passed in checks.items():
    print(f"{'PASS' if passed else 'FAIL'} {label}")
if not all(checks.values()):
    raise SystemExit(1)
PY
