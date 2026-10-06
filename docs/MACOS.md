# macOS downloads

The **Build macOS installer** workflow builds on native Apple Silicon and Intel runners, combines all four executables into universal binaries and produces one `SigmaDock.app` inside a DMG. Download the DMG from the GitHub prerelease, open it, drag the app to Applications, then launch it. Rust is not required. Git and the chosen agent CLI must already be installed.

Opening the installed app starts the bundled daemon when no compatible daemon is reachable. Closing the app leaves its workers running. Reopening reconnects. Logout/reboot and daemon crashes still lose live sessions; this does not install a login item or launch service. `--no-daemon` keeps explicit daemon management available. The bundled CLI is `/Applications/SigmaDock.app/Contents/MacOS/sdk`.

Finder does not inherit terminal shell configuration. The bundled daemon receives the inherited PATH plus the app's helpers, `~/.local/bin`, `~/.cargo/bin`, `/opt/homebrew/bin` and `/usr/local/bin`. Custom shell setup and forge tokens are not loaded automatically; start the bundled daemon from a terminal with the intended environment if needed. Logs stay in the local state directory (`daemon.log`). No update checking is added.

## Build and download

Run **Build macOS installer** manually on the desired branch. It creates a GitHub prerelease named `macos-COMMIT` and uploads the DMG and SHA-256 checksum. These are source snapshots, separate from immutable published crate versions. Pushing a version tag also builds a DMG and attaches it to that version's release. Existing version tags must stay on the matching source commit.

The build targets macOS 13 or later; rendered terminal behavior and older macOS versions still need manual qualification. Build jobs test both native architectures. Packaging rejects non-system dynamic-library dependencies and verifies signatures and disk-image integrity.

## Developer ID signing and notarization

Without Apple credentials the job creates an ad-hoc signed **test** DMG. It is not notarized and macOS Gatekeeper may block a downloaded copy. A normal public download needs Developer ID signing and Apple notarization: see [Apple's distribution guide](https://developer.apple.com/macos/distribution/).

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

For a local bundle test using built binaries:

```sh
python3 scripts/package_macos.py --bin-dir target/debug --output /tmp/sigmadock-bundle --version 0.1.0 --build-id COMMIT --arch arm64 --app-only
python3 scripts/macos_bundle_smoke.py /tmp/sigmadock-bundle/SigmaDock.app
```

Use your actual commit ID and native architecture. Omit `--app-only` to make a DMG. Packaging requires macOS command-line tools; macOS GitHub runners provide the full toolchain.
