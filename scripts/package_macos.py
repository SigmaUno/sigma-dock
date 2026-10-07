#!/usr/bin/env python3
"""Build a relocatable macOS app and DMG from already-built native binaries."""
import argparse
import hashlib
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
BINARIES = ['sigma-dock', 'sigma-dockerd', 'sigma-dock-mcp', 'sdk']


def run(*command):
    subprocess.run(command, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, default=ROOT / 'dist')
    parser.add_argument('--version', required=True)
    parser.add_argument('--build-id', required=True)
    parser.add_argument('--build-number', default='1')
    parser.add_argument('--arch', choices=['arm64', 'x86_64', 'universal'], default='universal')
    parser.add_argument('--app-only', action='store_true', help='Skip disk-image creation for local bundle tests')
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('macOS packaging requires a Mac with Xcode command-line tools')
    if not re.fullmatch(r'\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?', args.version):
        parser.error('Invalid version')
    if not re.fullmatch(r'[0-9a-f]{7,40}', args.build_id) or not args.build_number.isdecimal():
        parser.error('Invalid build ID or build number')
    identity = os.environ.get('APPLE_SIGNING_IDENTITY', '')
    notary = [os.environ.get(key) for key in ['APPLE_NOTARY_KEY_PATH', 'APPLE_API_KEY_ID', 'APPLE_API_ISSUER']]
    if any(notary) and (not all(notary) or not identity):
        parser.error('Notarization requires a signing identity and all API key settings')
    args.output.mkdir(parents=True, exist_ok=True)
    app = args.output / 'SigmaDock.app'
    if app.exists():
        parser.error(f'{app} already exists; choose a fresh output directory')
    macos = app / 'Contents' / 'MacOS'
    resources = app / 'Contents' / 'Resources'
    macos.mkdir(parents=True)
    resources.mkdir()
    expected = {'arm64', 'x86_64'} if args.arch == 'universal' else {args.arch}
    for name in BINARIES:
        source = args.bin_dir / name
        arches = set(subprocess.check_output(['lipo', '-archs', str(source)], text=True).split())
        if arches != expected:
            parser.error(f'{name}: expected {expected}, received {arches}')
        dependencies = subprocess.check_output(['otool', '-L', str(source)], text=True)
        for line in dependencies.splitlines():
            if ' (compatibility version' in line:
                library = line.strip().split(' (compatibility version')[0]
                if not library.startswith(('/usr/lib/', '/System/Library/')):
                    parser.error(f'{name} depends on an unbundled library: {library}')
        shutil.copy2(source, macos / name)
        (macos / name).chmod(0o755)
    info = {
        'CFBundleName': 'SigmaDock', 'CFBundleDisplayName': 'SigmaDock',
        'CFBundleIdentifier': 'com.sigmauno.sigmadock', 'CFBundleExecutable': 'sigma-dock',
        'CFBundlePackageType': 'APPL', 'CFBundleInfoDictionaryVersion': '6.0',
        'CFBundleShortVersionString': args.version.split('-')[0],
        'CFBundleVersion': args.build_number, 'LSMinimumSystemVersion': '13.0',
        'NSHighResolutionCapable': True, 'SigmaDockSourceCommit': args.build_id,
    }
    with (app / 'Contents' / 'Info.plist').open('wb') as file:
        plistlib.dump(info, file)
    shutil.copy2(ROOT / 'LICENSE', resources / 'LICENSE')
    for name in ('NOTICE', 'LICENSE-MIT', 'LICENSE-APACHE'):
        shutil.copy2(ROOT / 'crates/sigma-dock-terminal' / name, resources / ('terminal-' + name))
    shutil.copy2(ROOT / 'packaging/macos/INSTALL.txt', resources / 'INSTALL.txt')
    # Sign nested helpers first. Signing the main executable inside an app
    # also discovers the enclosing bundle and requires its helpers to be signed.
    for name in BINARIES[1:]:
        command = ['codesign', '--force', '--sign', identity or '-']
        if identity: command += ['--options', 'runtime', '--timestamp']
        run(*command, str(macos / name))
    command = ['codesign', '--force', '--sign', identity or '-']
    if identity: command += ['--options', 'runtime', '--timestamp']
    run(*command, str(app))
    run('codesign', '--verify', '--deep', '--strict', str(app))
    if args.app_only:
        print(app)
        return
    suffix = 'notarized' if all(notary) else ('signed' if identity else 'test')
    dmg = args.output / f'SigmaDock-{args.version}-{args.build_id}-{args.arch}-{suffix}.dmg'
    with tempfile.TemporaryDirectory(prefix='sigma-dmg-') as directory:
        staging = Path(directory)
        shutil.copytree(app, staging / app.name, symlinks=True)
        (staging / 'Applications').symlink_to('/Applications')
        shutil.copy2(ROOT / 'packaging/macos/INSTALL.txt', staging / 'INSTALL.txt')
        run('hdiutil', 'create', '-volname', 'SigmaDock', '-srcfolder', str(staging),
            '-ov', '-format', 'UDZO', str(dmg))
    if identity:
        run('codesign', '--sign', identity, '--timestamp', str(dmg))
    if all(notary):
        run('xcrun', 'notarytool', 'submit', str(dmg), '--key', notary[0],
            '--key-id', notary[1], '--issuer', notary[2], '--wait')
        run('xcrun', 'stapler', 'staple', str(dmg))
        run('xcrun', 'stapler', 'validate', str(dmg))
    digest = hashlib.sha256(dmg.read_bytes()).hexdigest()
    (args.output / (dmg.name + '.sha256')).write_text(f'{digest}  {dmg.name}\n')
    print(dmg)


if __name__ == '__main__':
    main()
