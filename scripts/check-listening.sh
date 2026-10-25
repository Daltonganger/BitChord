#!/usr/bin/env bash
#
# Check Replay's per-day aggregation and the week that spans two months.
#
# # Why this exists
#
# `ListeningStore` is where Replay's numbers are computed, and the numbers are
# wrong in ways a build cannot see: a per-day row seeded from a month's running
# total reports minutes that never happened, and a week whose Monday falls in the
# previous month reads a file the month buckets do not open. Both are off-by-a-
# bucket mistakes rather than crashes, so they survive every build and every
# screenshot.
#
# This compiles the shipped store with a checker that records samples on chosen
# days into a temporary folder and asks for the summaries. It never touches the
# real listening library.

set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
APP="$ROOT/AppleApp"
FW="$APP/Frameworks/BitChordShared.xcframework/macos-arm64"
CHECK_DIR="${TMPDIR:-/tmp}/bitchord-listening-check"

if [ ! -d "$FW" ]; then
  echo "BitChordShared.xcframework is missing — run ./scripts/build-shared-framework.sh first." >&2
  exit 1
fi

rm -rf "$CHECK_DIR"
mkdir -p "$CHECK_DIR"

xcrun swiftc \
  -D DEBUG \
  -sdk "$(xcrun --show-sdk-path --sdk macosx)" \
  -target arm64-apple-macos15.0 \
  -swift-version 5 \
  -module-cache-path /tmp/bitchord-swift-module-cache \
  -F "$FW" \
  -framework BitChordShared \
  -Xlinker -rpath -Xlinker "$FW/BitChordShared.framework/Versions/A" \
  -o "$CHECK_DIR/check" \
  "$APP/Sources/PlaybackSession/ListeningStore.swift" \
  "$ROOT/scripts/listening-verify/main.swift"

"$CHECK_DIR/check"
