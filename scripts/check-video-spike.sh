#!/bin/bash
# The O9 music-video spike, against YouTube.
#
# A fixture can only prove that video fields parse; it cannot prove that the
# extraction stack the app uses is ever handed a video-only format, or that the
# URL it gets is served rather than throttled by YouTube's n parameter. The
# playback path asks with wantVideo = false and must keep doing so until these
# numbers exist, so the probe is the only thing that looks.
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
  "$here/video-spike-verify/main.swift" || exit 2

exec "$bin" "$repo/AppleApp/Resources/YouTubeSolver"
