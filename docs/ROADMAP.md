# Roadmap

This is an implementation roadmap, not a claim that every phase is complete.

## Phase 0 — Foundations

- [x] SigmaDock name and Apache-2.0 license.
- [x] Eleven-crate Rust workspace with daemon/UI separation.
- [x] Forgejo and GitHub CI definitions for fmt, clippy, tests and dependency policy.
- [x] Zero-telemetry policy, tracking-crate bans and verification guide.
- [x] Use the existing sigmadock.dev domain; prepare sigmadock-* crate metadata.
- [ ] Publish crate names in dependency order; configure repository mirroring externally.

## Phase 1 — One agent in a window

- [x] Native GPUI window and Alacritty-based terminal component.
- [x] Daemon-owned portable PTY, input, output and resize.
- [x] Thin Claude, Codex, Gemini, opencode, Aider and shell adapters.
- [ ] Manually qualify installed harnesses, alt-screen, mouse reporting, bracketed paste, truecolor, kitty keyboard, IME, selection and clipboard.
- [ ] Address terminal component mouse/scrollback limitations before claiming normal-terminal parity.

## Phase 2 — Workers

- [x] Versioned local JSON-RPC, CLI attach and bounded replay.
- [x] Unique worktree and branch per task; metadata in SQLite.
- [x] Live sidebar, notifications inferred from BEL/OSC and idle timer.
- [x] Port allocation and safe worktree cleanup/prune commands.
- [x] Five concurrent shell workers verified in the automated lifecycle smoke test.
- [x] UI reconnects to running daemon sessions.
- [x] Native project/worker creation form and lifecycle controls.
- [ ] Desktop notifications and full text editing/IME in forms.
- [x] Bounded local transcript checkpoints, unfinished-session UI, explicit unknown-state acknowledgement and context clearing.
- [ ] Recover live PTYs across daemon restarts; currently explicit lost-state and manual resume.
- [x] Signal-driven daemon shutdown stops sessions, reaps children and persists observed state.
- [ ] Robust process-group cleanup for child tools that detach or ignore hangup.

## Phase 3 — Board

- [x] Pure status derivation with blocker precedence tests.
- [x] Four native board columns and card-to-terminal navigation.
- [x] GitHub/Forgejo REST PR/status/review facts with timeout and failure backoff.
- [x] CLI diff summary, status and PR URL.
- [x] Conditional ETag requests and backoff respecting numeric Retry-After/rate-reset headers.
- [x] Bounded pagination for checks, reviews, comments and Forgejo Actions.
- [ ] Shared per-host rate-limit budgets.
- [x] Opt-in Forgejo Actions run/job/log support.
- [ ] Protected-branch readiness semantics.
- [x] Native diff summary and PR links; session/check/review status detail.
- [ ] Rich diff and check detail panes.

## Phase 4 — Feedback loop

- [x] Preview and explicitly send inline review comments to the owning agent.
- [x] Manual instruction injection via CLI or MCP.
- [x] Trim and send Forgejo job logs and GitHub check output/annotations.
- [ ] Full GitHub job logs while preserving the configured API egress policy.
- [x] Optional single-attempt automated CI feedback per head commit.
- [x] Preview/send a conflict-resolution instruction; execution stays with the worker.

## Phase 5 — Orchestrator

- [x] Local stdio MCP tools: list, status, message, archive and opt-in spawn.
- [x] Default manual spawn confirmation and daemon concurrency cap.
- [x] Managed per-project orchestrator sessions and revision-checked persistent planning notes.
- [x] Per-session Claude/Codex MCP configuration generation.

## Phase 6 — Release

- [x] Universal macOS app/DMG workflow with bundled helpers and optional signing/notarization.
- [ ] Configure Apple Developer ID credentials and qualify signed downloads.

- [ ] Terminal compatibility matrix with observed results on macOS and Linux.
- [ ] Settings UI and OS keychain token storage.
- [ ] Linux packages, AppImage and Homebrew tap.
- [ ] Signed release artifacts, docs site and demo recording.
- [ ] Runtime egress audit of release builds and dependency review.
