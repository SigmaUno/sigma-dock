# Releasing crates

All eleven crates share the workspace version. They are functional prototypes; terminal compatibility and packaging remain unfinished. Publishing creates immutable crate versions, not a production-readiness claim.

The GitHub **Release crates** workflow runs validation and a full workspace publication dry run, then publishes dependencies before their consumers with Cargo's workspace publisher. It reads the repository secret `crates_token` only in the upload step. The token needs publishing permission for these package names; initial publishing requires permission to create packages. It is never committed or printed.

For the initial release, run Release crates on `main` with version `0.1.0`. Subsequent releases:

1. Run **Prepare crate update** with a new version, or run `python3 scripts/release.py bump 0.1.1` locally and commit the result.
2. Merge the version update and wait for CI.
3. Run Release crates with the committed version, or push a `v0.1.1` tag at that commit.

GitHub must permit Actions to create pull requests for the update workflow. Version inputs are passed through environment variables and validated before they enter branch names or Cargo commands. The release job serializes uploads, validates existing tags and creates a GitHub prerelease after publication. A failed publication may be retried: the helper skips package versions already present on crates.io and publishes the remaining packages. Skipped versions are immutable and cannot receive changed code; release a new version for changes.

Crates: `sigma-dock-core`, `sigma-dock-store`, `sigma-dock-pty`, `sigma-dock-git`, `sigma-dock-forge`, `sigma-dock-agents`, `sigma-dock-ports`, `sigma-dock-mcp`, `sigma-dockerd`, `sigma-dock-ui`, `sigma-dock-cli`.

No domain registration is required; the homepage is sigmauno.com. Linux packages, signed binaries and Homebrew distribution are separate future work.
