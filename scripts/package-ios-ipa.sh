#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Build an unsigned Release IPA for sideloading or signing with your own team.

Usage:
  scripts/package-ios-ipa.sh <version> [--output-dir DIR] [--overwrite]

The script archives the iOS app with code signing disabled, removes embedded
provisioning profiles, and writes BitChord-VERSION-unsigned.ipa.
USAGE
}
fail() { printf 'Error: %s\n' "$*" >&2; exit 1; }
if [[ $# -eq 1 && ( "$1" == '-h' || "$1" == '--help' ) ]]; then usage; exit 0; fi
[[ $# -ge 1 ]] || { usage >&2; exit 2; }
version="$1"; shift
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'version must look like 1.0.0'
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
output_dir="$repo_root/build/release"
overwrite=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-dir) [[ $# -ge 2 ]] || fail '--output-dir needs a path'; output_dir="$2"; shift 2 ;;
    --overwrite) overwrite=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "Unknown option: $1" ;;
  esac
done
command -v xcodebuild >/dev/null 2>&1 || fail 'xcodebuild is required'
command -v ditto >/dev/null 2>&1 || fail 'ditto is required'
command -v zipinfo >/dev/null 2>&1 || fail 'zipinfo is required'
mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"
ipa="$output_dir/BitChord-$version-unsigned.ipa"
[[ ! -e "$ipa" || "$overwrite" -eq 1 ]] || fail "IPA already exists: $ipa (pass --overwrite to replace it)"
stage="$(mktemp -d "${TMPDIR:-/tmp}/bitchord-ipa.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
archive="$stage/BitChord.xcarchive"
app="$archive/Products/Applications/BitChord.app"

xcodebuild \
  -project "$repo_root/AppleApp/BitChord.xcodeproj" \
  -scheme BitChord \
  -configuration Release \
  -sdk iphoneos \
  -destination 'generic/platform=iOS' \
  -archivePath "$archive" \
  -derivedDataPath "$stage/DerivedData" \
  archive \
  CODE_SIGNING_ALLOWED=NO \
  CODE_SIGNING_REQUIRED=NO \
  CODE_SIGN_IDENTITY=
[[ -d "$app" ]] || fail "archive did not contain BitChord.app: $app"
plist="$app/Info.plist"
short_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$plist")"
[[ "$short_version" == "$version" ]] || fail "archive is version $short_version, not requested version $version"
mkdir -p "$stage/package/Payload"
ditto "$app" "$stage/package/Payload/BitChord.app"
find "$stage/package/Payload/BitChord.app" -name embedded.mobileprovision -type f -delete
# A release IPA is deliberately unsigned; don't leave export/archive profiles
# behind even if Xcode emitted one for a nested extension.
find "$stage/package/Payload/BitChord.app" -name embedded.provisionprofile -type f -delete
# zip updates an existing archive instead of clearing entries that disappeared
# from a rebuilt app, so start fresh when the caller explicitly allows overwrite.
rm -f "$ipa"
/usr/bin/zip -qry "$ipa" Payload
zipinfo -1 "$ipa" | grep -Fxq 'Payload/BitChord.app/Info.plist' || fail 'IPA archive is missing Payload/BitChord.app'
printf 'Created unsigned IPA: %s\n' "$ipa"
printf 'No developer signature or provisioning profile is included. iCloud sync requires signing with your own team and the relevant iCloud capabilities.\n'
