#!/bin/bash
# Every lyrics source, against the real thing.
#
# Sixteen providers that had never been run against a live host. This asks each
# one on its own, for a handful of tracks across four catalogues, and reports
# what came back — so "no lyrics" can be told apart from "this source needs a
# key" and from "this source is broken".
#
# It reaches the network on purpose, and it takes a while: sixteen sources, five
# tracks each, and a source that is down costs its timeout.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"

lyrics_pane="$repo/AppleApp/Sources/UI/NowPlaying/LyricsPane.swift"
if ! grep -Fq 'private let maxSharedCharacters = 250' "$lyrics_pane" ||
   ! grep -Fq 'selectedCharacterCount + shareText(at: index).count <= maxSharedCharacters' "$lyrics_pane"; then
  echo "FAIL: lyric sharing must enforce its 250-character quote budget" >&2
  exit 1
fi
echo "  ok  lyric share selection uses a 250-character budget"

fw="$repo/AppleApp/Frameworks/BitChordShared.xcframework/macos-arm64"
bin="$(mktemp -d)/lyrics-verify"
xcrun swiftc -O -o "$bin" \
  -F "$fw" -framework BitChordShared \
  -Xlinker -rpath -Xlinker "$fw/BitChordShared.framework/Versions/A" \
  "$here/lyrics-verify/main.swift"

exec "$bin"
