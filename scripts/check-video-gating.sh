#!/bin/bash
# The O9 picture gate, with fake surfaces and scene values.
#
# A device check can only say "the picture did/didn't show" once. The two rules
# that broke — a surface leaving is not the scene backgrounding, and a track
# without a videoId must clear the previous one — are arithmetic on state, so
# this compiles the real gate and pins them.
#
# Run: scripts/check-video-gating.sh
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"

bin="$(mktemp -d)/video-gating-verify"
xcrun swiftc -O -module-cache-path /tmp/bitchord-swift-module-cache -o "$bin" \
  "$repo/AppleApp/Sources/UI/VideoSurfaceState.swift" \
  "$here/video-gating-verify/main.swift" || exit 2

exec "$bin"
