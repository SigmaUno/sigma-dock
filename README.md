# SigmaDock

A native, local-first workspace for supervising parallel coding agents. Rust, GPUI, a local daemon, isolated git worktrees, and a berths view derived from session and forge facts. Licensed under Apache-2.0.

**Status: early working prototype.** The daemon, CLI, native berths view and embedded terminal are implemented. Full agent TUI compatibility and distribution packaging are not yet release-qualified. See [the roadmap](docs/ROADMAP.md) for the remaining work.

Project website: [sigmadock.dev](https://sigmadock.dev). See the [crate and executable migration guide](docs/MIGRATION.md) for version 0.1.2.

## macOS app download

[macOS builds](docs/MACOS.md) provide a universal DMG: drag SigmaDock to Applications and open it. The app starts its bundled daemon; Rust is not required. Initial test downloads are not Apple-notarized. Git and agent CLIs remain external prerequisites.

For Homebrew previews: `brew tap SigmaUno/tap`, then `brew install --cask sigma-dock-preview`. The stable `sigma-dock` cask awaits Apple signing and notarization. See [macOS installation and upgrade details](docs/MACOS.md#homebrew).

## Run

Requires macOS or Linux, Rust 1.88+ (newer dependencies may require newer stable Rust), Git, and your chosen agent CLI on `PATH`. Linux native builds also require GPUI's system dependencies; see [GPUI's Linux setup](https://github.com/zed-industries/zed/blob/main/docs/src/development/linux.md).

```sh
cargo build
cargo run -p sigmadockd
```

In another terminal:

```sh
# Returns a project ID. The repo needs at least one commit.
cargo run -p sigmadock-cli -- project /absolute/path/to/repo
# Use the returned project ID here.
cargo run -p sigmadock-cli -- spawn PROJECT_ID --title "Fix the login bug" --agent claude --prompt "Fix the login bug and run the relevant tests"
cargo run -p sigmadock-cli -- ls
cargo run -p sigmadock-cli -- attach WORKER_ID
# Ctrl-] detaches; the agent continues in the daemon.
cargo run -p sigmadock-ui
```

Build/install binaries with `cargo install --path crates/sigmadock-cli`, `cargo install --path crates/sigmadockd`, and `cargo install --path crates/sigmadock-ui`. Binaries are `sdk`, `sigmadockd`, and `sigma-dock`. Start the daemon separately before the UI. Use `--agent shell` to test without an agent subscription. Adapters also exist for `codex`, `gemini`, `opencode`, and `aider`; their current flags must be tested against your installed versions.

Each worker gets a unique `sigma/UUID` branch, a worktree outside the source checkout, a PTY, and `PORT` and `SIGMA_DOCK_WORKER_ID` environment variables. Each project has its own berths: at most ten worker sessions per project run concurrently by default (`sigmadockd --max-workers N`, also accepted as `--berths-per-project`). Orchestrators have a separate allowance of one per project. `sdk capacity` shows global and project counts; `sdk max-workers N` changes and persists the per-project limit. Ports 4200–4999 are assigned uniquely among active workers and checked for availability; they are best-effort leases, not OS reservations.

`SIGMA_DOCK_STATE_DIR` overrides local state. Defaults: `~/Library/Application Support/SigmaDock` on macOS, `$XDG_STATE_HOME/sigma-dock` or `~/.local/state/sigma-dock` on Linux. `SIGMA_DOCK_SOCKET` overrides the socket for all binaries. Keep the daemon socket and database on a local filesystem. The state directory is mode 0700 and the socket and database are mode 0600.

## Worker operations

```sh
sdk message WORKER_ID "Run tests and fix the failures"
sdk status WORKER_ID
sdk diff WORKER_ID
sdk diff WORKER_ID --stat
sdk summary WORKER_ID > note.md  # Markdown session summary for Obsidian and similar notes
sdk stop WORKER_ID
sdk resume WORKER_ID --continue
sdk archive WORKER_ID             # preserves files and branch
sdk archive WORKER_ID --cleanup   # removes a clean worktree, preserves branch
sdk prune PROJECT_ID              # prunes stale git worktree registrations
sdk spawn PROJECT_ID --title "Next task" --agent codex --queue
sdk fork WORKER_ID --title "Explore another approach" --prompt "Try the alternative"
sdk fork WORKER_ID --title "Continue local work" --include-uncommitted --queue
sdk queue                         # waiting tasks in FIFO order
sdk queue cancel TASK_ID
sdk queue retry TASK_ID --acknowledge-unknown  # after inspecting an interrupted/failed start
sdk remove-project PROJECT_ID     # requires no unarchived workers or waiting tasks
```

`sdk summary` and the **Summary** button (or **Copy summary** on a departed worker) build a Markdown note without any model: YAML frontmatter, the recorded outcome (status, PR, checks, review, session exit), the original instruction, commit subjects since the worker forked, and per-folder diff stats. It reads the live worktree when present, so uncommitted edits count, and the branch after `--cleanup`. Times are UTC. Workers created before this version have no recorded instruction or finish time.

Use **Fork…** on an agent row or in its **More** menu to start another worker from that worker's current branch HEAD. The agent is inherited unless you choose another harness, and you can supply a new title and instruction. **Include uncommitted changes** copies staged and unstaged tracked edits plus untracked files, retaining the staged/unstaged split. Ignored files are excluded unless already staged. The source's HEAD, index, working files and stash are untouched. Both the CLI and UI can queue a fork when a project is full; HEAD and local changes are captured when requested, so later source edits do not change the queued fork. Forks show their source on the agent list and terminal header. A HEAD-only fork also works after the source worktree has been cleaned up, while its branch remains.

Local-change snapshots are limited to 128 MiB of changed files and patches. Resolve merge conflicts first; sparse checkouts, changed submodules, embedded repositories and special files require committing the changes or choosing HEAD only. Snapshots use Git's normal clean filters. Repository scripts in a copied `.sigmadock.toml` still require the normal approval before execution.

The native UI includes a task-creation form, harness picker, worker controls, a unified diff viewer, and PR links. When all of a project's berths are occupied, the form explains the per-project limit and disables creation in that project until one of its sessions ends. It keeps your task details; the native waiting-list flow remains pending. The CLI and opt-in MCP spawning support persistent queuing with `--queue` / `queue: true`. Click **Choose repository…** in the task form to open the native folder picker. Form input currently supports typing at the end, backspace, tab and clipboard paste; full text editing and IME support are pending.

Workspace and terminal updates share a local daemon event subscription. Unchanged previews do not fetch output, and workspace state resyncs every 30 seconds for recovery.

Closing the UI does not stop workers. Normal daemon shutdown stops its sessions and saves their final observed state. Crashing the **daemon** loses its PTY handles: persisted active sessions become `lost`, never silently healthy. `sdk resume` starts a fresh process in the existing worktree; `--continue` asks a supported harness to resume its own conversation. Inspect and stop any surviving process before resuming after a daemon crash; `--acknowledge-unknown` is required for an interrupted worker. Codex continuation requires `sdk resume WORKER_ID --continue` without `--prompt`; send the next instruction with `sdk message` after startup. This prototype does not recover a live PTY across daemon restarts.

Live output replay is bounded to the latest 1 MiB per session and held in memory. A plain-text recovery tail of up to 16 KiB per worker is also saved locally in SQLite, with recorded state, activity/checkpoint times and PID; it does not recover a live PTY. Recovery retention is capped at seven days and 512 contexts. Use **Unfinished sessions** to inspect/copy context, attach a live session, start a fresh process or continue a supported harness, archive, or clear saved context. `sdk unfinished` and `sdk context WORKER_ID [--clear]` expose the same data. Reconnecting after that limit resets the terminal and replays the retained tail; terminal state may be incomplete. BEL and OSC 9/777 notifications flag `Needs you`; sixty seconds without I/O means `idle`, which remains `Working`. These are heuristics, not reliable inference of every harness's intent.

## Readiness pane

Choose **Readiness** on a berth, or press **⌘⇧K** with a berth or terminal focused. The pane combines local Git changes and ahead/behind/push counts, observed PR state, CI results and excerpts, unresolved review comments grouped by file, and merge conflicts. Its header shows **Ready**, **Blocked by N**, or **Unknown**. Failures take precedence over approval; missing, errored, incomplete or mismatched commit data never counts as passing. Readiness remains advisory and does not verify protected-branch rules or merge automatically.

Each feedback action opens an exact-text preview. Choose **Send to agent** to confirm, or cancel. CI uses the existing delivery guard; changed CI feedback must be previewed again. Git, PR, review and conflict plans use the existing bracketed-paste message path and reject previews after the local or observed PR HEAD changes. Review sends the displayed unresolved comments together; providers that omit comment resolution are shown as unknown.

Readiness loads on demand without additional background forge polling or an implicit Git fetch. **Reload** reloads local Git and detail data; **Refresh forge facts** also runs the existing explicit facts refresh. Git comparisons use cached remote refs, and results may become stale as work continues. Press **Esc** to close the pane.

## Worker base branches

New workers start from a freshly fetched branch on `origin`, independent of the source checkout's current branch or local commits. SigmaDock detects and stores the default from `refs/remotes/origin/HEAD` when a project is added; edit it with `sdk project-base PROJECT_ID release` (use the branch name without `origin/`). Existing projects are detected on first use. If the remote default is unknown, configure the branch explicitly or run `git remote set-head origin -a` first.

Each launch fetches only that branch with an eight-second timeout. A failed fetch uses the last cached remote commit and shows a warning on the berth; without a cached commit, creation fails. The user's checkout and local branches remain untouched. Queued tasks record the project branch at submission and fetch its latest commit when they start.

`sdk spawn PROJECT_ID --title "Stacked task" --base sigma/OTHER_WORKER_ID` overrides the project default and skips fetching. The task form's **Base ref override** field and MCP `spawn_worker`'s `base` argument provide the same override. Local-only repositories require an explicit base ref.

## Forge facts and feedback

Connect a project in **Settings → Forges** (⌘,). SigmaDock fills in the forge, owner and repository from the `origin` remote, **Test connection** confirms the token, and **Save** applies the forge to the project's current agents and every new one. The token comes from your GitHub CLI login (`gh auth token`; run `gh auth login` once) or from an environment variable of the daemon. Apps opened from Finder have no shell environment, so the GitHub CLI is the simplest choice there. SigmaDock stores where to read the token, never the token itself.

Single workers can still be configured from the CLI:

```sh
sdk forge WORKER_ID --owner my-org --repo my-repo --github-cli
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

The native window shows a list of **agents**, for all projects or the one selected in the sidebar, grouped into **Needs you**, **Running** and **Stopped**, with today's archived agents last. Each row shows the task, project, harness, branch, status, the latest line of terminal output, **Readiness** and one contextual action (**Reply**, **Send CI** or **Open PR**); click a row to open the agent. Each project runs at most its agent limit at once (ten by default, set in **Settings → Agents** or with `sdk max-workers N`); a full project does not limit the others.

The sidebar lists each project with its agents underneath, plus an **Inbox**. Opening an agent splits the window into the sidebar, the agent's terminal session and its **Changes**: the unified patch against the merge base of the worker’s recorded base ref and HEAD, with **Committed**, **Uncommitted** (index and working tree), and **Untracked** sections. The file list shows status and added/removed counts. **Viewed** checkboxes persist locally across restarts and reset when a file’s content or diff changes; use Space or Enter when focused. Binary files have a visible note. Patches are limited to 100 file entries, 64 KiB per file and 256 KiB overall, with visible truncation notices; incomplete patches cannot be marked viewed. `sdk diff WORKER_ID` prints the same sections; `--stat` prints their summaries. Changes, files and individual lines open in your own editor (Zed, Cursor, VS Code, Sublime Text, JetBrains IDEs — IntelliJ IDEA, RustRover, GoLand, PyCharm and WebStorm, including Toolbox installs — Xcode, the default app or a custom command, chosen in Settings); SigmaDock has no built-in editor.

**Settings → Editor** also offers `$VISUAL` / `$EDITOR`: SigmaDock prefers non-empty `VISUAL`, then `EDITOR`, splits the command into argv without shell evaluation and runs it in its own terminal pane. Vim/Neovim and other terminal editors receive `+N path`; Helix receives `path:N:1`. Quoted executable paths and arguments are supported, while shell expansion and pipelines are not. The pane stays open while you switch workers or views, and does not consume a berth or send input to the agent. Save and exit or close the editor pane before opening another target; closing the app stops its editor process. GUI editor detection prefers PATH launchers, then installed app bundles, including bundle-ID lookup for nonstandard install locations. Installed app icons appear in the picker and open buttons. The agent header's dropdown changes the editor only for the current worker; choose the saved default in Settings. Editor launch errors include a clickable link back to Editor settings.

The Inbox pairs a chat with your default agent with the agents that need you and, for repositories with a configured forge token, open issues assigned to you and pull requests requesting your review. Choose the default agent's harness (Claude Code or Codex) and project in **Settings → Default agent**. Its message box starts that project's orchestrator with a briefing of the inbox and your first message, resumes it if it has stopped, or messages it while it runs; the orchestrator holds no berth.

Keyboard navigation on macOS:

| Shortcut | Action |
|---|---|
| Tab / Shift+Tab | Move focus between projects, agents and their actions |
| ↑ / ↓ on an agent | Move through the agent list |
| Enter on an agent | Open it |
| ⌘Enter on an agent | Run its contextual action (Reply, Send CI, or Open PR) |
| ⌘[ | Return from an open agent to the list |
| ⌘I | Open the Inbox |
| ⌘⇧O | Open the agent's worktree in your editor |
| ⌘1 | Show all agents |
| ⌘2–⌘9 | Select the first eight projects in sidebar order |
| ⌘N / ⌘, | New task / Settings |

Focused controls use the theme's focus ring. Tooltips include the task title, status and shortcuts. In the full terminal, plain Escape, Tab, Enter and arrow keys continue to go to the agent. The task form and settings keep their own input handling.

GPUI 0.2.2 does not expose accessibility labels for these custom controls. Tooltips and visible labels do not establish VoiceOver support; native screen-reader qualification remains pending.

Each worker's status is computed, never set manually:

- **Working:** active or idle without a PR or blocker.
- **Needs you:** input notification, lost session, unsuccessful exit, CI failure, requested changes, conflict, closed PR, or forge fetch failure.
- **In review:** draft/open PR without all readiness signals.
- **Ready to merge:** open PR with approval, passing observed checks and positive mergeability; merged PRs remain visible until archived.

## Project orchestrator tools

`sigmadock-mcp` is a local stdio MCP bridge to the same daemon. Configure your agent's MCP client to launch it (install with `cargo install --path crates/sigmadock-mcp`). It exposes `list_workers`, `get_worker_status`, `message_worker`, and `archive_worker`. The daemon must already be running and the socket environment must match.

Spawning is disabled by default: the user creates proposed workers with `sdk spawn`. Explicitly launching `sigmadock-mcp --allow-spawn` enables `spawn_worker` within the daemon's concurrency limit. Archive never removes worktrees through MCP. Managed Claude and Codex orchestrators are available with `sdk orchestrator PROJECT_ID --agent claude --prompt "Plan the next tasks"`. Add `--allow-spawn` to authorize worker creation. Install `sigmadock-mcp` next to the daemon, or pass `sigmadockd --mcp-binary /absolute/path/sigmadock-mcp`. Each project has at most one unarchived orchestrator; resume or archive it before creating another. Per-session MCP settings do not alter global harness configuration.

Project-scoped tools include `read_planning_notes` and `write_planning_notes`; writes require the previous revision and persist locally in SQLite. `sdk notes PROJECT_ID` reads them. Tool scope prevents accidental cross-project calls but does not sandbox the harness, which runs as your user.

## Subscription usage

Open **Usage** on a worker for provider-reported subscription windows, used/remaining percentages and reset times. Codex queries its CLI account API; Claude offers an opt-in session-local status-line collector. Unsupported data is labeled unavailable, and account quotas are never treated as per-worker allowances. See [usage and privacy details](docs/USAGE.md).

## Zero-telemetry policy

SigmaDock sends no analytics, crash reports, tracking events, or remote logs. Local terminal data is never uploaded by SigmaDock. Logs stay on local stderr; metadata stays in SQLite. There is no analytics SDK, hosted backend, account, or telemetry endpoint. `deny.toml` bans known tracking crates; both CI workflows enforce the ban against the complete dependency graph. A crate-name ban is a guardrail, not proof of all transitive behavior: [the verification procedure](docs/PRIVACY.md) describes runtime checks.

**Application network egress is limited to configured GitHub or Forgejo APIs, plus explicit update checks against `api.github.com/repos/SigmaUno/sigma-dock/releases`.** Automatic update checks are disabled by default; enable daily checks or use **Check for updates** in Settings. Checks send no authentication, identifiers or workspace data. Downloads open the GitHub release page in your browser; the app does not install updates. Core worker and terminal operations use only a local Unix socket and git subprocesses. Agent CLIs and tools launched inside a worker have their own network behavior, telemetry, and permissions. SigmaDock does not sandbox or control them and cannot promise they send no telemetry. Rust dependency downloads and audit checks are build-time network traffic, not application telemetry.

The Claude adapter sets `DISABLE_TELEMETRY=1`, `DISABLE_ERROR_REPORTING=1` and `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`; the Aider adapter sets `AIDER_ANALYTICS=false`. These are best-effort settings. Codex, Gemini and other harnesses must be configured separately; [the privacy guide](docs/PRIVACY.md) links their official guidance. Credentials already in the daemon environment are inherited by child processes; use a dedicated daemon environment if those processes should not see your forge tokens. OS keychain integration is planned.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
python3 scripts/smoke.py
python3 scripts/feedback_smoke.py
python3 scripts/events_smoke.py
```

`cargo build` defaults to the headless binaries. `cargo build -p sigmadock-ui` builds the native UI. Both `.forgejo/workflows/ci.yml` and `.github/workflows/ci.yml` run the same checks. Mirror setup is an administrator operation and is not performed by the repository.

[Releasing crates](docs/RELEASING.md) · [Architecture](docs/ARCHITECTURE.md) · [Roadmap](docs/ROADMAP.md) · [Privacy verification](docs/PRIVACY.md)

Terminal appearance is available from **⚙ Settings** in the top-right workspace header (Cmd/Ctrl+,). Change fonts, size, dark/light themes, individual RGB/ANSI colors, cursor shape/blinking, line spacing and padding without restarting workers. Preferences stay in the local state directory’s `preferences.json`; **Restore appearance defaults** resets this section.

Update checks compare tagged semantic versions and require a compatible macOS installer. Preview enables prereleases; stable skips them. Development snapshots retain their Cargo version and source commit, but hashes are never ordered. [Update design and installation](docs/UPDATES.md) describes caching, privacy and the manual upgrade path.

The interface follows system appearance using Catppuccin Latte (light) and Mocha (dark), including live system-mode changes. New terminal preferences also follow the system palette. **Terminal colors: custom** and edited colors preserve your choices independently; existing preference files retain their saved colors. Catppuccin and Lucide icon attribution is included in app and crate packages.

The agent view's right pane has **Changes**, **Readiness**, and **Scripts** tabs. Readiness shows the blocker count, Git/PR/CI/review checklist cards, relative check times, and failing checks inline. Refresh updates forge facts; feedback opens an exact-text preview with **Send to agent** and **Copy**. Script setup failures stay visible above the terminal with a link to Scripts.

**CI preview** opens a native results pane for the selected worker, with checks/statuses/workflows/jobs, commit and refresh time, expandable details, copy buttons and source links. Refresh is independent of the terminal. Changed-head results are marked stale. GitHub shows API check output, annotations and job steps without following log-download redirects; Forgejo Actions remain opt-in and show bounded failed-job log excerpts where supported. `sdk ci-preview WORKER_ID` returns the same structured report; `sdk ci WORKER_ID` remains feedback preview.

### Repository workspace scripts

Check in `.sigmadock.toml` at your repository root to prepare every worker consistently:

```toml
[scripts]
setup = """
pnpm install
cp "$SIGMA_DOCK_ROOT_PATH/.env" .env
"""
archive = "./script/sigmadock-archive.sh"
run_mode = "concurrent" # nonconcurrent stops the other runs in this worker first

[scripts.run.web]
command = "pnpm dev --port $PORT"
default = true

[scripts.run.test-watch]
command = "pnpm test --watch"
```

Hooks execute through `/bin/sh -c` in the selected worker's worktree. Every script receives `SIGMA_DOCK_ROOT_PATH`, `SIGMA_DOCK_WORKTREE_PATH`, `SIGMA_DOCK_WORKER_ID`, `SIGMA_DOCK_BRANCH` and the worker's leased `PORT`. Ignore generated files (such as `.env` and dependency folders) in Git, or remove them in the archive hook before requesting cleanup.

A new worker waits for approval of the exact file contents before executing any repository hook. Open its berth to review the complete config and approve it. Approval is stored per project in SQLite and invalidated when the content hash changes. Skipping setup starts the agent without approving or executing scripts. Setup runs before the agent; failure leaves the worker blocked with output and **Retry setup**, **Skip setup and start agent**, and **Archive** actions. Run/stop actions appear in the agent list; each named run has a separate attachable terminal in the worker view.

```sh
sdk scripts WORKER_ID                       # review contents, commands, SHA-256 and approval
sdk scripts WORKER_ID --approve HASH        # approve exactly the reviewed contents
sdk setup WORKER_ID                         # retry a failed setup
sdk setup WORKER_ID --skip                  # start the agent without running setup
sdk run WORKER_ID                           # start the default run
sdk run WORKER_ID test-watch
sdk attach WORKER_ID --script run:test-watch # Ctrl-] detaches without stopping the run
sdk run WORKER_ID --stop                    # stop this worker's run scripts
sdk run WORKER_ID test-watch --stop
sdk attach WORKER_ID --script setup
sdk stop WORKER_ID                          # stop the agent and its script sessions
sdk archive WORKER_ID --cleanup             # stream archive output, then remove a clean worktree
sdk archive WORKER_ID --cleanup --force     # continue after hook failure; approval still required
```

Archive hooks run before any cleanup. A hook failure blocks archive; `--force` permits continuation after that hook's failure, but still refuses uncommitted files. After an interrupted daemon session, inspect surviving processes before using `--acknowledge-unknown` with `sdk setup`, `sdk resume`, or `sdk archive`. Hooks are never rerun automatically after a restart. Archive output for the 32 most recent archived workers is retained in memory until daemon restart; agent recovery checkpoints remain persisted separately.

Stopping sends SIGTERM to the PTY process group, then SIGKILL after two seconds. Descendants still reachable in the process tree at stop time are also signalled, including children that created their own session; independently reparented processes that escaped before stop are outside this guarantee. Nonconcurrent runs wait for this cleanup before the next command starts. A config is limited to 64 KiB and 16 named runs; unknown fields, invalid modes, empty commands and multiple defaults produce errors. Per-user config overrides and port ranges are not supported.
