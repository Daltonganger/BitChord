# macOS release DMG

The 1.0.0 DMG is unsigned by a developer and is not notarized. The packaging
script ad-hoc signs a staging copy so its nested code has consistent signatures.
It removes provisioning profiles and restricted entitlements; the released Mac
build therefore does not use iCloud Key-Value Store or shared iCloud Keychain
sessions. The app's local lyrics choices and local sign-in continue to work.
The widget cannot use the app-group shared container in this ad-hoc package.

## Build and package

Create an Apple Silicon Release archive without code signing, then copy its app
bundle into a folder named `BitChord.app` inside an export directory:

```sh
xcodebuild \
  -project AppleApp/BitChord.xcodeproj \
  -scheme BitChord \
  -configuration Release \
  -destination 'platform=macOS,arch=arm64' \
  -archivePath "$TMPDIR/BitChord-mac.xcarchive" \
  archive CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY=

mkdir -p "$TMPDIR/BitChord-mac-export"
ditto "$TMPDIR/BitChord-mac.xcarchive/Products/Applications/BitChord.app" \
  "$TMPDIR/BitChord-mac-export/BitChord.app"
scripts/package-mac-dmg.sh 1.0.0 "$TMPDIR/BitChord-mac-export"
```

The script checks the app version, stages a copy, removes provisioning profiles
and old signatures, enables the macOS app sandbox, and ad-hoc signs nested code
inside-out. The ad-hoc entitlement set contains only the app sandbox, network
client, and user-selected file access. It removes iCloud KVS, App Groups, shared
Keychain groups, and generated application/team identifiers. iCloud and App
Group capabilities need a team-signed build; the App Group-backed widget cannot
share its container in this package. The script checks the app signature, arm64
slice, Team ID and entitlement payload, then builds a 1x/2x Finder background,
sets the volume icon and layout, closes and reopens the Finder window to read
back its saved `.DS_Store` state, compresses the DMG, and runs `hdiutil verify`.
The original export is left untouched.

The package script accepts `--output-dir DIR` and requires `--overwrite` to
replace an existing local DMG. GitHub upload and release options remain separate
and ask before changing GitHub; `--create-release` requires an existing pushed
`vVERSION` tag. No tag, signature, notarization, or upload is created by this
local packaging command.

## Open BitChord on macOS

Because the download is not Developer ID signed or notarized, Gatekeeper may
block its first launch. Only continue if you downloaded the DMG from the
official BitChord release and trust it.

1. Drag BitChord to Applications.
2. In Applications, Control-click BitChord and choose **Open**, then confirm
   **Open** in the warning dialog. If that choice is not available, open
   System Settings → Privacy & Security and choose **Open Anyway** for BitChord.

After moving the app to Applications, an informed user can also clear the
quarantine flag in Terminal:

```sh
xattr -dr com.apple.quarantine "/Applications/BitChord.app"
open "/Applications/BitChord.app"
```

This removes the downloaded-file quarantine attribute; it does not add a
Developer ID signature or notarization.
