#!/usr/bin/env bash
# Fetch the development/CI beat graph into AppleApp/Resources/Models/.
# The app downloads the full Beat This! final0 graph at runtime; this small0
# graph is the bundle fallback for a dev box / CI build without a network step
# (same I/O contract: mel [1, T, 128] -> two logit heads). The vocals graph is
# NOT fetched: it is bundled and committed as
# AppleApp/Resources/Models/vocals_umxhq_int8.onnx (open-unmix umxhq, MIT; see
# THIRD_PARTY_NOTICES.md) because the app needs it on every device.
#
# Pinned to a revision and checked against a SHA-256, the same fact the app
# itself downloads by (see AutomixModelStore.swift). A branch can move and a tag
# can be re-pointed; a commit and a digest cannot, and a dev box and a listener's
# device must end up with the same bytes or parity claims mean nothing.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/AppleApp/Resources/Models"
mkdir -p "$DEST"

# Pinned Hugging Face revision. The repository declares MIT.
BEAT_URL="https://huggingface.co/ashudesai/songbird-models/resolve/345312bf5604913b3c0d05815f93c81c4b114e4d/small0.onnx"

BEAT_SHA="c462b064a4033050ca6a5354bf866ff4610ab1fd8f1dec1e494b20b22d870aea"

fetch() {
  local url="$1" out="$2" want="$3"
  if [[ -s "$out" ]] && [[ "$(shasum -a 256 "$out" | awk '{print $1}')" == "$want" ]]; then
    echo "already present and verified: $out"
    return
  fi
  echo "downloading $(basename "$out")…"
  curl -L --fail --retry 3 -o "$out.tmp" "$url"
  local got
  got="$(shasum -a 256 "$out.tmp" | awk '{print $1}')"
  if [[ "$got" != "$want" ]]; then
    rm -f "$out.tmp"
    echo "checksum mismatch for $(basename "$out")" >&2
    echo "  expected $want" >&2
    echo "  got      $got" >&2
    exit 1
  fi
  mv "$out.tmp" "$out"
  echo "verified $(basename "$out")"
}

fetch "$BEAT_URL" "$DEST/beat_this.onnx" "$BEAT_SHA"
ls -lh "$DEST"/*.onnx
