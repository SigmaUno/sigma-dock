# Release detection and manual upgrades

Settings includes **Check for updates**, an **Automatic daily checks** opt-in (off by default), and a stable/preview channel selector. A compatible newer release produces a non-blocking notice showing the installed and available versions, plain-text release notes and a release-page download link. Dismissing a version persists across restarts and suppresses later automatic notices for that version. A manual check can show it again. A successful check with no newer compatible release reports “Up to date”. No release match is a normal result, including an empty release list.

## Version and installer rules

The app embeds the Cargo version, 40-character source commit (or `unknown`), build channel and target architecture. CI builds from `v*` tags use stable/preview according to the semantic version; other builds are snapshots. `SIGMA_DOCK_BUILD_CHANNEL` can explicitly identify a stable, preview or snapshot build. Published crate sources use Cargo’s VCS metadata when available. The architecture is compiled separately into each slice of the universal macOS app.

Snapshot builds compare their embedded Cargo version against tagged releases; two commits with the same version are never ordered. Tags such as `macos-b101a03bb133` are ignored. A snapshot based on 0.1.0 can notify for a compatible v0.1.1, but not another 0.1.0 snapshot. Build metadata does not change version precedence. Publish a higher semantic version for update notification eligibility.

The checker reads [GitHub’s release API](https://docs.github.com/en/rest/releases/releases?apiVersion=latest) for `SigmaUno/sigma-dock`. Only valid `v`-prefixed semantic versions newer than the installed version qualify. Stable excludes both releases flagged prerelease and semantic prereleases; preview includes both. Drafts, unrelated tags, incomplete uploads and releases without a matching installer are skipped. Currently installer support is macOS DMG: `SigmaDock-…-universal-….dmg` or a matching `arm64`/`x86_64` asset. Checksum files do not qualify. Universal installers are preferred. Linux installation packaging must be added before Linux releases can produce an installer notice.

## Network behavior and scheduling

Checks run outside the UI thread and independently of worker commands. No API token, machine identifier, project information or terminal output is included. The only update endpoint is `https://api.github.com/repos/SigmaUno/sigma-dock/releases`. Requests specify a fixed product user agent and the GitHub API version. Redirects are refused; pagination always returns to the fixed endpoint. Each request has a five-second connection timeout and ten-second total timeout. Responses are bounded at 2 MB; pagination is bounded at ten pages and an incomplete traversal reports an error instead of claiming up to date.

In-memory page caches retain ETags and reuse releases after 304 responses. Successful checks schedule the next background check 24 hours later. Network errors use exponential backoff, and GitHub rate limits respect Retry-After / rate-limit reset headers (bounded to one day). Retry deadlines persist locally for automatic checks; the in-memory checker also prevents repeated manual retries during backoff. Cached release data/ETags are not persisted. Offline and HTTP errors appear in Settings and do not change daemon or worker state. The regular local worker poll only starts a background check when the opt-in is enabled, the deadline is due and no check is already running.

## Installation now and future investigation

The initial feature opens the GitHub release page in a browser. The user downloads the appropriate installer and follows [macOS installation instructions](MACOS.md). It never downloads an executable into the app, replaces an installation, stops the daemon or sends an agent a command. Existing published snapshots are test builds and may be ad-hoc signed; the notice makes no notarization claim.

Before implementing automatic installation, decide how to authenticate release manifests and checksums: a hash hosted beside an installer protects against accidental corruption, not compromise of the release account. Verify the expected Developer ID and notarization before installation, support rollback and atomic replacement, and handle read-only/managed app locations. A new UI must negotiate daemon API compatibility and reconnect to the existing daemon. Updating a running daemon requires an explicit migration/restart strategy because its process owns live PTYs; do not kill it to replace the UI. Test interrupted downloads, corrupted installers, old/new daemon combinations and live agents before offering any installer automation.

## Validation

Unit tests cover semantic precedence, stable/preview filtering, snapshot tags, missing/wrong-architecture/incomplete assets, dismissal and the default-off scheduler. Loopback HTTP fixtures cover 304 caching, request headers, offline failures, rate limits/backoff, redirects and invalid JSON. The privacy guide describes a separate runtime egress audit; automated tests do not claim to have performed a packet-capture audit.
