# Releasing crates

All twelve crates share the workspace version. They are functional prototypes; terminal compatibility and packaging remain unfinished. Publishing creates immutable crate versions, not a production-readiness claim.

The GitHub **Release crates** workflow runs validation and a full workspace publication dry run, then publishes dependencies before their consumers with Cargo's workspace publisher. It reads the repository secret `crates_token` only in the upload step. The token needs publishing permission for these package names; initial publishing requires permission to create packages. It is never committed or printed.

The renamed crate family starts at `0.1.2`. See [the migration guide](MIGRATION.md). Subsequent releases:

1. Run **Prepare crate update** with a new version, or run `python3 scripts/release.py bump 0.1.3` locally and commit the result.
2. Review and merge the generated release branch, then wait for CI.
3. Run Release crates with the committed version, or push a `v0.1.3` tag at that commit.

The update workflow pushes a release branch and provides a compare link. A maintainer opens/merges the change; this works with the organization policy that blocks Actions from creating pull requests. Version inputs are passed through environment variables and validated before they enter branch names or Cargo commands. The release job serializes uploads, validates existing tags and creates a GitHub prerelease after publication. The helper honors crates.io new-package rate-limit retry times automatically (with a retry budget based on workspace size). For new names, crates.io currently permits a burst of five followed by one new crate every ten minutes; the twelve-name initial release can take roughly seventy minutes after validation. Other failed publications may be retried: the helper skips package versions already present on crates.io and publishes the remaining packages. Skipped versions are immutable and cannot receive changed code; release a new version for changes.

Crates: `sigmadock-core`, `sigmadock-store`, `sigmadock-pty`, `sigmadock-git`, `sigmadock-forge`, `sigmadock-agents`, `sigmadock-ports`, `sigmadock-mcp`, `sigmadockd`, `sigmadock-ui`, `sigmadock-cli`, `sigmadock-terminal`.

The canonical homepage is https://sigmadock.dev. The [Homebrew preview tap](https://github.com/SigmaUno/homebrew-tap) and universal macOS test installers are available; production signing/notarization and stable Homebrew distribution remain pending qualification. Linux packages are future work.

Both distribution workflows create and verify the exact release tag before long checks. This keeps the tested commit referenced if `main` advances and allows publication using `--verify-tag`, avoiding GitHub's [workflow-scoped release restriction](https://github.blog/changelog/2023-11-02-github-actions-enforcing-workflow-scope-when-creating-a-release/) without a new publishing token. An existing tag is never moved. A failed build may leave its reserved tag; rerun that same commit after addressing the failure rather than moving a published version tag.

## Next release: worker status terminology

The Rust API now exposes `sigmadock_core::Status` and `status(&Facts)` in place of the previous type and derivation function. The four derived states and their serialized snake_case values are unchanged.

JSON-RPC `get_worker_status` now returns `status` alongside `worker` and `pid`. The legacy `column` field remains as a deprecated alias with the same value for one release; clients should migrate to `status` before the following release removes the alias. The MCP `get_worker_status` tool forwards both fields unchanged. `API_VERSION` stays at 1 because this response addition preserves existing clients.
