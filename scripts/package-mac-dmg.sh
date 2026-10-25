#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Package an unsigned Release export of BitChord.app as an ad-hoc-signed DMG.

Usage:
  scripts/package-mac-dmg.sh <version> <export-folder> [options]

Options:
  --output-dir DIR  Write BitChord-VERSION.dmg to DIR (default: export-folder)
  --overwrite       Replace an existing local DMG with the same name
  --publish         Upload an asset to an existing GitHub release
  --replace-asset   Replace the same-named asset (requires --publish)
  --create-release  Create a new GitHub release from an existing vVERSION tag
  --notes FILE      Release notes (required for --create-release)
  -h, --help        Show this help

The export is copied before processing and remains untouched. The DMG contains
no provisioning profile, Developer ID signature, or iCloud entitlement. GitHub
changes require confirmation; new releases require an existing pushed tag.
USAGE
}

fail() { printf 'Error: %s\n' "$*" >&2; exit 1; }
if [[ $# -eq 1 && ( "$1" == "-h" || "$1" == "--help" ) ]]; then usage; exit 0; fi
[[ $# -ge 2 ]] || { usage >&2; exit 2; }
version="$1"; export_dir="$2"; shift 2
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
output_dir=""; overwrite=0; publish=0; create_release=0; replace_asset=0; notes_file=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-dir) [[ $# -ge 2 ]] || fail "--output-dir needs a path"; output_dir="$2"; shift 2 ;;
    --overwrite) overwrite=1; shift ;;
    --publish) publish=1; shift ;;
    --replace-asset) replace_asset=1; shift ;;
    --create-release) create_release=1; shift ;;
    --notes) [[ $# -ge 2 ]] || fail "--notes needs a Markdown file"; notes_file="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "Unknown option: $1" ;;
  esac
done
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "version must look like 1.0.0"
[[ "$publish" -eq 0 || "$create_release" -eq 0 ]] || fail "choose either --publish or --create-release"
[[ "$replace_asset" -eq 0 || "$publish" -eq 1 ]] || fail "--replace-asset requires --publish"
[[ -d "$export_dir" ]] || fail "export folder does not exist: $export_dir"
app_source="$export_dir/BitChord.app"
[[ -d "$app_source" ]] || fail "expected BitChord.app inside: $export_dir"
if [[ -z "$output_dir" ]]; then output_dir="$export_dir"; fi
mkdir -p "$output_dir"; output_dir="$(cd "$output_dir" && pwd)"
dmg_path="$output_dir/BitChord-$version.dmg"
[[ ! -e "$dmg_path" || "$overwrite" -eq 1 ]] || fail "DMG already exists: $dmg_path (pass --overwrite to replace it)"
[[ -z "$notes_file" || -f "$notes_file" ]] || fail "release notes file does not exist: $notes_file"
[[ -z "$notes_file" || "$publish" -eq 1 || "$create_release" -eq 1 ]] || fail "--notes requires --publish or --create-release"
[[ "$create_release" -eq 0 || -n "$notes_file" ]] || fail "--create-release requires --notes FILE"

for tool in ditto codesign otool install_name_tool sips tiffutil hdiutil create-dmg osascript file lipo; do
  command -v "$tool" >/dev/null 2>&1 || fail "required tool not found: $tool"
done
[[ -x /usr/libexec/PlistBuddy ]] || fail "required tool not found: /usr/libexec/PlistBuddy"
if [[ "$publish" -eq 1 || "$create_release" -eq 1 ]]; then
  command -v gh >/dev/null 2>&1 || fail "GitHub CLI (gh) is required for --publish"
  gh auth status >/dev/null 2>&1 || fail "sign in to GitHub CLI first (gh auth login)"
  github_repo="$(cd "$repo_root" && gh repo view --json nameWithOwner --jq '.nameWithOwner')" || fail "could not determine GitHub repository"
  github_repo_url="$(cd "$repo_root" && gh repo view --json url --jq '.url')" || fail "could not determine GitHub repository URL"
  tag="v$version"
  if [[ "$publish" -eq 1 ]]; then
    release_assets="$(gh release view "$tag" --repo "$github_repo" --json assets --jq '.assets[].name')" || fail "GitHub release $tag does not exist; use --create-release for a new release"
    if printf '%s\n' "$release_assets" | grep -Fxq "BitChord-$version.dmg" && [[ "$replace_asset" -ne 1 ]]; then
      fail "release already has BitChord-$version.dmg; pass --replace-asset to replace it"
    fi
  elif gh release view "$tag" --repo "$github_repo" >/dev/null 2>&1; then
    fail "GitHub release $tag already exists; use --publish to add or update its asset"
  fi
fi

plist="$app_source/Contents/Info.plist"
[[ -f "$plist" ]] || fail "app Info.plist not found"
short_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$plist")"
build_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$plist")"
[[ "$short_version" == "$version" ]] || fail "export is version $short_version, not requested version $version"
executable="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$plist")"
app_executable="$app_source/Contents/MacOS/$executable"
[[ -f "$app_executable" ]] || fail "app executable not found: $app_executable"
icon_name="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIconFile' "$plist" 2>/dev/null || true)"
[[ -n "$icon_name" ]] || icon_name="AppIcon"
[[ "$icon_name" == *.icns ]] || icon_name="$icon_name.icns"
icon_source="$app_source/Contents/Resources/$icon_name"
[[ -f "$icon_source" ]] || fail "app volume icon not found: $icon_source"
background_source="$repo_root/artwork/cover-exports/DMG-background.png"
[[ -f "$background_source" ]] || fail "DMG background not found: $background_source"

stage="$(mktemp -d "${TMPDIR:-/tmp}/bitchord-dmg.XXXXXX")"
mounted_device=""; mountpoint="$stage/mount"
cleanup() {
  if [[ -n "$mounted_device" ]]; then hdiutil detach "$mounted_device" -quiet >/dev/null 2>&1 || true; fi
  rm -rf "$stage"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$stage/payload" "$mountpoint"
ditto "$app_source" "$stage/payload/BitChord.app"
app="$stage/payload/BitChord.app"
rw_image="$stage/BitChord-$version-rw.dmg"
candidate="$stage/BitChord-$version.dmg"
safe_entitlements="$stage/ad-hoc-entitlements.plist"
source_entitlements="$repo_root/AppleApp/BitChord.entitlements"

printf 'Preparing BitChord %s (build %s) as an unsigned, ad-hoc package…\n' "$version" "$build_version"
# Save the input signature's entitlement claims before stripping it. The source
# plist is also reported below, so unsigned builds still produce an audit list.
codesign -d --entitlements :- "$app" > "$stage/input-entitlements.plist" 2>/dev/null || true
profile_plists=()
profile_index=0
while IFS= read -r -d '' profile; do
  profile_plist="$stage/profile-$profile_index.plist"
  if security cms -D -i "$profile" > "$profile_plist" 2>/dev/null; then
    profile_plists+=("$profile_plist")
    profile_index=$((profile_index + 1))
  fi
done < <(find "$app" -name embedded.provisionprofile -type f -print0)

cat > "$safe_entitlements" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>com.apple.security.app-sandbox</key><true/>
  <key>com.apple.security.network.client</key><true/>
  <key>com.apple.security.files.user-selected.read-write</key><true/>
</dict></plist>
PLIST

printf 'Ad-hoc entitlements retained: app sandbox, network client, user-selected read/write files.\n'
has_input_entitlement() {
  local key="$1" profile_plist
  if /usr/libexec/PlistBuddy -c "Print :$key" "$stage/input-entitlements.plist" >/dev/null 2>&1 \
     || /usr/libexec/PlistBuddy -c "Print :$key" "$source_entitlements" >/dev/null 2>&1; then return 0; fi
  for profile_plist in "${profile_plists[@]}"; do
    if /usr/libexec/PlistBuddy -c "Print :Entitlements:$key" "$profile_plist" >/dev/null 2>&1; then return 0; fi
  done
  return 1
}
for key in \
  com.apple.developer.ubiquity-kvstore-identifier \
  com.apple.security.application-groups \
  com.apple.security.keychain-access-groups \
  application-identifier \
  com.apple.developer.team-identifier; do
  if has_input_entitlement "$key"; then
    case "$key" in
      com.apple.developer.ubiquity-kvstore-identifier) reason="iCloud KVS requires a team-signed entitlement" ;;
      com.apple.security.application-groups) reason="App Groups require a team-signed entitlement; the widget shared container is unavailable" ;;
      com.apple.security.keychain-access-groups) reason="shared Keychain access groups require a team-signed entitlement" ;;
      application-identifier|com.apple.developer.team-identifier) reason="ad-hoc signatures do not carry an application or team identity" ;;
    esac
    printf 'Dropped %s: %s.\n' "$key" "$reason"
  fi
done
printf 'Dropped source com.apple.security.app-sandbox=false; ad-hoc package enables the sandbox.\n'
printf 'Audio-input entitlement omitted: the macOS app does not capture microphone input.\n'

find "$app" -name embedded.provisionprofile -type f -delete
while IFS= read -r -d '' code_bundle; do
  codesign --remove-signature "$code_bundle" >/dev/null 2>&1 || true
done < <(find "$app" -depth \( -name '*.appex' -o -name '*.framework' -o -name '*.xpc' -o -name '*.app' \) -print0)
codesign --remove-signature "$app" >/dev/null 2>&1 || true

if ! otool -l "$app/Contents/MacOS/$executable" | grep -Fq '@executable_path/../Frameworks'; then
  install_name_tool -add_rpath '@executable_path/../Frameworks' "$app/Contents/MacOS/$executable"
fi

# Sign loose Mach-O helpers first, then nested code bundles deepest-first, then
# the containing app. No --deep signing: each nested bundle gets its own seal.
while IFS= read -r -d '' binary; do
  if file -b "$binary" | grep -q 'Mach-O'; then
    codesign --force --sign - --timestamp=none "$binary"
  fi
done < <(find "$app/Contents" -type f -perm -111 -print0)
while IFS= read -r -d '' code_bundle; do
  case "$code_bundle" in
    *.app|*.appex) codesign --force --sign - --timestamp=none --entitlements "$safe_entitlements" "$code_bundle" ;;
    *) codesign --force --sign - --timestamp=none "$code_bundle" ;;
  esac
done < <(find "$app/Contents" -depth \( -name '*.appex' -o -name '*.framework' -o -name '*.xpc' -o -name '*.app' \) -print0)
codesign --force --sign - --timestamp=none --entitlements "$safe_entitlements" "$app"
codesign --verify --deep --strict "$app"
lipo -verify_arch arm64 "$app/Contents/MacOS/$executable"
signature_info="$(codesign -dv --verbose=4 "$app" 2>&1)"
printf '%s\n' "$signature_info" | grep -Fq 'Signature=adhoc' || fail 'staged app is not ad-hoc signed'
printf '%s\n' "$signature_info" | grep -Fq 'TeamIdentifier=not set' || fail 'ad-hoc app unexpectedly has a Team ID'
if printf '%s\n' "$signature_info" | grep -q '^Authority='; then fail 'ad-hoc app unexpectedly has a signing authority'; fi
codesign -d --entitlements :- "$app" > "$stage/final-entitlements.plist" 2>/dev/null
for forbidden in com.apple.developer.ubiquity-kvstore-identifier com.apple.security.application-groups com.apple.security.keychain-access-groups application-identifier com.apple.developer.team-identifier; do
  if /usr/libexec/PlistBuddy -c "Print :$forbidden" "$stage/final-entitlements.plist" >/dev/null 2>&1; then
    fail "restricted entitlement remained after ad-hoc signing: $forbidden"
  fi
done

# Finder uses a 2x TIFF representation on Retina. Keep the source artwork intact.
# Preserve both requested PNG renditions, then convert them to TIFF because
# tiffutil's HiDPI combiner consumes TIFF representations.
sips -z 512 768 -s format png "$background_source" --out "$stage/dmg-background-1x.png" >/dev/null
cp "$background_source" "$stage/dmg-background-2x.png"
sips -s format tiff "$stage/dmg-background-1x.png" --out "$stage/dmg-background-1x.tiff" >/dev/null
sips -s format tiff "$stage/dmg-background-2x.png" --out "$stage/dmg-background-2x.tiff" >/dev/null
tiffutil -cathidpicheck "$stage/dmg-background-1x.tiff" "$stage/dmg-background-2x.tiff" -out "$stage/dmg-background.tiff" >/dev/null

# Use a read/write staging image so the Finder layout can be saved and loaded
# again from its .DS_Store before making the compressed, read-only distribution.
create-dmg \
  --overwrite --format UDRW \
  --volname "BitChord $version" \
  --background "$stage/dmg-background.tiff" \
  --volicon "$icon_source" \
  --window-pos 200 120 \
  --window-size 768 540 \
  --text-size 13 \
  --icon-size 110 \
  --icon "BitChord.app" 192 280 \
  --hide-extension "BitChord.app" \
  --app-drop-link 576 280 \
  --hdiutil-quiet \
  "$rw_image" "$stage/payload"

mounted_device="$(hdiutil attach -readwrite -nobrowse -mountpoint "$mountpoint" "$rw_image" | awk '/\/dev\// {print $1; exit}')"
[[ -n "$mounted_device" ]] || fail 'could not mount the writable DMG'
osascript - "$version" "$mountpoint" <<'APPLESCRIPT'
on run argv
  set volumeName to "BitChord " & item 1 of argv
  set mountPath to item 2 of argv
  tell application "Finder"
    set mountedDisk to disk volumeName
    open mountedDisk
    delay 1
    set targetWindow to container window of mountedDisk
    set current view of targetWindow to icon view
    set toolbar visible of targetWindow to false
    set statusbar visible of targetWindow to false
    set pathbar visible of targetWindow to false
    try
      set sidebar width of targetWindow to 0
    end try
    set bounds of targetWindow to {200, 120, 968, 660}
    set icon size of icon view options of targetWindow to 110
    set text size of icon view options of targetWindow to 13
    set position of item "BitChord.app" of mountedDisk to {192, 280}
    set position of item "Applications" of mountedDisk to {576, 280}
    close targetWindow
    delay 2
    open mountedDisk
    delay 1
    set targetWindow to container window of mountedDisk
    set measuredBounds to bounds of targetWindow
    set measuredToolbar to toolbar visible of targetWindow
    set measuredStatus to statusbar visible of targetWindow
    set measuredPath to pathbar visible of targetWindow
    set measuredSidebarWidth to sidebar width of targetWindow
    if (item 3 of measuredBounds) - (item 1 of measuredBounds) is not equal to 768 then error "Finder did not restore a 768-point content window"
    if (item 4 of measuredBounds) - (item 2 of measuredBounds) is not equal to 540 then error "Finder did not restore the title-bar-adjusted 540-point frame"
    if measuredToolbar or measuredStatus or measuredPath or measuredSidebarWidth is not equal to 0 then error "Finder restored a visible toolbar, status bar, path bar, or sidebar"
    close targetWindow
    return "Finder .DS_Store readback: bounds=" & (measuredBounds as text) & ", toolbar=false, statusbar=false, pathbar=false, sidebarWidth=" & measuredSidebarWidth
  end tell
end run
APPLESCRIPT
# The close/open round-trip above reads back Finder's persisted window state.
# Confirm the saved directory entry exists before detaching the disk image.
[[ -f "$mountpoint/.DS_Store" ]] || fail 'Finder did not save a .DS_Store layout'
hdiutil detach "$mounted_device" -quiet
mounted_device=""
hdiutil convert "$rw_image" -format UDZO -imagekey zlib-level=9 -o "$candidate" >/dev/null
hdiutil verify "$candidate" >/dev/null
mv -f "$candidate" "$dmg_path"
printf 'Created and verified: %s\n' "$dmg_path"

if [[ "$publish" -eq 1 ]]; then
  tag="v$version"
  if [[ "$replace_asset" -eq 1 ]]; then printf 'This will replace BitChord-%s.dmg on GitHub release %s.\n' "$version" "$tag";
  else printf 'This will add BitChord-%s.dmg to GitHub release %s.\n' "$version" "$tag"; fi
  read -r -p 'Continue? [y/N] ' answer
  [[ "$answer" == "y" || "$answer" == "Y" ]] || fail 'publishing cancelled'
  if [[ "$replace_asset" -eq 1 ]]; then gh release upload "$tag" "$dmg_path" --repo "$github_repo" --clobber
  else gh release upload "$tag" "$dmg_path" --repo "$github_repo"; fi
  if [[ -n "$notes_file" ]]; then gh release edit "$tag" --repo "$github_repo" --notes-file "$notes_file"; fi
  printf 'Updated GitHub release: %s/releases/tag/%s\n' "$github_repo_url" "$tag"
elif [[ "$create_release" -eq 1 ]]; then
  tag="v$version"
  printf 'This will create and publish GitHub release %s with the DMG and supplied notes.\n' "$tag"
  read -r -p 'Continue? [y/N] ' answer
  [[ "$answer" == "y" || "$answer" == "Y" ]] || fail 'publishing cancelled'
  gh release create "$tag" "$dmg_path" --repo "$github_repo" --verify-tag --title "BitChord $version" --notes-file "$notes_file"
  printf 'Created GitHub release: %s/releases/tag/%s\n' "$github_repo_url" "$tag"
fi
