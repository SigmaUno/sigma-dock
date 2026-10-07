# Agent subscription usage

Select a worker and open **Usage**. The panel shows provider-reported plan, percentage consumed/remaining, window duration and reset time wherever available. Subscription windows belong to the shared account: running five workers does not give five independent allowances. Absolute token allowances and per-worker subscription attribution are currently unavailable; SigmaDock never invents them.

## Codex

Refresh launches a separate, read-only `codex app-server` process using the CLI's existing sign-in. It initializes the connection and calls `account/read` with `refreshToken: false`, `account/rateLimits/read`, and optionally `account/usage/read`. No thread or turn is created and no inference is requested. Provider requests may occur through this child CLI. Account email, authentication tokens and raw responses are not retained or displayed. The child is terminated after the query, with a 15-second overall deadline. Errors leave any older report visibly timestamped.

Compatible subscription accounts expose plan name and quota window percentages. API-key and Bedrock authentication do not represent a ChatGPT subscription allowance. Optional lifetime token activity is labeled separately from the subscription window and may be unsupported in older CLI versions. The UI does not turn lifetime token counts into a quota estimate.

Protocol reference: [Codex app-server account APIs](https://developers.openai.com/codex/app-server).

## Claude Code

Collection is disabled by default. **Enable on next launch** saves a worker preference; explicitly stop/resume the worker when convenient to apply it. SigmaDock supplies a session-local `--settings` status-line command using the bundled `sdk usage-report` helper. Existing global settings are unchanged. A custom status line is replaced only for that launched session. Disabling stops accepting reports immediately and removes the override on the next launch.

The helper accepts at most 64 KiB of JSON input and forwards only supported five-hour/seven-day quota percentages, reset times and current context input/output token counts to the local daemon. Workspace paths, emails, credentials, model messages and unrecognized fields are discarded. Plan names and absolute allowances are not provided by this adapter. Context counts are not session totals or account subscription consumption. Quota fields may be absent until a response completes, or unavailable for the installed version/authentication mode.

Reference: [Claude Code status-line fields](https://code.claude.com/docs/en/statusline).

## Other harnesses and privacy

Gemini, opencode, Aider and shell currently show an explicit unavailable state. Gemini users can inspect `/stats model` in the CLI itself ([command reference](https://geminicli.com/docs/reference/commands)). No terminal scraping or undocumented account endpoints are used.

Usage reports are held only in daemon memory, with source and observation time. They are cleared when a worker launches again or is archived and disappear when the daemon restarts. Refresh does not restart a PTY or modify an agent conversation. Agent CLIs may have their own network traffic/telemetry; the application's zero-telemetry policy does not claim to control their behavior. The Codex reader overrides log, trace and metrics OpenTelemetry exporters to `none` for its own process ([configuration reference](https://developers.openai.com/codex/config-reference)).

CLI equivalents:

```sh
sdk usage WORKER_ID
sdk usage-reporting WORKER_ID --enable
# Disable collection (the preference applies to command arguments on next launch):
sdk usage-reporting WORKER_ID
```
