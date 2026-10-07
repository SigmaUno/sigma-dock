# SigmaDock terminal

GPUI terminal backed by Alacritty, with live appearance and cursor settings.
Derived from [gpui-terminal 0.1.0](https://github.com/zortax/gpui-terminal).
See NOTICE and the original MIT/Apache-2.0 license texts.

## Default terminal font

The `monospace` preference resolves to bundled JetBrains Mono (Regular/Bold/Italic/BoldItalic), registered once per GPUI application. Measurement and painting use the same concrete family on Linux and macOS. This avoids GPUI 0.2.2's Linux fallback to a proportional font when it cannot resolve the generic family name. Fonts are embedded; no OS font install, network request or global font change is required. Explicit custom families remain user-controlled. Fonts are distributed under the included SIL Open Font License 1.1.
