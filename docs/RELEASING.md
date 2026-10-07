# Releasing crates

All twelve crates share the workspace version. They are functional prototypes; terminal compatibility and packaging remain unfinished. Publishing creates immutable crate versions, not a production-readiness claim.

The GitHub **Release crates** workflow runs validation and a full workspace publication dry run, then publishes dependencies before their consumers with Cargo's workspace publisher. It reads the repository secret `crates_token` only in the upload step. The token needs publishing permission for these package names; initial publishing requires permission to create packages. It is never committed or printed.

For the initial release, run Release crates on `main` with version `0.1.0`. Subsequent releases:

1. Run **Prepare crate update** with a new version, or run `python3 scripts/release.py bump 0.1.1` locally and commit the result.
2. Review and merge the generated release branch, then wait for CI.
3. Run Release crates with the committed version, or push a `v0.1.1` tag at that commit.

The update workflow pushes a release branch and provides a compare link. A maintainer opens/merges the change; this works with the organization policy that blocks Actions from creating pull requests. Version inputs are passed through environment variables and validated before they enter branch names or Cargo commands. The release job serializes uploads, validates existing tags and creates a GitHub prerelease after publication. The helper honors crates.io new-package rate-limit retry times automatically (with a retry budget based on workspace size). For new names, crates.io currently permits a burst of five followed by one new crate every ten minutes; the eleven-name initial release can take about an hour. Other failed publications may be retried: the helper skips package versions already present on crates.io and publishes the remaining packages. Skipped versions are immutable and cannot receive changed code; release a new version for changes.

Crates: `sigma-dock-core`, `sigma-dock-store`, `sigma-dock-pty`, `sigma-dock-git`, `sigma-dock-forge`, `sigma-dock-agents`, `sigma-dock-ports`, `sigma-dock-mcp`, `sigma-dockerd`, `sigma-dock-ui`, `sigma-dock-cli`.

No domain registration is required; the homepage is sigmauno.com. Linux packages, signed binaries and Homebrew distribution are separate future work.

Both distribution workflows create and verify the exact release tag before long checks. This keeps the tested commit referenced if `main` advances and allows publication using `--verify-tag`, avoiding GitHub's [workflow-scoped release restriction](https://github.blog/changelog/2023-11-02-github-actions-enforcing-workflow-scope-when-creating-a-release/) without a new publishing token. An existing tag is never moved. A failed build may leave its reserved tag; rerun that same commit after addressing the failure rather than moving a published version tag.
