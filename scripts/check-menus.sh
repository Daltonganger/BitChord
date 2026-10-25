#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
python3 - "$root" <<'PY'
import sys
from pathlib import Path

root = Path(sys.argv[1])
menus = (root / "AppleApp/Sources/UI/ActionMenus.swift").read_text()
detail = (root / "AppleApp/Sources/UI/DetailView.swift").read_text()

checks = {
    "player menu offers the retained audio cut":
        'controller.songVersions[entry.id] != nil' in menus and 'Button("Convert to Audio"' in menus,
    "collection download reports waiting and downloaded states":
        'Label("Download All"' in menus and '"Waiting"' in menus and '"Downloaded"' in menus,
    "album and playlist cards share music.youtube.com release links":
        'music.youtube.com/playlist?list=' in menus and 'music.youtube.com/browse/' in menus,
    "collection download deletion keeps its modal confirmation":
        'toast.requestConfirmation(ConfirmationRequest(' in menus and 'confirm: "Delete Download"' in menus,
    "partial collection downloads remain deletable when idle and not failed":
        'if let owner, !downloading, owner.requests.allSatisfy' in menus
        and '?.status != .failed' in menus,
    "owned playlist deletion keeps its modal confirmation":
        'Button("Delete Playlist", role: .destructive)' in detail
        and 'confirmDelete(page)' in detail
        and 'title: "Delete this playlist?"' in detail,
    "release overflow contains queue and download actions":
        'releaseMoreMenu(page)' in detail and 'Button("Play Next")' in detail
        and 'Button("Add to Queue")' in detail and 'CollectionDownloadActions(browseId: browseId' in detail,
}

failed = [name for name, passed in checks.items() if not passed]
for name, passed in checks.items():
    print(f"{'PASS' if passed else 'FAIL'}: {name}")
if failed:
    raise SystemExit(1)
PY
