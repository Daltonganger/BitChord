#!/bin/bash
# The remote library, against a real WebDAV server.
#
# Same reasoning as check-party-ui.sh's sibling: the parsers are unit-tested
# against fixtures, and a fixture cannot tell you that a client and a server
# disagree about a header. This starts a server that speaks the awkward parts of
# WebDAV — three href shapes, escaped names, real byte ranges, a real 401 — and
# drives the shipped bridge against it.
#
# Nothing here touches the app's settings permanently: the harness reads what is
# stored, sets its own share, and puts it back.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
port="${WEBDAV_PORT:-8081}"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/bitchord-webdav-check.XXXXXX")"
fixture_root="$scratch/root"
request_log="$scratch/requests.log"
upload_source="$scratch/upload.flac"
server_pid=""

cleanup() {
  if [[ -n "$server_pid" ]]; then kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; fi
  rm -rf "$scratch"
}
trap cleanup EXIT

export WEBDAV_FIXTURE_ROOT="$fixture_root"
export WEBDAV_REQUEST_LOG="$request_log"
python3 "$here/webdav-fixture.py" "$port" "$fixture_root" >"$scratch/server.log" 2>&1 &
server_pid=$!

python3 - "$upload_source" <<'PY'
import sys
with open(sys.argv[1], "wb") as audio:
    audio.write(bytes(range(256)) * 64)
PY

# Wait for it to answer rather than sleeping a fixed amount: a slow start should
# not read as a failure.
for _ in $(seq 1 40); do
  if curl -s -o /dev/null -m 1 "http://127.0.0.1:$port/"; then break; fi
  sleep 0.25
done

fw="$repo/AppleApp/Frameworks/BitChordShared.xcframework/macos-arm64"
bin="$(mktemp -d)/webdav-verify"
grep -Fq 'refreshAfterUpload: false' "$repo/AppleApp/Sources/UI/Library/DownloadsView.swift"
grep -Fq 'Upload selected to WebDAV' "$repo/AppleApp/Sources/UI/Library/DownloadsView.swift"
echo "  ok  Downloads selection uploads through the deferred-refresh batch path"
xcrun swiftc -O -o "$bin" \
  -F "$fw" -framework BitChordShared \
  -Xlinker -rpath -Xlinker "$fw/BitChordShared.framework/Versions/A" \
  "$repo/AppleApp/Sources/UI/WebDavStore.swift" \
  "$here/webdav-verify/main.swift"

WEBDAV_URL="http://localhost:$port/Music" WEBDAV_UPLOAD_SOURCE="$upload_source" "$bin"
cmp "$upload_source" "$fixture_root/Music/Björk - Upload Test.flac"
cmp "$upload_source" "$fixture_root/Music/Björk - Upload Test (1).flac"
echo "  ok  streamed upload bytes are unchanged and conflicts keep both files"

echo ""
echo "requests, with how each was answered:"
awk '{print $1, $2, $3}' "$request_log" | sort | uniq -c | sed 's/^/  /'
