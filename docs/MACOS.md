# macOS downloads

The **Build macOS installer** workflow builds on native Apple Silicon and Intel runners, combines all four executables into universal binaries and produces one `SigmaDock.app` inside a DMG. Download the DMG from the GitHub prerelease, open it, drag the app to Applications, then launch it. Rust is not required. Git and the chosen agent CLI must already be installed.

Opening the installed app starts the bundled daemon when no compatible daemon is reachable. Closing the app leaves its workers running. Reopening reconnects. Logout/reboot and daemon crashes still lose live sessions; this does not install a login item or launch service. `--no-daemon` keeps explicit daemon management available. The bundled CLI is `/Applications/SigmaDock.app/Contents/MacOS/sdk`.

Finder does not inherit terminal shell configuration. The bundled daemon receives the inherited PATH plus the app's helpers, `~/.local/bin`, `~/.cargo/bin`, `/opt/homebrew/bin` and `/usr/local/bin`. Custom shell setup and forge tokens are not loaded automatically; start the bundled daemon from a terminal with the intended environment if needed. Logs stay in the local state directory (`daemon.log`). Manual update checks and opt-in daily checks are available in Settings; see [the update design](UPDATES.md).

## Build and download

Run **Build macOS installer** manually on the desired branch. It creates a GitHub prerelease named `macos-COMMIT` and uploads the DMG and SHA-256 checksum. These are source snapshots, separate from immutable published crate versions. Pushing a version tag requires production signing and notarization, builds a DMG, and attaches it to that version's release. Manual dispatch remains an explicitly labeled test-build path unless **production** is selected. Existing version tags must stay on the matching source commit.

The build targets macOS 13 or later; rendered terminal behavior and older macOS versions still need manual qualification. Build jobs test both native architectures. Packaging rejects non-system dynamic-library dependencies and verifies signatures and disk-image integrity.

## Developer ID signing and notarization

Without Apple credentials a non-production manual build creates an ad-hoc signed **test** DMG. Version tags and manual **production** builds fail before compilation if any required credentials are missing. It is not notarized and macOS Gatekeeper may block a downloaded copy. A normal public download needs Developer ID signing and Apple notarization: see [Apple's distribution guide](https://developer.apple.com/macos/distribution/).

Add repository Actions secrets:

| Secret | Value |
|---|---|
| `APPLE_CERTIFICATE_P12` | Base64-encoded Developer ID Application certificate and its private key exported as `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | Password used for that export |
| `APPLE_SIGNING_IDENTITY` | Full identity, e.g. `Developer ID Application: Company Name (TEAMID)` |
| `APPLE_API_KEY_P8` | App Store Connect notarization API private key contents |
| `APPLE_API_KEY_ID` | API key ID |
| `APPLE_API_ISSUER` | API issuer ID |

The packaging job imports the certificate into a temporary keychain, signs every executable and the app with hardened runtime, signs the DMG, submits it using `notarytool`, and staples the accepted ticket. Credentials are cleaned up even on failure. Partially configured notarization fails visibly. `crates_token` is unrelated to Apple signing and is not used by the macOS build.

## Build a local installer

On macOS, install Rust, Python 3 and Xcode command-line tools (`xcode-select --install` if needed), then run from the repository root:

```sh
MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --locked --release \
  -p sigma-dock-ui -p sigma-dockerd -p sigma-dock-cli -p sigma-dock-mcp

version=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')
python3 scripts/package_macos.py --bin-dir target/release \
  --version "$version" --build-id "$(git rev-parse --short=12 HEAD)" \
  --arch "$(uname -m)"
```

The output is `dist/SigmaDock.app`, a DMG and its SHA-256 checksum. Open the DMG, drag the app into Applications, then launch it. This creates a native Apple Silicon or Intel test installer for the machine running the build. Apple signing credentials are not required; the app is ad-hoc signed and not notarized. Git and your chosen agent CLI still need to be installed separately. The GitHub workflow builds the universal installer containing both architectures.

Add `--app-only` to the packaging command to skip DMG creation. To smoke-test the bundled daemon, CLI, worker and PTY without launching the GUI:

```sh
python3 scripts/macos_bundle_smoke.py dist/SigmaDock.app
```

Production packaging requires an explicit Apple `Accepted` result, retains JSON notarization diagnostics as a CI artifact, validates the stapled DMG, then mounts the final image and verifies the copied app’s Developer ID identity, hardened runtime, Gatekeeper assessment, and bundled daemon/PTY smoke test. Checksums are generated after these checks. Production publication has no ad-hoc fallback. These checks do not replace browser-download qualification on clean Apple Silicon and Intel Macs.

The certificate owner should document the Apple team ID, certificate expiry and renewal schedule outside this repository, rotate the exported certificate/password and notarization key together, and limit who can change production release workflows and secrets. Credential values and private keys must never be committed. Current production qualification is pending credential setup and clean-Mac testing.
