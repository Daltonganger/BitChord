#!/bin/bash
# The session-expiry matcher, through the app's real error types.
#
# Detection reads the bridge's message off `localizedDescription`, which only
# carries it for a `LocalizedError`. The search error was a plain `Error`, so
# that site could never match and a refused session would have looked like an
# ordinary load failure. This compiles the real `InnertubeSearch.swift` and the
# real shared matcher, and asserts both the match and the reason it was missing.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"

fw="$repo/AppleApp/Frameworks/BitChordShared.xcframework/macos-arm64"
bin="$(mktemp -d)/auth-verify"
xcrun swiftc -O -o "$bin" \
  -F "$fw" -framework BitChordShared \
  -Xlinker -rpath -Xlinker "$fw/BitChordShared.framework/Versions/A" \
  "$repo/AppleApp/Sources/PlaybackSession/QueueEntry.swift" \
  "$repo/AppleApp/Sources/App/InnertubeSearch.swift" \
  "$here/auth-verify/main.swift" || exit 2

exec "$bin"
