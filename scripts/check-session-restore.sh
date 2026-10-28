#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
tmp="$(mktemp -d "${TMPDIR:-/tmp}/bitchord-session.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
xcrun swiftc -parse-as-library -swift-version 5 -target arm64-apple-macos15.0 \
  AppleApp/Sources/PlaybackSession/QueueEntry.swift \
  AppleApp/Sources/PlaybackSession/LastPlayed.swift \
  scripts/session-restore-verify/check.swift -o "$tmp/check"
"$tmp/check"
