# Dependency policy

`cargo deny check` enforces telemetry bans, license/source policy, vulnerability and unsoundness advisories. GPUI and the terminal component are pinned to published releases for reproducible builds. The lockfile is checked in. Native dependencies carry maintenance risk, and a clean advisory check is not proof of absence of vulnerabilities or telemetry.

The current native dependency stack has the following **maintenance-only** exceptions in `deny.toml`. No vulnerability advisory is exempted. These should be removed as soon as the native stack can migrate to maintained replacements; review at every dependency update and before release.

| Advisory | Transitive crate | Migration direction |
|---|---|---|
| RUSTSEC-2025-0052 | async-std | Await GPUI executor migration |
| RUSTSEC-2024-0384 | instant | Replace through the native graphics dependency |
| RUSTSEC-2024-0436 | paste | Migrate macro dependencies to a maintained fork |
| RUSTSEC-2026-0173 | proc-macro-error2 | Migrate macro dependency stack |
| RUSTSEC-2025-0134 | rustls-pemfile | Upgrade GPUI's HTTP helper |
| RUSTSEC-2026-0206 | rustybuzz | Upgrade text shaping dependencies |
| RUSTSEC-2026-0192 | ttf-parser | Upgrade font parsing dependencies |

Application code instantiates a separate forge-only HTTP client; it does not instantiate GPUI's HTTP helper. Build-time and transitively linked libraries remain part of the audit scope.

GPUI's macOS renderer uses its supported `runtime_shaders` feature so development works with Command Line Tools without a separately installed Metal compiler. Shaders compile locally when the native app runs. Terminal feature support still needs qualification against real installed harnesses.

The Linux UI pins `libc` to 0.2.189 because GPUI's `gpui_http_client` pulls `zed-async-tar` with `xattr` 0.2.3, which references `ENOATTR` removed in libc 0.2.190. The pin is present in the published manifest, so fresh downstream Linux builds also receive the compatible version. Remove it when the pinned GPUI stack updates its xattr dependency. All advisory checks still apply to the pinned version.

The `sigmadock-terminal` crate derives from Leonard Seibold’s `gpui-terminal` 0.1.0 (MIT OR Apache-2.0). The source is kept locally to expose cursor shape/blinking alongside runtime font and palette changes. Its NOTICE and both upstream licenses are included in crate packages and macOS app resources. Changes do not replace the terminal parser or PTY ownership.

Interface icons are a vendored subset of [Lucide](https://lucide.dev) v1.52.0 (ISC), kept in `crates/sigmadock-ui/assets/icons/` and embedded into the binary at build time. Only the icons the UI uses are copied, and no icon package is a build dependency. The license is in `packaging/licenses/Lucide.txt` and ships in crate packages and macOS app resources. To add an icon, copy its SVG from the same Lucide release and register it in `crates/sigmadock-ui/src/icons.rs`.
