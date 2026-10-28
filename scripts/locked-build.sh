#!/bin/zsh
# Build the Apple app while another agent may be building from the same checkout.
#
#   scripts/locked-build.sh mac|ios|ipad [derived-data-dir]
#
# xcodegen rewrites BitChord.xcodeproj, so two builds started together can read
# a half-written project. The lock is a directory (mkdir is atomic); a lock
# older than 30 minutes is taken to be left behind by a killed build. Exit
# status is xcodebuild's; only errors and this project's warnings are printed.
set -u
cd "$(dirname "$0")/../AppleApp" || exit 2

platform=${1:-mac}
derived=${2:-}
lock=/tmp/bitchord-build.lock

case $platform in
    mac) destination='platform=macOS,arch=arm64' ;;
    ios) destination="platform=iOS,id=${BITCHORD_IOS_DEVICE_ID:?Set BITCHORD_IOS_DEVICE_ID}" ;;
    ipad) destination="platform=iOS,id=${BITCHORD_IPAD_DEVICE_ID:?Set BITCHORD_IPAD_DEVICE_ID}" ;;
    *) echo "usage: $0 mac|ios|ipad [derived-data-dir]" >&2; exit 2 ;;
esac

waited=0
until mkdir "$lock" 2>/dev/null; do
    if [[ -n $(find "$lock" -maxdepth 0 -mmin +30 2>/dev/null) ]]; then
        echo "removing a stale build lock" >&2
        rmdir "$lock" 2>/dev/null
        continue
    fi
    (( waited == 0 )) && echo "waiting for the other build to finish…" >&2
    waited=1
    sleep 5
done
trap 'rmdir "$lock" 2>/dev/null' EXIT INT TERM

xcodegen generate --spec project.yml --quiet || exit 3

args=(-project BitChord.xcodeproj -scheme BitChord -destination "$destination"
      -configuration Debug -allowProvisioningUpdates)
[[ -n ${DEVELOPMENT_TEAM:-} ]] && args+=("DEVELOPMENT_TEAM=$DEVELOPMENT_TEAM")
[[ -n $derived ]] && args+=(-derivedDataPath "$derived")

xcodebuild "${args[@]}" build -quiet 2>&1 | grep -E 'error:|warning: .*BitChord/AppleApp|BUILD' | head -60
exit ${pipestatus[1]}
