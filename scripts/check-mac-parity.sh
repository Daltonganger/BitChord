#!/usr/bin/env bash
# Source-contract checks for the Mac shell affordances added in I13.
set -euo pipefail

cd "$(dirname "$0")/.."

require() {
  local file="$1" needle="$2" description="$3"
  if grep -Fq -- "$needle" "$file"; then
    printf '  ok   %s\n' "$description"
  else
    printf '  FAIL %s\n' "$description" >&2
    return 1
  fi
}

require AppleApp/Sources/UI/MacMiniPlayer.swift \
  'applicationShouldTerminateAfterLastWindowClosed' 'close-to-tray controls last-window termination'
require AppleApp/Sources/UI/MacMiniPlayer.swift \
  'applicationShouldHandleReopen' 'Dock reopen is handled'
require AppleApp/Sources/UI/MacMiniPlayer.swift \
  'mac.closeToTray' 'close-to-tray preference is shared with the menu-bar toggle'
require AppleApp/Sources/UI/MacMiniPlayer.swift \
  'Keep BitChord open when windows close' 'menu-bar player exposes the close-to-tray toggle'
require AppleApp/Sources/UI/MacMiniPlayer.swift \
  'stop_music_on_close' 'window close honors the Stop Music on Close preference'
require AppleApp/Sources/UI/MacShell.swift \
  'MacCompactNavigationBar()' 'compact-width shell shows navigation'
require AppleApp/Sources/UI/MacShell.swift \
  'appModel.requestedTab = tab' 'compact navigation routes through the root tab request'
require AppleApp/Sources/App/BitChordApp.swift \
  '.modifier(MacWindowAccountToolbar())' 'window-level toolbar exposes account switching once'
require AppleApp/Sources/UI/MacMiniPlayer.swift \
  'window.deminiaturize(nil)' 'Dock reopen restores a minimized main window'
require AppleApp/Sources/UI/MacMiniPlayer.swift \
  'contains(MiniPlayerWindow.id)' 'Dock reopen excludes the mini-player window'
require AppleApp/Sources/App/BitChordApp.swift \
  'SettingsDeepLink.handle(url)' 'scene accepts Settings deep links'
require AppleApp/Sources/App/BitChordApp.swift \
  '.background(SettingsDeepLinkOpener())' 'Mac opens Settings for a deep-link request'
require AppleApp/Sources/App/BitChordApp.swift \
  'NSApp.isHidden || (!NSApp.isActive && hasVisibleWindow)' 'background pausing does not mistake a last-window close for app deactivation'
require AppleApp/Sources/UI/FeedViews.swift \
  '.animation(reduceMotion ? nil : .easeOut(duration: 0.14), value: hovering)' 'cards animate their hover lift unless Reduce Motion is on'
require AppleApp/Sources/UI/FeedViews.swift \
  '.scaleEffect(hovering && !reduceMotion ? 1.03 : 1)' 'cards lift while hovered'
require AppleApp/Sources/UI/Library/LocalMusicView.swift \
  'MacCardHoverLift()' 'local album cards use the Mac hover lift'
require AppleApp/Sources/UI/Library/LibraryGridView.swift \
  'MacCardHoverLift()' 'local track cards use the Mac hover lift'
require AppleApp/Sources/UI/Library/LibraryLandingView.swift \
  'MacCardHoverLift()' 'on-device cards use the Mac hover lift'
