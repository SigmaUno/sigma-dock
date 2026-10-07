#!/usr/bin/env python3
"""Render the SigmaDock icon SVGs into AppIcon.icns and PNG sizes.

Writes the .icns container directly (PNG-backed entries), so it runs on any OS
and does not need Xcode's iconutil. Rendering uses the first available of
resvg, rsvg-convert or the cairosvg Python module.
"""
import argparse
import io
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
ICON_DIR = ROOT / 'assets' / 'icon'
SOURCE = ICON_DIR / 'sigmadock.svg'
SMALL_SOURCE = ICON_DIR / 'sigmadock-small.svg'
SMALL_MAX_PX = 32  # renditions at or below this size use the simplified artwork

# icns entry type -> pixel size (all PNG-encoded; supported since macOS 10.7)
ICNS_ENTRIES = [
    ('icp4', 16), ('ic11', 32),    # 16pt @1x/@2x
    ('icp5', 32), ('ic12', 64),    # 32pt @1x/@2x
    ('ic07', 128), ('ic13', 256),  # 128pt @1x/@2x
    ('ic08', 256), ('ic14', 512),  # 256pt @1x/@2x
    ('ic09', 512), ('ic10', 1024), # 512pt @1x/@2x
]
PNG_SIZES = [16, 32, 48, 64, 128, 256, 512, 1024]
PNG_MAGIC = b'\x89PNG\r\n\x1a\n'


def renderer():
    if shutil.which('resvg'):
        def render(svg, px, out):
            subprocess.run(['resvg', '-w', str(px), '-h', str(px), str(svg), str(out)], check=True)
        return 'resvg', render
    if shutil.which('rsvg-convert'):
        def render(svg, px, out):
            subprocess.run(['rsvg-convert', '-w', str(px), '-h', str(px), '-o', str(out), str(svg)], check=True)
        return 'rsvg-convert', render
    try:
        import cairosvg
    except ImportError:
        sys.exit('No SVG renderer found: install resvg (brew install resvg), librsvg, or cairosvg')

    def render(svg, px, out):
        cairosvg.svg2png(url=str(svg), write_to=str(out), output_width=px, output_height=px)
    return 'cairosvg', render


def png_size(data):
    if data[:8] != PNG_MAGIC:
        raise ValueError('not a PNG')
    return struct.unpack('>II', data[16:24])


def write_icns(path, entries):
    body = b''.join(kind.encode() + struct.pack('>I', len(data) + 8) + data for kind, data in entries)
    path.write_bytes(b'icns' + struct.pack('>I', len(body) + 8) + body)


def verify_icns(path):
    data = path.read_bytes()
    if data[:4] != b'icns' or struct.unpack('>I', data[4:8])[0] != len(data):
        raise SystemExit(f'{path}: bad icns header')
    found, offset = {}, 8
    while offset < len(data):
        kind = data[offset:offset + 4].decode()
        length = struct.unpack('>I', data[offset + 4:offset + 8])[0]
        found[kind] = png_size(data[offset + 8:offset + length])
        offset += length
    for kind, px in ICNS_ENTRIES:
        if found.get(kind) != (px, px):
            raise SystemExit(f'{path}: {kind} should be {px}x{px} PNG, found {found.get(kind)}')
    print(f'OK: {path.relative_to(ROOT)} has {len(ICNS_ENTRIES)} PNG renditions (16-1024px)')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--verify', action='store_true',
                        help='Only validate the committed AppIcon.icns (no rendering)')
    args = parser.parse_args()
    icns = ICON_DIR / 'AppIcon.icns'
    if args.verify:
        verify_icns(icns)
        return
    name, render = renderer()
    png_dir = ICON_DIR / 'png'
    png_dir.mkdir(exist_ok=True)
    cache = {}
    with tempfile.TemporaryDirectory(prefix='sigma-icon-') as tmp:
        for px in sorted(set(PNG_SIZES) | {px for _, px in ICNS_ENTRIES}):
            out = Path(tmp) / f'{px}.png'
            render(SMALL_SOURCE if px <= SMALL_MAX_PX else SOURCE, px, out)
            cache[px] = out.read_bytes()
            if png_size(cache[px]) != (px, px):
                sys.exit(f'{name} rendered {png_size(cache[px])} for {px}px')
    for px in PNG_SIZES:
        (png_dir / f'sigmadock-{px}.png').write_bytes(cache[px])
    write_icns(icns, [(kind, cache[px]) for kind, px in ICNS_ENTRIES])
    print(f'Rendered with {name}: {icns.relative_to(ROOT)} and {len(PNG_SIZES)} PNGs')
    verify_icns(icns)


if __name__ == '__main__':
    main()
