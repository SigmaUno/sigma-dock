# SigmaDock

A native, local-first workspace for supervising parallel coding agents. Rust, GPUI, a local daemon, isolated git worktrees, and a board derived from session and forge facts. Licensed under Apache-2.0.

**Status: early working prototype.** The daemon, CLI, native board and embedded terminal are implemented. Full agent TUI compatibility and distribution packaging are not yet release-qualified. See [the roadmap](docs/ROADMAP.md) for the remaining work.

## macOS app download

[macOS builds](docs/MACOS.md) provide a universal DMG: drag SigmaDock to Applications and open it. The app starts its bundled daemon; Rust is not required. Initial test downloads are not Apple-notarized. Git and agent CLIs remain external prerequisites.

## Run

Requires macOS or Linux, Rust 1.88+ (newer dependencies may require newer stable Rust), Git, and your chosen agent CLI on `PATH`. Linux native builds also require GPUI's system dependencies; see [GPUI's Linux setup](https://github.com/zed-industries/zed/blob/main/docs/src/development/linux.md).

```sh
cargo build
cargo run -p sigma-dockerd
```

In another terminal:

```sh
# Returns a project ID. The repo needs at least one commit.
cargo run -p sigma-dock-cli -- project /absolute/path/to/repo
# Use the returned project ID here.
cargo run -p sigma-dock-cli -- spawn PROJECT_ID --title "Fix the login bug" --agent claude --prompt "Fix the login bug and run the relevant tests"
cargo run -p sigma-dock-cli -- ls
cargo run -p sigma-dock-cli -- attach WORKER_ID
# Ctrl-] detaches; the agent continues in the daemon.
cargo run -p sigma-dock-ui
```

Build/install binaries with `cargo install --path crates/sigma-dock-cli`, `cargo install --path crates/sigma-dockerd`, and `cargo install --path crates/sigma-dock-ui`. Binaries are `sdk`, `sigma-dockerd`, and `sigma-dock`. Start the daemon separately before the UI. Use `--agent shell` to test without an agent subscription. Adapters also exist for `codex`, `gemini`, `opencode`, and `aider`; their current flags must be tested against your installed versions.

Each worker gets a unique `sigma/UUID` branch, a worktree outside the source checkout, a PTY, and `PORT` and `SIGMA_DOCK_WORKER_ID` environment variables. At most five sessions run concurrently by default (`sigma-dockerd --max-workers N`). Ports 4200–4999 are assigned uniquely among active workers and checked for availability; they are best-effort leases, not OS reservations.

`SIGMA_DOCK_STATE_DIR` overrides local state. Defaults: `~/Library/Application Support/SigmaDock` on macOS, `$XDG_STATE_HOME/sigma-dock` or `~/.local/state/sigma-dock` on Linux. `SIGMA_DOCK_SOCKET` overrides the socket for all binaries. Keep the daemon socket and database on a local filesystem. The state directory is mode 0700 and the socket and database are mode 0600.

## Worker operations

```sh
sdk message WORKER_ID "Run tests and fix the failures"
sdk status WORKER_ID
sdk diff WORKER_ID
sdk stop WORKER_ID
sdk resume WORKER_ID --continue
sdk archive WORKER_ID             # preserves files and branch
sdk archive WORKER_ID --cleanup   # removes a clean worktree, preserves branch
sdk prune PROJECT_ID              # prunes stale git worktree registrations
```

The native UI includes a task-creation form, harness picker, worker controls, a diff summary, and PR links. Click **Choose repository…** in the task form to open the native folder picker. Form input currently supports typing at the end, backspace, tab and clipboard paste; full text editing and IME support are pending.

Closing the UI does not stop workers. Normal daemon shutdown stops its sessions and saves their final observed state. Crashing the **daemon** loses its PTY handles: persisted active sessions become `lost`, never silently healthy. `sdk resume` starts a fresh process in the existing worktree; `--continue` asks a supported harness to resume its own conversation. Inspect and stop any surviving process before resuming after a daemon crash. Codex continuation requires `sdk resume WORKER_ID --continue` without `--prompt`; send the next instruction with `sdk message` after startup. This prototype does not recover a live PTY across daemon restarts.

Output replay is bounded to the latest 1 MiB per session and held only in memory. Reconnecting after that limit resets the terminal and replays the retained tail; terminal state may be incomplete. BEL and OSC 9/777 notifications flag `Needs you`; sixty seconds without I/O means `idle`, which remains `Working`. These are heuristics, not reliable inference of every harness's intent.

## Forge facts and feedback

Export a forge token into the daemon's environment before starting it. Configuration stores **the environment variable name**, never the token. For example:

```sh
sdk forge WORKER_ID --owner my-org --repo my-repo
sdk forge WORKER_ID --kind forgejo --api-url https://forge.example/api/v1 --owner my-org --repo my-repo --token-env FORGEJO_TOKEN
sdk refresh WORKER_ID
sdk review WORKER_ID        # preview fetched inline review comments
sdk review WORKER_ID --send # send them to this worker as bracketed paste
```

Only explicitly configured workers are polled, initially every 30 seconds with exponential backoff on failure. All requests have timeouts and redirects are disabled. Use HTTPS; HTTP is permitted only for loopback development servers. This version supports GitHub.com and Forgejo, not GitHub Enterprise. PR/check/review pagination limits fail visibly rather than claim complete facts. ETags and numeric `Retry-After`/GitHub rate-reset headers are honored. Checks, reviews and comments are paginated (up to 20 pages; overflow fails visibly). Forgejo Actions are opt-in with `sdk forge ... --actions`; older servers may not provide these endpoints. Status is advisory: approval and passing observed checks do not prove every protected-branch rule is satisfied. SigmaDock does not merge PRs automatically.

```sh
sdk ci WORKER_ID             # preview failure details
sdk ci WORKER_ID --send      # explicitly send them to the owning worker
sdk auto-ci WORKER_ID --enable # one automatic attempt per PR head commit
sdk auto-ci WORKER_ID        # disable automatic delivery
sdk conflict WORKER_ID      # preview a rebase instruction
sdk conflict WORKER_ID --send
```

GitHub feedback includes check output and annotations. Full GitHub job logs require redirects outside the configured API, so this version does not download them. Forgejo Actions feedback includes bounded job-log tails where supported. Reports are trimmed to 32 KiB and control characters are removed. Native worker controls also offer CI preview/send, review preview and a conflict plan.

Automatic feedback is off by default and waits for an idle coding worker with failed CI. The daemon records an attempt before writing to the PTY to prevent duplicate delivery after partial writes or restarts; delivery errors remain visible in worker status. Idle is a heuristic. Forge content is untrusted task data and the harness retains its own permission controls. Conflict instructions do not run git or push changes automatically.

The four board columns are computed, never dragged manually:

- **Working:** active or idle without a PR or blocker.
- **Needs you:** input notification, lost session, unsuccessful exit, CI failure, requested changes, conflict, closed PR, or forge fetch failure.
- **In review:** draft/open PR without all readiness signals.
- **Ready to merge:** open PR with approval, passing observed checks and positive mergeability; merged PRs remain visible until archived.

## Project orchestrator tools

`sigma-dock-mcp` is a local stdio MCP bridge to the same daemon. Configure your agent's MCP client to launch it (install with `cargo install --path crates/sigma-dock-mcp`). It exposes `list_workers`, `get_worker_status`, `message_worker`, and `archive_worker`. The daemon must already be running and the socket environment must match.

Spawning is disabled by default: the user creates proposed workers with `sdk spawn`. Explicitly launching `sigma-dock-mcp --allow-spawn` enables `spawn_worker` within the daemon's concurrency limit. Archive never removes worktrees through MCP. Managed Claude and Codex orchestrators are available with `sdk orchestrator PROJECT_ID --agent claude --prompt "Plan the next tasks"`. Add `--allow-spawn` to authorize worker creation. Install `sigma-dock-mcp` next to the daemon, or pass `sigma-dockerd --mcp-binary /absolute/path/sigma-dock-mcp`. Each project has at most one unarchived orchestrator; resume or archive it before creating another. Per-session MCP settings do not alter global harness configuration.

Project-scoped tools include `read_planning_notes` and `write_planning_notes`; writes require the previous revision and persist locally in SQLite. `sdk notes PROJECT_ID` reads them. Tool scope prevents accidental cross-project calls but does not sandbox the harness, which runs as your user.

## Zero-telemetry policy

SigmaDock sends no analytics, crash reports, tracking events, update checks, or remote logs. Local terminal data is never uploaded by SigmaDock. Logs stay on local stderr; metadata stays in SQLite. There is no analytics SDK, hosted backend, account, or telemetry endpoint. `deny.toml` bans known tracking crates; both CI workflows enforce the ban against the complete dependency graph. A crate-name ban is a guardrail, not proof of all transitive behavior: [the verification procedure](docs/PRIVACY.md) describes runtime checks.

**Application network egress is limited to configured GitHub or Forgejo APIs.** Core worker and terminal operations use only a local Unix socket and git subprocesses. Agent CLIs and tools launched inside a worker have their own network behavior, telemetry, and permissions. SigmaDock does not sandbox or control them and cannot promise they send no telemetry. Rust dependency downloads and audit checks are build-time network traffic, not application telemetry.

The Claude adapter sets `DISABLE_TELEMETRY=1`, `DISABLE_ERROR_REPORTING=1` and `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`; the Aider adapter sets `AIDER_ANALYTICS=false`. These are best-effort settings. Codex, Gemini and other harnesses must be configured separately; [the privacy guide](docs/PRIVACY.md) links their official guidance. Credentials already in the daemon environment are inherited by child processes; use a dedicated daemon environment if those processes should not see your forge tokens. OS keychain integration is planned.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
python3 scripts/smoke.py
python3 scripts/feedback_smoke.py
```

`cargo build` defaults to the headless binaries. `cargo build -p sigma-dock-ui` builds the native UI. Both `.forgejo/workflows/ci.yml` and `.github/workflows/ci.yml` run the same checks. Mirror setup is an administrator operation and is not performed by the repository.

[Releasing crates](docs/RELEASING.md) · [Architecture](docs/ARCHITECTURE.md) · [Roadmap](docs/ROADMAP.md) · [Privacy verification](docs/PRIVACY.md)
