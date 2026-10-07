# Canonical website

The project owner owns https://sigmadock.dev and has explicitly chosen to retain the existing **Lovable Cloud** website and hosting. Website source is managed separately from this application repository. Workspace package homepages, GitHub repository metadata and the Homebrew tap point to this domain. No DNS or hosting changes are needed for the remaining work.

## Issue #14 audit — 2026-10-07

- All twelve renamed `sigmadock-*` / `sigmadockd` packages at version 0.1.2 are published and unyanked on crates.io. The [rename release run](https://github.com/SigmaUno/sigma-dock/actions/runs/37606122509) completed successfully. See [the migration mapping](MIGRATION.md) for old package names and retained executable/state compatibility.
- HTTPS requests to the homepage, `/docs` and `/downloads` return 200. The live pages link to the correct source repository.
- None of those three served HTML documents contains a canonical link or `og:url` metadata.
- The live docs still use `sigma-dockerd`, `sigma-dock-ui`, `sigma-dock-cli` and `crates/sigma-dock-cli` in commands. These no longer match the checkout.
- Downloads still point to the older 0.1.0 test DMG; the homepage and downloads also link to `v0.1.1`. The renamed 0.1.2 universal test DMG and matching Homebrew preview are available.
- No direct support/issues link is present in those pages.

Publication and repository naming are complete. The remaining acceptance work is publishing the website edits below and checking the resulting HTML/links. This repository cannot deploy that independent Lovable project. HTTP 200 alone does not qualify canonical metadata or installation instructions.

## Apply in Lovable

Paste the following into the existing Lovable project. Preserve the site's hosting, domain, design and content outside these corrections.

> Update SigmaDock's canonical metadata and installation links for GitHub issue #14. Do not create a new site, change DNS or replace hosting.
>
> Set exactly one absolute canonical link and one `og:url` on each route: `/` → `https://sigmadock.dev/`, `/docs` → `https://sigmadock.dev/docs`, `/downloads` → `https://sigmadock.dev/downloads`. Use the framework's existing route metadata mechanism so navigation updates the URL and the served HTML contains each route's correct metadata. Do not hardcode the homepage canonical on every route. Preserve existing titles/descriptions and social image metadata.
>
> Replace old Rust package names in all code examples and installation copy: `sigma-dockerd` → `sigmadockd`; `sigma-dock-core`, `sigma-dock-store`, `sigma-dock-pty`, `sigma-dock-git`, `sigma-dock-forge`, `sigma-dock-agents`, `sigma-dock-ports`, `sigma-dock-mcp`, `sigma-dock-ui`, `sigma-dock-cli`, `sigma-dock-terminal` → the corresponding `sigmadock-*` names. Update crate directory paths too. Executables remain `sigma-dock` for the UI and `sdk` for the CLI; the daemon executable is `sigmadockd`, and the MCP executable is `sigmadock-mcp`. Keep the repository name `SigmaUno/sigma-dock`, app name `SigmaDock.app`, and `SIGMA_DOCK_*` variables unchanged.
>
> Use the source-checkout commands and Homebrew preview instructions below. Label the current DMG and Homebrew channel as development previews, ad-hoc signed and not notarized. Do not advertise a stable signed installer or Gatekeeper bypass. Git and the selected agent CLI are separate prerequisites. Update download buttons to the verified 0.1.2 preview asset, link its checksum and build notes, and show the Releases index instead of presenting v0.1.1 as the latest version. This preview was built at commit 656f3a4b3b36; do not claim it contains later main-branch features. Keep CLI/UI/daemon from the same source or release.
>
> Add footer links to Source, Releases, Documentation and Report an issue using the URLs below. Add the crate migration guide to documentation. Publish in the existing project, then verify the three live pages rather than only the Lovable preview.

### Correct source-checkout commands

From the current source checkout (after installing its native build prerequisites):

```sh
git clone https://github.com/SigmaUno/sigma-dock.git
cd sigma-dock
cargo build --locked
cargo run -p sigmadockd
```

In another terminal in the same checkout:

```sh
cargo run -p sigmadock-cli -- project /absolute/path/to/repository
cargo run -p sigmadock-cli -- spawn PROJECT_ID --title "Fix the login bug" \
  --agent claude --prompt "Fix the login bug and run the relevant tests"
cargo run -p sigmadock-ui
```

The target repository needs at least one commit. Use the project ID returned by `sdk project`; `--agent shell` is available for a local shell worker. To install the CLI from the checkout, use `cargo install --path crates/sigmadock-cli`. For published crate installation, use the matching package names and versions in [MIGRATION.md](MIGRATION.md). Do not mix the current API-2 source clients with an already-running older release daemon; finish workers and restart the matching daemon.

### Homebrew preview

```sh
brew tap SigmaUno/tap
brew install --cask sigma-dock-preview
sdk --help
```

Stable `sigma-dock` distribution remains pending signing/notarization qualification (#7/#12). Link [the macOS guide](https://github.com/SigmaUno/sigma-dock/blob/main/docs/MACOS.md) for prerequisites and upgrade behavior.

### Verified download and navigation targets

| Link | URL |
| --- | --- |
| Source | https://github.com/SigmaUno/sigma-dock |
| Releases | https://github.com/SigmaUno/sigma-dock/releases |
| Documentation | https://sigmadock.dev/docs |
| Report an issue | https://github.com/SigmaUno/sigma-dock/issues |
| Migration guide | https://github.com/SigmaUno/sigma-dock/blob/main/docs/MIGRATION.md |
| Preview build notes | https://github.com/SigmaUno/sigma-dock/releases/tag/macos-656f3a4b3b36 |
| Preview DMG | https://github.com/SigmaUno/sigma-dock/releases/download/macos-656f3a4b3b36/SigmaDock-0.1.2-656f3a4b3b36-universal-test.dmg |
| Preview SHA-256 | https://github.com/SigmaUno/sigma-dock/releases/download/macos-656f3a4b3b36/SigmaDock-0.1.2-656f3a4b3b36-universal-test.dmg.sha256 |

The preview DMG SHA-256 reported by GitHub and the matching Homebrew cask is `20d974d83a625d51d1e2a272c35b8e1aca5f6364eb98c6698cf5b4a8dcc0f255`. Update preview links only after the replacement asset and checksum are verified; do not use a version-specific crate tag as a substitute for an available DMG.

## Finish verification

After the Lovable deployment, check the served HTML for all three routes (and metadata after client-side navigation), corrected command examples, support link, and download/checksum destinations. Confirm HTTPS redirects for any alternate site hostnames controlled by the website owner. No `sigmauno.com` redirect is claimed: that is a separate website, and its deployment settings are unavailable here. Redirect only project URLs the owner controls; changing the homepage does not move the GitHub repository or local app data.

Keep #14 open until these live-site checks pass. The publication result no longer blocks closure.
