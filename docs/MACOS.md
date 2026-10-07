# macOS downloads

The **Build macOS installer** workflow builds on native Apple Silicon and Intel runners, combines all four executables into universal binaries and produces one `SigmaDock.app` inside a DMG. Download the DMG from the GitHub prerelease, open it, drag the app to Applications, then launch it. Rust is not required. Git and the chosen agent CLI must already be installed.

Opening the installed app starts the bundled daemon when no compatible daemon is reachable. Closing the app leaves its workers running. Reopening reconnects. Logout/reboot and daemon crashes still lose live sessions; this does not install a login item or launch service. `--no-daemon` keeps explicit daemon management available. The bundled CLI is `/Applications/SigmaDock.app/Contents/MacOS/sdk`.

Finder does not inherit terminal shell configuration. The bundled daemon receives the inherited PATH plus the app's helpers, `~/.local/bin`, `~/.cargo/bin`, `/opt/homebrew/bin` and `/usr/local/bin`. Custom shell setup and forge tokens are not loaded automatically; start the bundled daemon from a terminal with the intended environment if needed. Logs stay in the local state directory (`daemon.log`). Manual update checks and opt-in daily checks are available in Settings; see [the update design](UPDATES.md).

## Homebrew

The public tap is [SigmaUno/homebrew-tap](https://github.com/SigmaUno/homebrew-tap). A separate preview channel is available while production Apple signing and notarization remain pending:

```sh
brew tap SigmaUno/tap
brew install --cask sigma-dock-preview
sdk --help
brew upgrade --cask sigma-dock-preview
brew uninstall --cask sigma-dock-preview
```

Preview downloads are ad-hoc signed and Gatekeeper may block them; the tap does not disable Gatekeeper. The stable `brew install --cask sigma-dock` command becomes available after production qualification. Git and the selected agent CLI are separate prerequisites. The cask links the bundled `sdk`; resolve any existing executable with that name before installation. Finish workers before replacing or restarting the daemon during upgrades. Normal upgrade/uninstall preserves local settings, session history and worktrees; the tap has no destructive cleanup hook.

Maintainers update a cask after upstream publication with the tap's `scripts/update_cask.py`. Stable promotion refuses prereleases and test assets, verifies the actual DMG checksum, signing, notarization and universal binaries, and requires a reviewed tap commit. Preview and stable casks conflict rather than overwrite one another silently. See the tap README for release and architecture qualification steps.

## Build and download

After reviewing and merging the version-update pull request, push a matching version tag such as `v0.1.3` at that commit. **Build macOS installer** runs only on `v*` tags, requires production signing and notarization, and uploads the DMG and SHA-256 checksum to that version's release. Branch pushes, pull requests and manual dispatch do not start builds. Existing version tags must stay on the matching source commit. For preview builds without Apple credentials, use the local installer instructions below; previously published preview downloads remain available.

The build targets macOS 13 or later; rendered terminal behavior and older macOS versions still need manual qualification. Build jobs test both native architectures. Packaging rejects non-system dynamic-library dependencies and verifies signatures and disk-image integrity.

## Developer ID signing and notarization

Tagged builds fail before compilation if any required Apple credentials are missing. Local non-production builds create an ad-hoc signed **test** DMG; it is not notarized and macOS Gatekeeper may block a downloaded copy. A normal public download needs Developer ID signing and Apple notarization: see [Apple's distribution guide](https://developer.apple.com/macos/distribution/).

Add repository Actions secrets:

| Secret | Value |
|---|---|
| `APPLE_CERTIFICATE_P12` | Base64-encoded Developer ID Application certificate and its private key exported as `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | Password used for that export |
| `APPLE_SIGNING_IDENTITY` | Full identity, e.g. `Developer ID Application: Company Name (TEAMID)` |
| `APPLE_API_KEY_P8` | App Store Connect notarization API private key contents |
| `APPLE_API_KEY_ID` | API key ID |
| `APPLE_API_ISSUER` | API issuer ID |

The packaging job imports the certificate into a temporary keychain, signs every executable and the app with hardened runtime, submits a temporary app ZIP using `notarytool`, and staples the accepted ticket to the app before creating the DMG. It then signs and notarizes the final DMG and staples its ticket too. The copied app and image must both pass ticket validation; each submission retains separate diagnostics. Credentials are cleaned up even on failure. Partially configured notarization fails visibly. `crates_token` is unrelated to Apple signing and is not used by the macOS build.

## Build a local installer

On macOS, install Rust, Python 3 and Xcode command-line tools (`xcode-select --install` if needed), then run from the repository root:

```sh
MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --locked --release \
  -p sigmadock-ui -p sigmadockd -p sigmadock-cli -p sigmadock-mcp

version=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')
python3 scripts/package_macos.py --bin-dir target/release \
  --version "$version" --build-id "$(git rev-parse --short=12 HEAD)" \
  --arch "$(uname -m)"
```

The output is `dist/SigmaDock.app`, a DMG and its SHA-256 checksum. If `dist/SigmaDock.app` already exists, pass `--output /path/to/a/fresh-directory` to package another build. Open the DMG, drag the app into Applications, then launch it. This creates a native Apple Silicon or Intel test installer for the machine running the build. Apple signing credentials are not required; the app is ad-hoc signed and not notarized. Git and your chosen agent CLI still need to be installed separately. The GitHub workflow builds the universal installer containing both architectures.

Add `--app-only` to the packaging command to skip DMG creation. To smoke-test the bundled daemon, CLI, worker and PTY without launching the GUI:

```sh
python3 scripts/macos_bundle_smoke.py dist/SigmaDock.app
```

Production packaging requires an explicit Apple `Accepted` result, retains JSON notarization diagnostics as a CI artifact, validates the stapled app and DMG, then mounts the final image and verifies the copied app’s Developer ID identity, hardened runtime, Gatekeeper assessment, and bundled daemon/PTY smoke test. Checksums are generated after these checks. Production publication has no ad-hoc fallback. These checks do not replace browser-download qualification on clean Apple Silicon and Intel Macs.

The certificate owner should document the Apple team ID, certificate expiry and renewal schedule outside this repository, rotate the exported certificate/password and notarization key together, and limit who can change production release workflows and secrets. Credential values and private keys must never be committed. Current production qualification is pending credential setup and clean-Mac testing.

## Procedural-macro build errors on newer macOS

Rust 1.96 can emit stripped procedural-macro libraries whose LINKEDIT layout is rejected by macOS 27. The visible error can be `E0463: can't find crate for zerofrom_derive` even though the library was built. The workspace keeps debug information and disables stripping for build-time dependencies in both development and release profiles, following the [upstream Rust issue](https://github.com/rust-lang/rust/issues/157750). Application release optimization is unchanged. Pull the current repository and rebuild; Cargo selects new artifacts for the changed profile, so clearing the registry or the entire target directory is unnecessary.

## Production qualification for issue #7

The release safeguards are implemented; successful Apple notarization and clean-Mac acceptance are still pending. Do not treat mocked distribution tests or ad-hoc local installers as qualification.

1. The Apple team owner must enroll in the Apple Developer Program and create a **Developer ID Application** certificate with its private key. Export the certificate and key as a password-protected `.p12` from Keychain Access. Record the team, responsible owner, expiry and renewal date in your private operations records.
2. Create an App Store Connect team API key permitted to submit notarization requests. Keep the downloaded `.p8` private. Set the six secrets in the table above under GitHub repository **Settings → Secrets and variables → Actions**. `APPLE_CERTIFICATE_P12` is the base64 representation of the export; `APPLE_API_KEY_P8` is the PEM file contents. Never paste either into issues or PRs.
3. Merge the reviewed changes and prepare a new version through its own PR. Push its matching, immutable `v*` tag. There are no branch, PR or manual build triggers. If only credentials need correcting, rerun the failed jobs at that same tag; source fixes require a new version and tag.
4. Inspect the **Build macOS installer** run. Both `notarization-app-result.json` and `notarization-dmg-result.json` must report `Accepted`; download the diagnostics artifact. Ticket, signature, hardened-runtime, Gatekeeper and bundled PTY checks must all pass before publication.
5. Download the final DMG through a browser on clean Apple Silicon and Intel Macs with default Gatekeeper settings. Compare its SHA-256 with the published checksum, install through Finder, launch, select a repository, start a shell worker and an installed agent, type and resize the terminal, close/reopen the app and confirm worker survival. Do not remove quarantine or disable Gatekeeper to pass this test.
6. Repeat installation and first launch offline on a fresh test machine or restored snapshot, with the browser-downloaded DMG already present. Avoid warming Gatekeeper's online cache first. Both the DMG and copied app carry stapled tickets; offline first launch still requires real qualification.
7. Record the source commit, tag, workflow URL, DMG filename/checksum, Apple submission IDs, macOS version and architecture, online/offline results and tester in #7. Include any runtime errors. Only after both architectures pass should #7 close and stable Homebrew promotion (#12) proceed.

No extra hardened-runtime entitlements are currently granted. The UI, daemon, CLI and MCP helper are signed with the same Developer ID; the bundled smoke exercises daemon bootstrap, PTYs and external shell execution. Interactive agent and terminal behavior remain part of the clean-Mac checks. Add an entitlement only for a demonstrated failure, with a focused review.

Apple references: [customizing notarization](https://developer.apple.com/documentation/Security/customizing-the-notarization-workflow) and [packaging Mac software](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution).
