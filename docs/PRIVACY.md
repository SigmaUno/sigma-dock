# Privacy and zero-telemetry verification

SigmaDock has no application analytics, crash upload, update check, remote logging or account service. Runtime HTTP exists only in the explicitly configured forge adapter. Local IPC uses an owner-only Unix socket. Terminal output stays in a bounded in-memory ring; project/worker metadata and observed forge facts stay in local SQLite.

## Verify a build

1. Run `cargo deny check` against the lockfile. The tracking SDK deny list is enforced in both CI definitions and includes Sentry, PostHog, Segment, OpenTelemetry exporters and other analytics SDKs. This does not detect every possible tracking implementation.
2. Review network entry points with `rg 'reqwest|https?://|TcpStream|UdpSocket' crates`. The port allocator briefly binds loopback to check availability; it makes no outbound connection. Check dependency source changes when updating the lockfile.
3. Run the daemon and UI without any configured forge and without workers. Use an OS connection monitor (`lsof -nP -i` on macOS, `ss -tpn` on Linux) and a packet capture to verify those processes initiate no network connections. Attribute process and child traffic separately.
4. Configure a test forge and confirm only that API host is contacted. Redirects are disabled and response errors do not print credentials. Review proxy environment settings if traffic passes through a configured proxy.
5. Run a shell worker, then individual harnesses, recording their child-process connections separately. A harness can spawn network-capable tools or call external services, including telemetry services. SigmaDock provides no network sandbox.
6. Verify local data permissions and search artifacts for credentials; the SQLite forge configuration stores only the token environment variable name. The daemon inherits tokens and passes its environment to workers, so isolate its environment when needed.

This procedure must be performed on a release build before claiming a completed runtime audit. No such audit is implied by the policy or dependency bans alone.

## Harness settings

Use the installed harness's current official documentation; settings change between versions. No single environment variable proves a harness sends no tracking data.

| Harness | SigmaDock behavior | Official guidance |
|---|---|---|
| Claude Code | Disables telemetry, error reporting and nonessential traffic via documented environment flags | [Data usage](https://code.claude.com/docs/en/data-usage) |
| Codex | Does not configure OpenTelemetry; inspect your user configuration and disable configured exporters | [Configuration reference](https://developers.openai.com/codex/config-reference/) |
| Gemini CLI | Inherits your settings; explicitly disable telemetry and usage statistics in your Gemini configuration | [Telemetry](https://geminicli.com/docs/cli/telemetry/) |
| Aider | Adds `AIDER_ANALYTICS=false` | [Analytics](https://aider.chat/docs/more/analytics.html) |
| opencode | Inherits your configuration; review plugins and providers as well as the CLI | [Configuration](https://opencode.ai/docs/config/) |

Deleting the local state directory removes metadata and stored worker worktrees. Archive with `--cleanup` is safer because it refuses uncommitted files and preserves the worker's branch in the source repository. Logs may contain local task paths and operational errors; do not upload them inadvertently.
