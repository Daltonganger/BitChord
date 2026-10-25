#!/bin/bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
require() {
  local file="$1" pattern="$2"
  if ! grep -Fq "$pattern" "$repo/$file"; then
    echo "missing artist-signature contract: $file: $pattern" >&2
    exit 1
  fi
}

require AppleApp/Sources/UI/SearchView.swift 'isCurrent: isCurrentlyPlaying(hit)'
require AppleApp/Sources/UI/SearchView.swift 'private func qualityBadge(_ raw: String?)'
require AppleApp/Sources/App/ArtistSignatureRepository.swift 'https://commons.wikimedia.org/w/api.php'
require AppleApp/Sources/App/ArtistSignatureRepository.swift 'iiurlwidth'
require AppleApp/Sources/App/ArtistSignatureRepository.swift 'parseResponse'
require AppleApp/Sources/UI/DetailView.swift 'artist_signatures_enabled'
require AppleApp/Sources/UI/DetailView.swift 'artist_signatures_wikimedia_url'
require AppleApp/Sources/UI/DetailView.swift 'Signed by '
require AppleApp/Sources/UI/DetailView.swift 'for delaySeconds in [0, 5, 15, 30]'
require AppleApp/tests/ArtistSignatureRepositoryTests.swift 'testParseSelectsMatchingSvgThumbnail'

binary="$(mktemp "${TMPDIR:-/tmp}/bitchord-artist-signature-tests.XXXXXX")"
trap 'rm -f "$binary"' EXIT
swiftc -parse-as-library \
  "$repo/AppleApp/Sources/App/ArtistSignatureRepository.swift" \
  "$repo/AppleApp/tests/ArtistSignatureRepositoryTests.swift" \
  -o "$binary"
"$binary"

echo "artist signature and search source contracts passed"
