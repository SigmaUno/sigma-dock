# macOS menu bar investigation (#36)

Status: recommendation, before implementation. Investigated 2026-10-07 against GPUI 0.2.2 and repository commit `5053fe4`. No menu bar or notification capability is implemented by this document.

## Recommendation

Use `tray-icon` with its `muda` menus inside the existing GPUI UI process, conditional on a native compatibility spike. Keep an application-level supervisor alive when its last window closes. This avoids another executable, signing identity and window-routing protocol. Prefer native text menu rows first; custom AppKit views can follow if necessary.

The documented main-thread requirement fits GPUI's AppKit run loop, but that is an inference, not a verified integration. Prove startup timing, menu callbacks and window reopening on the pinned GPUI version before committing to the dependency. If the spike fails, use a small `objc2-app-kit` status-item adapter in the same process. A separate helper is the fallback only if a windowless GPUI process cannot be made reliable.

## Options and evidence

| Option | Benefit | Cost / qualification | Recommendation |
|---|---|---|---|
| `tray-icon` + `muda` | Native menus, template icon support, stable worker IDs in actions; other platforms supported | Create/update/drop on main thread after AppKit starts; prove GPUI callback integration | First spike; macOS-only dependency initially |
| Direct `objc2-app-kit` | Full control over `NSStatusBar`, `NSStatusItem`, menu targets and custom views | Own Objective-C lifetimes, action dispatch and main-thread constraints | Fallback if wrapper conflicts |
| Separate menu helper | Indicator survives UI process exit/crash | Extra signed binary or bundle, IPC/window routing and duplicate-instance handling | Defer; not required merely for closing a window |
| Upstream GPUI | Would avoid maintaining an adapter | No usable API found in pinned version or inspected upstream platform trait | Do not upgrade or vendor leftover code for this feature |

[`tray-icon` documentation](https://docs.rs/tray-icon/0.26.0/tray_icon/) requires a running main-thread event loop before icon creation and exposes menu events through `muda`. Its [macOS backend](https://github.com/tauri-apps/tray-icon/blob/dev/src/platform_impl/macos/mod.rs) uses AppKit; it is not a second application event loop. Linux support has backend/system-library choices, so it is not automatically part of this macOS change. Direct bindings expose status-item creation/removal through [`objc2-app-kit`](https://docs.rs/objc2-app-kit/0.3.2/objc2_app_kit/struct.NSStatusBar.html).

Locally inspected GPUI 0.2.2 `src/platform/mac.rs` does not declare the included `status_item.rs`. `Application::on_reopen` in `src/app.rs` provides a Dock/Finder reopen hook. SigmaDock's `main.rs` currently opens one window and stores its `EventMonitor` in `Workspace`; that ownership cannot sustain an indicator after the workspace is destroyed. No explicit SigmaDock quit-on-last-window handler was found, but native windowless behavior still needs verification.

Upstream Zed was checked at `eb466341a6b4306604cffef466cfdf9b1aafcecc`: its [GPUI platform trait](https://github.com/zed-industries/zed/blob/eb466341a6b4306604cffef466cfdf9b1aafcecc/crates/gpui/src/platform.rs) exposes a Dock menu, with no status-item/tray API found. The macOS backend has moved into separate crates. A non-truncated repository tree search found no status-item/tray file under `crates/gpui` or `crates/gpui_macos`. This does not establish upstream plans; no upgrade is assumed to solve #36.

## Process and window lifecycle

Create one application-owned supervisor holding the tray, daemon client, shared event feed, latest snapshot and optional window handle. Workspace rendering consumes the same snapshot; closing a window drops terminal views but keeps supervision and the tray. Preserve #19's event-driven refresh with bounded reconnect and periodic resync; do not add another worker polling loop.

Route menu callbacks into GPUI through a queued foreground action, outside native menu callbacks and existing `App` borrows. Never fetch RPCs synchronously while a native menu is tracking. Use worker IDs, not row indices. Coalesce updates; rebuild menus outside tracking so an update cannot invalidate the clicked item. Preserve keyboard navigation and accessibility with text states rather than colour alone.

Normal launch opens the window and tray. A future `--background` mode may start with only the tray; it must still create the application supervisor. Closing the last window retains the process. `Open SigmaDock`, worker actions and `on_reopen` use one open-or-focus path: reuse an existing window, otherwise create one, select the worker's project and focus its berth/terminal after loading its snapshot. Missing/archived workers open the workspace with a useful message. Enforce one UI supervisor per daemon/socket so repeated launches do not create duplicate icons.

The menu groups all unarchived workers by project, including exited workers awaiting acknowledgement; attention groups/rows sort first, then stable project/title/ID ordering. Each row shows title, harness and state. Show a template icon plus attention count, quiet for idle/running, and an explicit disconnected/unknown indication when the daemon is unavailable. Keep `Open SigmaDock` available even with no workers or a disconnected daemon. `Quit SigmaDock` ends the UI/indicator explicitly and leaves daemon workers running. Automatic login/reboot startup is outside this issue; “always running” means until explicit quit, logout or crash, not an undeclared launch service.

## Shared attention rules

Add one pure attention projection in `sigmadock-core`, used by the daemon, tray and berth decorations. Keep the existing forge-derived `Status`; do not equate `NeedsYou` with the narrower notification triggers or infer completion from idle output.

| Fact / transition | Attention reason | Notification |
|---|---|---|
| `NeedsInput` | Waiting for input or approval | Once per new waiting episode |
| `Exited` with exit code 0 | Finished session | Once per completed session |
| `Exited` with nonzero code | Failed session | Once per failed session |
| `Idle` or `Running` | None | None |
| `Lost` / unknown exit outcome | Session unavailable | Show explicitly; do not claim successful completion |
| CI/review/merge facts | Existing forge status | Outside the three notification triggers |

Long-lived agents may finish a task without exiting. Current PTY facts cannot prove that: BEL/OSC notifications are heuristic input requests, and silence means idle. Harness-specific completion signals are a follow-up; the first version must label process exit as session completion rather than fabricate task completion.

Persist attention episodes/acknowledgements with a session identity that survives daemon restart; the current output generation is process-local and insufficient for durable deduplication. Acknowledging a finished/failed episode clears its badge but retains the worker row. Input clears waiting only when the daemon observes the state transition, so simply opening a terminal cannot hide an unresolved approval. Record user-requested stop separately: current `stop_worker` only kills the PTY, so stop currently looks like an error exit. Intentional stops and archives must not produce failure notifications. Baseline restored workers on daemon startup; never replay a flood of old completed-session notifications. A persisted notification outbox can bound retries; crashes during delivery may still duplicate a notification, so exactly-once OS delivery is not promised.

## Daemon notifications and bundle qualification

The daemon owns transition detection, deduplication and local notification submission, independently of window visibility. Menu code must not send the same notification again. Request notification permission through an explicit setting and handle denied/revoked permission without repeated prompts; badge state still works. No push service, accounts or remote notification traffic is required. [Apple documents permission requests](https://developer.apple.com/documentation/usernotifications/asking-permission-to-use-notifications) and the [`UNUserNotificationCenter` API](https://developer.apple.com/documentation/usernotifications/unusernotificationcenter).

The current app uses `com.sigmauno.sigmadock`; `sigmadockd` is a plain executable in `Contents/MacOS`, with no separate helper bundle or declared notification identity. Being inside a signed parent bundle does not yet prove which identity/authorization the daemon receives. Test `NSBundle.mainBundle`, notification permission, actual delivery and response callbacks in the packaged, signed daemon under the Finder/minimal PATH launch. If that layout is unsupported, introduce a signed nested notification helper bundle, invoked by the daemon, with an explicit identity; assess packaging and permission UX before choosing it. Do not replace GPUI's application delegate to add notification callbacks.

For unbundled `cargo run` and headless/server daemons, keep native notification delivery disabled with an actionable local diagnostic; events and attention remain available. App-window closure must not affect delivery. Notification clicks need the same worker-ID open-or-focus route, with a qualified bundled callback/helper path. Production identity testing depends on the Apple credentials and signed-build qualification in #7; ad-hoc tests cannot establish it.

## Implementation sequence and acceptance

1. Native compatibility spike: template icon, one native menu action, callback-to-GPUI routing, repeated close/reopen, windowless updates, fullscreen/light/dark behavior and explicit Quit. Check no second run loop or delegate replacement. Record results before adopting the adapter.
2. Shared attention projection and persisted episodes: test waiting/input, success/error exits, deliberate stop, archive, resume, daemon restart, acknowledgement and simultaneous reasons.
3. Application-level supervision, grouped menu and worker focus: test background start, disconnected/reconnect, stale menu IDs, two launch attempts and menu tracking during rapid updates. Measure idle RPCs to preserve #19.
4. Daemon notification identity spike and integration: test permission grant/deny/revoke, delivery with no window, response routing and deduplication; document the supported signed-bundle identity. Update bundle signing/smoke scripts if a helper is necessary.
5. Native qualification on Apple Silicon and Intel: keyboard/VoiceOver menu use, template contrast, finished/failed/waiting badges agreeing with berths, worker survival and no missed attention after closing the last window. Run workspace checks and local lifecycle/event smokes; CI remains tag-only. Keep #36 open until these runtime checks pass.

This PR supplies the requested first-task write-up. Native integration, notification identity and runtime acceptance are deliberately marked unverified rather than reported as implemented.
