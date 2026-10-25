#!/usr/bin/env bash
#
# The Handoff payload — what one device hands another.
#
# A build proves the encoder compiles; it does not prove the other device can read
# what it wrote. `userInfo` accepts property-list types only, Ints come back as
# NSNumbers on the far side, and a payload that arrives unreadable is a handoff
# that silently does nothing. So this runs the app's own payload type and feeds it
# both good and bad activities.
#
# Also cross-checks the activity type against the plist declaration it needs:
# `NSUserActivityTypes` in `AppleApp/project.yml` is what lets the system launch
# this app for an activity that arrives from the other device, and it is easy to
# add the activity in code and forget the declaration.
#
# Needs no window server and no devices.

set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
HERE="$(cd "$(dirname "$0")" && pwd)"
CHECK_DIR="${TMPDIR:-/tmp}/bitchord-handoff-check"

rm -rf "$CHECK_DIR"
mkdir -p "$CHECK_DIR"

xcrun swiftc \
  -sdk "$(xcrun --show-sdk-path --sdk macosx)" \
  -target arm64-apple-macos15.0 \
  -swift-version 5 \
  -o "$CHECK_DIR/check" \
  "$ROOT/AppleApp/Sources/App/HandoffActivity.swift" \
  "$HERE/handoff-verify/main.swift"

status=0
"$CHECK_DIR/check" || status=1

if grep -q -- "- app.bitchord.playback" "$ROOT/AppleApp/project.yml"; then
  echo "  ok   the activity type is declared in NSUserActivityTypes"
else
  echo "  FAIL the activity type is not declared in AppleApp/project.yml"
  status=1
fi

exit "$status"
