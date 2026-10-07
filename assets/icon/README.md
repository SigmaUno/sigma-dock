# SigmaDock icon

The SigmaDock icon is a sigma drawn as a shell prompt: the top bar and `>`
chevron form the Σ, and the bottom bar is detached as a `_` cursor. Colours are
from Catppuccin Mocha (Lavender/Mauve glyph, Green cursor, Surface0→Crust
background).

## Files

| File | Purpose |
| --- | --- |
| `sigmadock.svg` | Source artwork, used for renditions of 64px and larger |
| `sigmadock-small.svg` | Simplified artwork for 16px and 32px renditions |
| `AppIcon.icns` | Generated macOS icon, copied into `SigmaDock.app/Contents/Resources` |
| `png/sigmadock-<size>.png` | Generated PNGs (16–1024px) for Linux packages and docs |

Only the SVGs are edited by hand. After changing either one, regenerate the
outputs and commit them together:

```sh
# needs one of: resvg, rsvg-convert, or the cairosvg Python module
#   brew install resvg
python3 scripts/build_icons.py
python3 scripts/build_icons.py --verify  # validates AppIcon.icns, no renderer needed
```

## License

Original artwork created for SigmaDock. Licensed under the Apache License 2.0,
the same as the rest of the repository (see `LICENSE`). No third-party artwork
is used.
