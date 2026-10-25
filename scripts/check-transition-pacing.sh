#!/bin/bash
# A′3's session pacing, with fake styles.
#
# A device check can only hear whether a set sounded mixed. The two rules that
# broke are arithmetic on a pair-keyed value — a re-plan of one transition must
# replace its entry, and must not see its own effect as budget already spent —
# so this compiles the real value and pins them.
#
# Run: scripts/check-transition-pacing.sh
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"

bin="$(mktemp -d)/transition-pacing-verify"
xcrun swiftc -O -module-cache-path /tmp/bitchord-swift-module-cache -o "$bin" \
  "$repo/AppleApp/Sources/PlaybackSession/TransitionPacing.swift" \
  "$here/transition-pacing-verify/main.swift" || exit 2

exec "$bin"
