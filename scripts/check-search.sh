#!/bin/bash
# The search paths, against YouTube Music.
#
# A fixture can only prove the parser reads the body it was handed. It cannot
# prove that the anonymous typeahead request is answered at all, that a
# continuation token survives the wire, or that the same parser reads both
# pages — and those go stale quietly: a refused typeahead reads as "no rows"
# rather than as an error, and paging reads as "there was nothing more".
#
# Run signed out, exactly as the Search screen does on a device with no account.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"

fw="$repo/AppleApp/Frameworks/BitChordShared.xcframework/macos-arm64"
bin="$(mktemp -d)/search-verify"
xcrun swiftc -O -o "$bin" \
  -F "$fw" -framework BitChordShared \
  -Xlinker -rpath -Xlinker "$fw/BitChordShared.framework/Versions/A" \
  "$here/search-verify/main.swift" || exit 2

exec "$bin"
