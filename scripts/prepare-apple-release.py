#!/usr/bin/env python3
"""Prepare one paired Apple release; never uploads or marks a release latest."""
import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import re
import subprocess
import tempfile
import zipfile

BUNDLE_ID = 'app.bitchord.BitChord'


def identity(plist):
    version = plist.get('CFBundleShortVersionString', '')
    build = str(plist.get('CFBundleVersion', ''))
    if not re.fullmatch(r'\d+\.\d+\.\d+', version) or not build.isdecimal() or int(build) <= 0:
        raise ValueError('Each app must have a stable version and a positive numeric build')
    if plist.get('CFBundleIdentifier') != BUNDLE_ID:
        raise ValueError('Both installers must use the existing BitChord app identity')
    return version, int(build)


def digest(path):
    with path.open('rb') as handle:
        hasher = hashlib.sha256()
        for chunk in iter(lambda: handle.read(1024 * 1024), b''):
            hasher.update(chunk)
        return hasher.hexdigest()


def prepare(ipa, dmg, ios, mac, output):
    version, build = identity(ios)
    if identity(mac) != (version, build):
        raise ValueError('iOS and Mac must have the same version AND build; no release files were written')
    revisions = {ios.get('BitChordRevision', 'unknown'), mac.get('BitChordRevision', 'unknown')}
    if len(revisions) != 1:
        raise ValueError('iOS and Mac must be built from the same revision')
    revision = revisions.pop()
    if not re.fullmatch(r'[a-fA-F0-9]{7,40}', revision):
        raise ValueError('Both installers must embed the same clean source revision; unknown or dirty builds cannot form a published release')
    if ipa.suffix.lower() != '.ipa' or dmg.suffix.lower() != '.dmg':
        raise ValueError('Expected one IPA and one DMG')
    for package in (ipa, dmg):
        if package.name != Path(package.name).name or any(c in package.name for c in '\n\r'):
            raise ValueError('Invalid artifact name')
    artifacts = [
        dict(platform='ios', name=ipa.name, sha256=digest(ipa), minimumOS='iOS ' + ios.get('MinimumOSVersion', '18.0'), installation='sign-and-install'),
        dict(platform='macos', name=dmg.name, sha256=digest(dmg), minimumOS='macOS ' + mac.get('LSMinimumSystemVersion', '15.0'), installation='sparkle-or-dmg'),
    ]
    manifest = dict(schema=1, version=version, build=build, revision=revision, artifacts=artifacts)
    instructions = f'''# BitChord {version} (build {build})

This release contains one iOS IPA and one Apple Silicon Mac DMG, built from revision {revision}.

- **iPhone/iPad:** download `{ipa.name}`, then sign and install it with the same signing app and identity used for the installed BitChord. Keep the existing bundle identity to retain local data.
- **Mac update:** use Version & Updates in BitChord and open the Mac updater. The signed Sparkle feed must name this exact build and DMG.
- **First Mac install/manual recovery:** open `{dmg.name}` and move BitChord to Applications, replacing the installed app. Do not uninstall the app or delete its data first.

The latest release page is the permanent entry point: https://github.com/bagumamartin/BitChord/releases/latest
The in-app build number distinguishes packages that share a marketing version. Downloading an installer does not itself install it.

SHA-256 checksums are listed in SHA256.txt and release.json. Keep these artifacts immutable once published.
'''
    output.mkdir(parents=True, exist_ok=True)
    files = {'release.json': json.dumps(manifest, indent=2) + '\n',
             'SHA256.txt': ''.join(f"{item['sha256']}  {item['name']}\n" for item in artifacts),
             'INSTALL.md': instructions}
    for name, contents in files.items():
        temporary = output / ('.' + name + '.tmp')
        temporary.write_text(contents)
        temporary.replace(output / name)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ipa', required=True, type=Path)
    parser.add_argument('--dmg', required=True, type=Path)
    parser.add_argument('--output-dir', required=True, type=Path)
    args = parser.parse_args()
    if not args.ipa.is_file() or not args.dmg.is_file():
        parser.error('Both existing installers are required')
    with zipfile.ZipFile(args.ipa) as archive:
        ios = plistlib.loads(archive.read('Payload/BitChord.app/Info.plist'))
    with tempfile.TemporaryDirectory(prefix='bitchord-release-') as temporary:
        mount = Path(temporary) / 'mount'
        mount.mkdir()
        subprocess.run(['hdiutil', 'attach', '-readonly', '-nobrowse', '-mountpoint', str(mount), str(args.dmg.resolve())], check=True, capture_output=True)
        try:
            mac = plistlib.loads((mount / 'BitChord.app/Contents/Info.plist').read_bytes())
            result = prepare(args.ipa, args.dmg, ios, mac, args.output_dir)
        finally:
            subprocess.run(['diskutil', 'eject', str(mount)], check=True, capture_output=True)
    print(f"Prepared BitChord {result['version']} build {result['build']}; no release was published")


if __name__ == '__main__':
    main()
