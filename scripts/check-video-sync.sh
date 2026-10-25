#!/bin/bash
# The picture-follows-the-engine contract, with fake clock values.
#
# A device check can only say "looks in sync" on one machine at one moment.
# The decisions themselves — seek past 150 ms, nudge at most ±0.1, pause with
# the transport, one seek per cooldown — are arithmetic, so this compiles the
# real policy and pins them at their boundaries.
#
# Run: scripts/check-video-sync.sh
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"

bin="$(mktemp -d)/video-sync-verify"
xcrun swiftc -O -module-cache-path /tmp/bitchord-swift-module-cache -o "$bin" \
  "$repo/AppleApp/Sources/PlaybackSession/VideoSyncPolicy.swift" \
  "$here/video-sync-verify/main.swift" || exit 2

exec "$bin"
