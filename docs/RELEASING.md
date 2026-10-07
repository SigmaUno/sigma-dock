# Releasing crates

All twelve crates share the workspace version. They are functional prototypes; terminal compatibility and packaging remain unfinished. Publishing creates immutable crate versions, not a production-readiness claim.

The GitHub **Release crates** workflow runs validation and a full workspace publication dry run, then publishes dependencies before their consumers with Cargo's workspace publisher. It reads the repository secret `crates_token` only in the upload step. The token needs publishing permission for these package names; initial publishing requires permission to create packages. It is never committed or printed.

The renamed crate family starts at `0.1.2`. See [the migration guide](MIGRATION.md). Subsequent releases:

1. Run **Prepare crate update** with a new version, or run `python3 scripts/release.py bump 0.1.3` locally and commit the result.
2. Open a pull request for the release branch, run validation locally, then review and merge it.
3. Push a `v0.1.3` tag at the reviewed commit. CI, crate publication and the production macOS installer run on tags only; branch pushes and pull requests do not start builds. The macOS installer requires the Apple credentials described in [the macOS guide](MACOS.md).

The update workflow pushes a release branch and provides a compare link. A maintainer opens/merges the change; this works with the organization policy that blocks Actions from creating pull requests. Version inputs are passed through environment variables and validated before they enter branch names or Cargo commands. The release job serializes uploads, validates existing tags and creates a GitHub prerelease after publication. The helper honors crates.io new-package rate-limit retry times automatically (with a retry budget based on workspace size). For new names, crates.io currently permits a burst of five followed by one new crate every ten minutes; the twelve-name initial release can take roughly seventy minutes after validation. Other failed publications may be retried: the helper skips package versions already present on crates.io and publishes the remaining packages. Skipped versions are immutable and cannot receive changed code; release a new version for changes.

Crates: `sigmadock-core`, `sigmadock-store`, `sigmadock-pty`, `sigmadock-git`, `sigmadock-forge`, `sigmadock-agents`, `sigmadock-ports`, `sigmadock-mcp`, `sigmadockd`, `sigmadock-ui`, `sigmadock-cli`, `sigmadock-terminal`.

The canonical homepage is https://sigmadock.dev. The [Homebrew preview tap](https://github.com/SigmaUno/homebrew-tap) and universal macOS test installers are available; production signing/notarization and stable Homebrew distribution remain pending qualification. Linux packages are future work.

Both distribution workflows verify the pushed release tag before long checks. This keeps the tested commit referenced if `main` advances and allows publication using `--verify-tag`, avoiding GitHub's [workflow-scoped release restriction](https://github.blog/changelog/2023-11-02-github-actions-enforcing-workflow-scope-when-creating-a-release/) without a new publishing token. An existing tag is never moved. Rerun failed jobs at the same tag for transient or credential failures. Code fixes require a new reviewed commit and version tag; never move an existing release tag.

## Unreleased: advisory Checks pane

Each berth exposes a Checks pane, also available with ⌘⇧K, combining Git state, PR metadata, CI excerpts, unresolved inline reviews and conflict instructions. Feedback actions preview exact text before using the existing delivery paths. The shared readiness summary preserves blockers and treats unknown, errored, incomplete or mismatched data as unknown. No new background forge polling or automatic merge behavior is introduced.

The additive `worker_checks` RPC returns independent detail errors. Facts include the observed PR target branch; CI previews include a completeness flag. Older serialized facts remain readable. GitHub review feedback now uses review-thread resolution metadata from GraphQL; a token with repository read access is required. Forgejo uses review comment resolver metadata when provided, and missing metadata is reported as unknown.

## Unreleased: fresh remote worker bases

Projects now store their default branch on `origin`; new and queued workers fetch that branch at launch rather than using a local branch or checkout `HEAD`. Edit the preference with `sdk project-base PROJECT_ID BRANCH`. An explicit spawn `--base` (or task-form/MCP base override) keeps its existing local-ref behavior and bypasses fetching. Repositories without a known remote default must configure a project branch or supply an explicit base.

Fetches time out after eight seconds. A cached remote commit can be used on failure, with a persisted berth warning; missing cached commits fail visibly. Existing project and worker JSON records load with absent preferences/warnings, and pre-upgrade queued records retain their recorded base as an explicit override.

## Unreleased: worker status terminology

The Rust API exposes `sigmadock_core::Status` and `status(&Facts)` for deriving a worker's status. Downstream Rust callers must migrate to these names. The four states (Working, Needs you, In review, Ready to merge), their precedence and their serialized snake_case values are unchanged; they describe workers displayed in berths.

JSON-RPC `get_worker_status` returns `status` as the canonical derived-state field. The legacy `column` field is a deprecated alias with the same value, retained for the first release containing this change and removed in the following release. RPC and MCP consumers should migrate to `status` now; the MCP tool forwards both fields during the transition. The separate API-2 compatibility requirements below still apply.

## Unreleased: daemon API 2 and berths

Issue #17 adds stable worker berth slots, global/per-project capacity and a persistent FIFO task queue. SQLite migrates to schema 4, retaining old workers with no assigned berth until their next session. The daemon API is now 2; UI/CLI/MCP clients refuse API-1 daemons. Finish running sessions and restart the matching daemon before using the new clients. Existing API-2 responses use `status`, with `column` retained as a deprecated alias for one release. Queue prompts remain in local state until cancellation or successful launch. See [architecture](ARCHITECTURE.md) for task failure/retry and project-removal behavior. Native queue presentation remains tracked in #21.

## Unreleased: daemon event subscriptions

Issue #19 adds an API-2 `subscribe` stream without removing polling RPCs. Native workspace refreshes and terminal output follow events, with bounded subscribers, reconnect/resync and polling fallback. Session generations prevent replay from mixing resumed sessions. The event regression and idle-workload measurement run in the existing tag-triggered CI and crate release workflows.

## Unreleased: app and installer notarization

Production macOS packaging notarizes and staples the app before building the DMG, then separately notarizes and staples the final image. Verification requires tickets on both the image and the app copied from it, exact Developer ID authority and hardened runtime. Separate Apple diagnostics are retained for each submission. Apple credentials and clean-Mac online/offline qualification remain required; see [the issue #7 procedure](MACOS.md#production-qualification-for-issue-7).

## Unreleased: deterministic terminal font

The default monospace preference uses bundled JetBrains Mono instead of relying on a platform generic-family fallback. Font metrics and glyph painting use the same family and four styles. Existing custom font preferences are retained.
