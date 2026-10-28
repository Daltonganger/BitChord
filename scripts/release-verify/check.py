import importlib.util
import sys
sys.dont_write_bytecode = True
from pathlib import Path
import tempfile

spec = importlib.util.spec_from_file_location('release', Path(__file__).resolve().parents[1] / 'prepare-apple-release.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

with tempfile.TemporaryDirectory() as temporary:
    root = Path(temporary)
    ipa, dmg = root / 'BitChord-build13.ipa', root / 'BitChord-build13.dmg'
    ipa.write_bytes(b'ios fixture'); dmg.write_bytes(b'mac fixture')
    plist = dict(CFBundleIdentifier='app.bitchord.BitChord', CFBundleShortVersionString='1.0.4', CFBundleVersion='13', BitChordRevision='abc1234')
    output = root / 'release'
    manifest = module.prepare(ipa, dmg, plist, plist, output)
    assert manifest['build'] == 13 and len(manifest['artifacts']) == 2
    assert manifest['artifacts'][0]['sha256'] == module.digest(ipa)
    old = (output / 'release.json').read_bytes()
    for change in [dict(CFBundleVersion='12'), dict(CFBundleShortVersionString='1.0.3'), dict(CFBundleIdentifier='other.app'), dict(BitChordRevision='def5678'), dict(BitChordRevision='unknown'), dict(BitChordRevision='abc1234-dirty')]:
        try:
            module.prepare(ipa, dmg, plist, dict(plist, **change), output)
            raise AssertionError('Mismatched package accepted')
        except ValueError:
            assert (output / 'release.json').read_bytes() == old
print('PASS paired release identity, source revision, checksums and refusal without replacing good metadata')
