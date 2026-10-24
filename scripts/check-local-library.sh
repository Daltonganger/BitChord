#!/bin/bash
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
bin="$(mktemp -d)/local-library-verify"
xcrun swiftc -module-cache-path /tmp/bitchord-swift-module-cache -o "$bin" \
  "$repo/AppleApp/Sources/PlaybackSession/LocalTrackOrder.swift" \
  "$repo/scripts/local-library-verify/main.swift"
"$bin"
