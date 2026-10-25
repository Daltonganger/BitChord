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
share_card="$repo/AppleApp/Sources/UI/NowPlaying/LyricShareCard.swift"
if ! grep -Fq 'private let maxSharedCharacters = 250' "$lyrics_pane" ||
   ! grep -Fq 'selection.isEmpty || nextTotal <= maxSharedCharacters' "$lyrics_pane" ||
   ! grep -Fq 'lyricTextIsRightToLeft(line.text)' "$lyrics_pane" ||
   ! grep -Fq 'PHAssetCreationRequest.forAsset()' "$lyrics_pane" ||
   ! grep -Fq 'storyFormat: storyCardFormat' "$lyrics_pane" ||
   ! grep -Fq 'renderer.scale = 2' "$lyrics_pane" ||
   ! grep -Fq '.frame(width: 540, height: cardHeight' "$share_card" ||
   ! grep -Fq 'storyFormat ? 960 : nil' "$share_card" ||
   ! grep -Fq 'NSPhotoLibraryAddUsageDescription:' "$repo/AppleApp/project.yml"; then
  echo "FAIL: lyric sharing must cover its budget exception, 1080px/story formats, and Photos export" >&2
  exit 1
fi
echo "  ok  lyric sharing enforces its character budget and export formats"

fw="$repo/AppleApp/Frameworks/BitChordShared.xcframework/macos-arm64"
bin="$(mktemp -d)/lyrics-verify"
xcrun swiftc -O -o "$bin" \
  -F "$fw" -framework BitChordShared \
  -Xlinker -rpath -Xlinker "$fw/BitChordShared.framework/Versions/A" \
  "$here/lyrics-verify/main.swift"

exec "$bin"
