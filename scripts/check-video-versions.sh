#!/bin/bash
# Live verification for counterpart-first music-video selection and alignment.
# Run only when an operator explicitly requests it: it makes Innertube and
# media-stream requests for the fixed 20-song sample.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
fw="$repo/AppleApp/Frameworks/BitChordShared.xcframework/macos-arm64"
native_fw="$repo/AppleApp/Frameworks/NativeCoreFFI.xcframework/macos-arm64"
verification_dir="$(mktemp -d)"
bin="$verification_dir/video-versions-verify"
mkdir -p "$verification_dir/home"
trap 'rm -rf "$verification_dir"' EXIT

if [ ! -d "$native_fw" ]; then
  echo "no NativeCoreFFI for macos-arm64 — run scripts/build-native-core.sh first" >&2
  exit 2
fi

xcrun swiftc -O -module-cache-path /tmp/bitchord-swift-module-cache -o "$bin" \
  -F "$native_fw" -framework NativeCoreFFI \
  -framework CoreAudio -framework AudioToolbox -framework CoreFoundation \
  -Xlinker -rpath -Xlinker "$native_fw/NativeCoreFFI.framework/Versions/A" \
  -F "$fw" -framework BitChordShared -framework WebKit -framework JavaScriptCore \
  -Xlinker -rpath -Xlinker "$fw/BitChordShared.framework/Versions/A" \
  "$repo/AppleApp/Sources/App/YouTubeChallengeSolver.swift" \
  "$repo/AppleApp/Sources/App/YouTubePlayerJs.swift" \
  "$repo/AppleApp/Sources/App/ApplePoTokenProvider.swift" \
  "$repo/AppleApp/Sources/App/InnertubeStreamResolver.swift" \
  "$repo/AppleApp/Sources/App/ResolvedVideoSide.swift" \
  "$repo/AppleApp/Generated/NativeCore.swift" \
  "$repo/AppleApp/Sources/PlaybackSession/PlaybackDebugLog.swift" \
  "$repo/AppleApp/Sources/PlaybackSession/VideoVersionAlignment.swift" \
  "$here/video-versions-verify/main.swift" || exit 2

HOME="$verification_dir/home" CFFIXED_USER_HOME="$verification_dir/home" \
  "$bin" "$repo/AppleApp/Resources/YouTubeSolver"
status=$?
exit "$status"
