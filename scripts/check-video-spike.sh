#!/bin/bash
# The O9 video side, against YouTube.
#
# A fixture can only prove that video fields parse; it cannot prove that the
# direct profile the audio resolves through answers with an AVC/MP4 rendition,
# that its URL is ready rather than throttled by YouTube's n parameter, or that
# the CDN serves it. The audio path never asks for video: the video side is its
# own request, and this check is what pins its contract before the player
# surfaces use it.
#
# Run: scripts/check-video-spike.sh
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"

fw="$repo/AppleApp/Frameworks/BitChordShared.xcframework/macos-arm64"
verification_dir="$(mktemp -d)"
bin="$verification_dir/video-spike-verify"
xcrun swiftc -O -module-cache-path /tmp/bitchord-swift-module-cache -o "$bin" \
  -F "$fw" -framework BitChordShared -framework WebKit \
  -Xlinker -rpath -Xlinker "$fw/BitChordShared.framework/Versions/A" \
  "$repo/AppleApp/Sources/App/YouTubeChallengeSolver.swift" \
  "$repo/AppleApp/Sources/App/YouTubePlayerJs.swift" \
  "$repo/AppleApp/Sources/App/ApplePoTokenProvider.swift" \
  "$repo/AppleApp/Sources/App/InnertubeStreamResolver.swift" \
  "$repo/AppleApp/Sources/App/ResolvedVideoSide.swift" \
  "$here/video-spike-verify/main.swift" || exit 2

exec "$bin" "$repo/AppleApp/Resources/YouTubeSolver"
