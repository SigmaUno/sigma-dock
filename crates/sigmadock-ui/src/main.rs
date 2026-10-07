//! Native berths workspace and reconnectable terminal, backed by the daemon's PTYs.
mod agent_ui;
mod appearance_ui;
mod berths_ui;
mod bootstrap;
mod checks_ui;
mod ci_ui;
mod diff_ui;
mod editor;
mod events;
mod icons;
mod inbox_ui;
mod keyboard_ui;
mod preferences;
mod recovery_ui;
mod scripts_ui;
mod settings_ui;
mod summary_ui;
mod theme;
mod update_ui;
mod updates;
mod usage_ui;
mod viewed;

use anyhow::Result;
use clap::Parser;
use gpui::{
    App, Application, Bounds, Context, Entity, SharedString, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, rgb, size,
};
use serde_json::json;
use sigmadock_core::{Capacity, Client, Output, Project, Worker, socket_path};
use sigmadock_terminal::{TerminalConfig, TerminalView};
use std::{
    io::{self, Read, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
#[derive(Parser)]
struct Args {
    #[arg(long, env = "SIGMA_DOCK_SOCKET", default_value_os_t = socket_path())]
    socket: PathBuf,
    /// Connect to an existing daemon without starting bundled helpers.
    #[arg(long)]
    no_daemon: bool,
}
struct RemoteWriter {
    script: Option<String>,
    client: Client,
    worker: String,
}
impl Write for RemoteWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.client
            .call(
                "input",
                json!({"worker_id":self.worker,"script":self.script,"bytes":bytes}),
            )
            .map_err(io::Error::other)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct RemoteReader {
    script: Option<String>,
    client: Client,
    worker: String,
    cursor: u64,
    pending: Vec<u8>,
    offset: usize,
    ended: bool,
    connected: Arc<AtomicBool>,
    events: events::EventFeed,
    observed: Option<events::WakeStamp>,
    more: bool,
}
impl RemoteReader {
    fn event_key(&self) -> String {
        self.script.as_ref().map_or_else(
            || self.worker.clone(),
            |script| format!("{}/{script}", self.worker),
        )
    }
}
impl Read for RemoteReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            if !self.connected.load(Ordering::Relaxed) {
                return Ok(0);
            }
            if self.offset < self.pending.len() {
                let count = bytes.len().min(self.pending.len() - self.offset);
                bytes[..count].copy_from_slice(&self.pending[self.offset..self.offset + count]);
                self.offset += count;
                return Ok(count);
            }
            if self.ended {
                return Ok(0);
            }
            let stamp = if self.more {
                self.events.stamp(&self.event_key())
            } else {
                let Some(stamp) =
                    self.events
                        .wait_for_output(&self.event_key(), self.observed, &self.connected)
                else {
                    return Ok(0);
                };
                stamp
            };
            if let (Some(previous), Some(signal)) =
                (self.observed.and_then(|stamp| stamp.signal), stamp.signal)
                && previous.generation != signal.generation
            {
                self.cursor = 0;
                self.pending = b"\x1bc".to_vec();
                self.offset = 0;
                self.observed = None;
                self.more = true;
                continue;
            }
            // Capture before the RPC so an event racing with the response is not lost.
            self.observed = Some(stamp);
            let output: Output = serde_json::from_value(
                self.client
                    .call(
                        "output",
                        json!({"worker_id":self.worker,"script":self.script,"cursor":self.cursor}),
                    )
                    .map_err(io::Error::other)?,
            )
            .map_err(io::Error::other)?;
            self.cursor = output.cursor;
            self.ended = output.exited;
            self.more = output.bytes.len() == 64 * 1024;
            self.pending = output.bytes;
            self.offset = 0;
            if output.truncated {
                self.pending.splice(0..0, b"\x1bc".iter().copied());
            }
        }
    }
}
fn full_capacity_message(max_workers: usize) -> String {
    format!(
        "This project is already running {max_workers} agents, its limit. Stop one, or raise the limit in Settings → Agents, before creating this task. Automatic queuing is not available yet."
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Berths,
    Inbox,
}
/// Popover menus in the agent view header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu {
    Editor,
    More,
}
struct Workspace {
    view: View,
    /// Projects whose agents are hidden in the sidebar tree.
    collapsed: std::collections::HashSet<String>,
    diff: diff_ui::DiffState,
    scripts: scripts_ui::ScriptsPane,
    terminal_script: Option<String>,
    inbox: inbox_ui::InboxState,
    menu: Option<Menu>,
    events: events::EventMonitor,
    event_epoch: u64,
    client: Client,
    workers: Vec<Worker>,
    /// Archived workers, for the "Departed today" list.
    departed: Vec<Worker>,
    capacity: Capacity,
    previews: std::collections::HashMap<String, berths_ui::Preview>,
    /// Berth highlighted from the needs-you strip or the last closed terminal.
    focused_berth: Option<String>,
    berth_focus: std::collections::HashMap<String, gpui::FocusHandle>,
    workspace_focus: gpui::FocusHandle,
    projects: Vec<Project>,
    /// `None` shows all berths.
    selected_project: Option<String>,
    daemon_connected: bool,
    error: Option<String>,
    terminal: Option<Entity<TerminalView>>,
    selected: Option<String>,
    form_open: bool,
    fork_source: Option<String>,
    task_notice: Option<String>,
    fork_include_changes: bool,
    fork_queue: bool,
    fields: [String; 4],
    active_field: usize,
    agent: String,
    form_focus: gpui::FocusHandle,
    /// The Inbox message box for the default agent.
    composer_focus: gpui::FocusHandle,
    busy: bool,
    repo_picker_open: bool,
    details: String,
    connection: Arc<AtomicBool>,
    preferences: preferences::Preferences,
    preferences_path: PathBuf,
    settings_open: bool,
    settings_section: settings_ui::Section,
    forge_form: Option<settings_ui::ForgeForm>,
    settings_focus: gpui::FocusHandle,
    settings_editor: Option<(usize, String)>,
    settings_error: Option<String>,
    usage_open: bool,
    usage_report: Option<sigmadock_core::AgentUsage>,
    usage_loading: bool,
    usage_error: Option<String>,
    checks: checks_ui::ChecksPane,
    checks_focus: gpui::FocusHandle,
    ci_open: bool,
    ci_report: Option<sigmadock_core::CiPreview>,
    ci_loading: bool,
    ci_error: Option<String>,
    ci_expanded: Option<String>,
    recovery_open: bool,
    recovery_entries: Vec<serde_json::Value>,
    recovery_error: Option<String>,
    recovery_selected: Option<String>,
    theme: theme::Theme,
    _appearance_subscription: gpui::Subscription,
    checker: Arc<std::sync::Mutex<updates::Checker>>,
    checking_update: bool,
    update_message: Option<String>,
    available_update: Option<updates::Available>,
    /// Worker whose session summary is being built.
    summary_loading: Option<String>,
    /// Worker whose summary was just copied, for a brief confirmation.
    summary_copied: Option<String>,
}
impl Workspace {
    fn new(client: Client, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let initial = berths_ui::Snapshot::load(&client);
        let event_monitor = events::EventMonitor::start(client.clone());
        cx.spawn(async move |this, cx| {
            let mut last_resync = std::time::Instant::now();
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                let Ok(request) = this.update(cx, |this, _| {
                    let (epoch, dirty) = this.events.feed.take_refresh();
                    if epoch != this.event_epoch {
                        this.event_epoch = epoch;
                        this.previews.clear();
                    }
                    (dirty || last_resync.elapsed() >= Duration::from_secs(30))
                        .then(|| this.client.clone())
                }) else {
                    break;
                };
                let Some(client) = request else {
                    continue;
                };
                last_resync = std::time::Instant::now();
                let snapshot = cx
                    .background_executor()
                    .spawn(async move { berths_ui::Snapshot::load(&client) })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.apply_snapshot(snapshot);
                        this.refresh_scripts_phase(cx);
                        if this.preferences.updates.due(updates::now()) {
                            this.check_updates(false, cx);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let preferences_path = sigmadock_core::state_dir().join("preferences.json");
        let loaded = preferences::Preferences::load(&preferences_path);
        let settings_error = loaded
            .as_ref()
            .err()
            .map(|error| format!("Preferences could not be loaded: {error}"));
        let appearance_subscription = cx.observe_window_appearance(window, |this, window, cx| {
            this.theme = theme::Theme::for_appearance(window.appearance());
            this.refresh_terminal_appearance(cx);
            cx.notify();
        });
        // Worktree diffs and forge activity have no daemon events; refresh while visible.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                if this
                    .update(cx, |this, cx| {
                        if this.terminal.is_some() {
                            this.load_changes(cx);
                            this.load_scripts(cx);
                        } else if this.view == View::Inbox {
                            this.load_inbox(false, cx);
                            this.ensure_chat(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let mut workspace = Self {
            view: View::Berths,
            collapsed: Default::default(),
            diff: Default::default(),
            scripts: Default::default(),
            terminal_script: None,
            inbox: Default::default(),
            menu: None,
            events: event_monitor,
            event_epoch: 0,
            usage_open: false,
            usage_report: None,
            usage_loading: false,
            usage_error: None,
            checks: Default::default(),
            checks_focus: cx.focus_handle(),
            ci_open: false,
            ci_report: None,
            ci_loading: false,
            ci_error: None,
            ci_expanded: None,
            recovery_open: true,
            recovery_entries: Vec::new(),
            recovery_error: None,
            recovery_selected: None,
            theme: theme::Theme::for_appearance(window.appearance()),
            _appearance_subscription: appearance_subscription,
            checker: Arc::new(std::sync::Mutex::new(updates::Checker::default())),
            checking_update: false,
            update_message: None,
            summary_loading: None,
            summary_copied: None,
            available_update: None,
            preferences: loaded.unwrap_or_default(),
            preferences_path,
            settings_open: false,
            settings_section: Default::default(),
            forge_form: None,
            settings_focus: cx.focus_handle(),
            settings_editor: None,
            settings_error,
            client,
            workers: Vec::new(),
            departed: Vec::new(),
            capacity: Capacity::default(),
            previews: Default::default(),
            focused_berth: None,
            berth_focus: Default::default(),
            workspace_focus: cx.focus_handle(),
            projects: Vec::new(),
            selected_project: None,
            daemon_connected: false,
            error: None,
            terminal: None,
            selected: None,
            form_open: false,
            fork_source: None,
            task_notice: None,
            fork_include_changes: false,
            fork_queue: false,
            fields: [String::new(), String::new(), String::new(), String::new()],
            active_field: 0,
            agent: "claude".into(),
            form_focus: cx.focus_handle(),
            composer_focus: cx.focus_handle(),
            busy: false,
            repo_picker_open: false,
            details: String::new(),
            connection: Arc::new(AtomicBool::new(false)),
        };
        workspace.apply_snapshot(initial);
        workspace.spawn_preview_loop(cx);
        workspace.workspace_focus.focus(window);
        workspace
    }
    /// A terminal view attached to a worker's daemon PTY until `connected` is cleared.
    fn connect_terminal(
        &self,
        id: &str,
        connected: Arc<AtomicBool>,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalView> {
        let reader = RemoteReader {
            script: self.terminal_script.clone(),
            client: self.client.clone(),
            worker: id.to_owned(),
            cursor: 0,
            pending: Vec::new(),
            offset: 0,
            ended: false,
            connected,
            events: self.events.feed.clone(),
            observed: None,
            more: false,
        };
        let writer = RemoteWriter {
            script: self.terminal_script.clone(),
            client: self.client.clone(),
            worker: id.to_owned(),
        };
        let resize_client = self.client.clone();
        let resize_script = self.terminal_script.clone();
        let resize_worker = id.to_owned();
        let config = self
            .theme
            .terminal(&self.preferences.appearance)
            .apply(TerminalConfig::default());
        cx.new(|cx| {
            TerminalView::new(writer, reader, config, cx).with_resize_callback(move |cols, rows| {
                let _ = resize_client.call(
                    "resize",
                    json!({"worker_id":resize_worker,"script":resize_script,"cols":cols,"rows":rows}),
                );
            })
        })
    }
    fn open_worker(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = false;
        self.focused_berth = Some(id.clone());
        self.details.clear();
        self.usage_open = false;
        self.usage_report = None;
        self.usage_error = None;
        self.ci_open = false;
        self.ci_report = None;
        self.ci_error = None;
        self.ci_expanded = None;
        self.menu = None;
        self.terminal_script = match self
            .workers
            .iter()
            .find(|worker| worker.id == id)
            .map(|worker| worker.workspace_scripts.phase)
        {
            Some(
                sigmadock_core::workspace_scripts::Phase::SettingUp
                | sigmadock_core::workspace_scripts::Phase::SetupFailed,
            ) => Some("setup".into()),
            Some(
                sigmadock_core::workspace_scripts::Phase::Archiving
                | sigmadock_core::workspace_scripts::Phase::ArchiveFailed,
            ) => Some("archive".into()),
            _ => None,
        };
        self.connection.store(false, Ordering::Relaxed);
        self.connection = Arc::new(AtomicBool::new(true));
        let terminal = self.connect_terminal(&id, self.connection.clone(), cx);
        terminal.read(cx).focus_handle().focus(window);
        self.terminal = Some(terminal);
        if self.selected.as_ref() != Some(&id) {
            let request = self.diff.request + 1;
            self.diff = Default::default();
            self.diff.request = request;
        }
        self.selected = Some(id);
        self.scripts.request += 1;
        self.scripts.loading = false;
        self.scripts.value = None;
        self.scripts.phase = self
            .workers
            .iter()
            .find(|worker| Some(&worker.id) == self.selected.as_ref())
            .map(|worker| worker.workspace_scripts.phase);
        self.load_scripts(cx);
        self.load_changes(cx);
        cx.notify();
    }

    fn run_action(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        let client = self.client.clone();
        let owner = params["worker_id"].as_str().map(str::to_owned);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { client.call(method, params) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(value) => {
                        if owner
                            .as_ref()
                            .is_some_and(|owner| this.selected.as_ref() != Some(owner))
                        {
                            cx.notify();
                            return;
                        }
                        this.load_scripts(cx);
                        if let Some(text) = value
                            .as_str()
                            .or_else(|| value.get("text").and_then(serde_json::Value::as_str))
                        {
                            this.details = text.into();
                        }
                    }
                    Err(error) => {
                        this.error = Some(error.to_string());
                        if matches!(
                            method,
                            "run_script" | "approve_scripts" | "setup_worker" | "archive_worker"
                        ) {
                            this.load_scripts(cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn creation_blocked_reason(&self) -> Option<String> {
        if !self.daemon_connected {
            Some("Connect to the daemon before creating a task.".into())
        } else if !(self.fork_source.is_some() && self.fork_queue)
            && self
                .projects
                .iter()
                .find(|project| project.path.to_string_lossy() == self.fields[0].trim())
                .is_some_and(|project| self.project_full(&project.id))
        {
            Some(if self.fork_source.is_some() {
                format!(
                    "All {} berths in this project are occupied. Enable ‘Queue if no berth is free’ to capture a fork now and start it when a berth opens.",
                    self.capacity.max_workers
                )
            } else {
                full_capacity_message(self.capacity.max_workers)
            })
        } else {
            None
        }
    }
    fn create_worker(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(reason) = self.creation_blocked_reason() {
            self.error = Some(reason);
            cx.notify();
            return;
        }
        if self.fields[0].trim().is_empty() || self.fields[1].trim().is_empty() {
            self.error = Some("Enter a repository path and task title".into());
            cx.notify();
            return;
        }
        self.task_notice = None;
        self.busy = true;
        self.error = None;
        let client = self.client.clone();
        let path = self.fields[0].clone();
        let title = self.fields[1].clone();
        let prompt = self.fields[2].clone();
        let base = self.fields[3].trim().to_owned();
        let agent = self.agent.clone();
        let last_capacity = self.capacity.clone();
        let fork_source = self.fork_source.clone();
        let include_changes = self.fork_include_changes;
        let queue = self.fork_queue;
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                if let Some(source) = fork_source {
                    let value = client.call("fork_worker", json!({"worker_id":source,"title":title,"prompt":if prompt.is_empty() { None } else { Some(prompt) },"agent":agent,"include_uncommitted":include_changes,"queue":queue}))?;
                    return Ok((None, value));
                }
                let project = client.call("add_project", json!({"path":path}))?;
                // Recheck the project's berths: the form snapshot may be up to two seconds old.
                let capacity: Capacity = match client.call("capacity", json!({})) {
                    Ok(value) => serde_json::from_value(value)?,
                    // Preserve the snapshot's fallback for older API-v1 daemons.
                    Err(error) if error.to_string() == "unknown method capacity" => last_capacity,
                    Err(error) => return Err(error),
                };
                let in_use = project["id"]
                    .as_str()
                    .and_then(|id| capacity.per_project.get(id))
                    .map_or(0, |project| project.in_use);
                if in_use >= capacity.max_workers {
                    anyhow::bail!("{}", full_capacity_message(capacity.max_workers));
                }
                let worker = client.call("spawn_worker", json!({"project_id":project["id"],"title":title,"agent":agent,"base":if base.is_empty() { None } else { Some(base) },"prompt":if prompt.is_empty() { None } else { Some(prompt) }})).map_err(|error| {
                    // Another client can take the final berth after the capacity check.
                    if error.to_string().contains("no free berth in this project") {
                        anyhow::anyhow!(full_capacity_message(capacity.max_workers))
                    } else {
                        error
                    }
                })?;
                Ok::<_, anyhow::Error>((serde_json::from_value::<Project>(project).ok(), worker))
            }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok((project, value)) => {
                        if let Some(project) = project {
                            this.selected_project = Some(project.id.clone());
                            if !this.projects.iter().any(|known| known.id == project.id) { this.projects.push(project); }
                        }
                        this.task_notice = (value["queued"] == true).then(|| format!("Fork queued. It will start when a berth opens. Task {}", value["id"].as_str().unwrap_or_default()));
                        if let Ok(worker) = serde_json::from_value::<Worker>(value) {
                            if !this.capacity.live.contains(&worker.id) { this.capacity.live.push(worker.id.clone()); }
                            this.workers.push(worker);
                        }
                        this.fork_source = None;
                        this.form_open = false; this.fields[1].clear(); this.fields[2].clear(); this.fields[3].clear();
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }).detach();
    }
    fn edit_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = &mut self.fields[self.active_field];
        if event.keystroke.key == "backspace" {
            field.pop();
        } else if event.keystroke.key == "tab" {
            self.active_field = if self.fork_source.is_some() {
                if self.active_field == 1 { 2 } else { 1 }
            } else {
                (self.active_field + 1) % 4
            };
        } else if event.keystroke.key == "v"
            && (event.keystroke.modifiers.platform || event.keystroke.modifiers.control)
        {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                field.push_str(&text.replace(['\r', '\n'], " "));
            }
        } else if let Some(text) = &event.keystroke.key_char
            && !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.platform
        {
            field.push_str(text);
        }
        cx.stop_propagation();
        cx.notify();
    }
    fn choose_repository(&mut self, cx: &mut Context<Self>) {
        if self.repo_picker_open || self.busy {
            return;
        }
        self.repo_picker_open = true;
        let selection = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose repository".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = selection.await;
            let _ = this.update(cx, |this, cx| {
                this.repo_picker_open = false;
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.first() {
                            if let Some(path) = path.to_str() {
                                this.fields[0] = path.to_owned();
                                this.active_field = 1;
                                this.error = None;
                            } else {
                                this.error = Some("Choose a repository with a UTF-8 path".into());
                            }
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        this.error = Some(format!("Could not open folder picker: {error}"))
                    }
                    Err(error) => {
                        this.error = Some(format!("Folder picker closed unexpectedly: {error}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn field(&self, index: usize, label: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        let text = if self.fields[index].is_empty() {
            label.into()
        } else {
            self.fields[index].clone()
        };
        div()
            .id(SharedString::from(format!("field-{index}")))
            .flex_1()
            .p_2()
            .rounded_md()
            .bg(rgb(if self.active_field == index {
                self.theme.selection
            } else {
                self.theme.card
            }))
            .when(index == 0, |field| field.cursor_pointer())
            .when(index != 0, |field| field.cursor_text())
            .child(text)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.active_field = index;
                this.form_focus.focus(window);
                if index == 0 {
                    this.choose_repository(cx);
                }
                cx.notify();
            }))
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        self.connection.store(false, Ordering::Relaxed);
    }
}
impl Workspace {
    fn error_banner(&self) -> Option<gpui::AnyElement> {
        self.error.as_ref().map(|error| {
            div()
                .p_3()
                .bg(rgb(self.theme.card))
                .text_color(rgb(self.theme.error))
                .border_l_4()
                .border_color(rgb(self.theme.error))
                .child(error.clone())
                .into_any_element()
        })
    }
    fn new_task_form(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut form = div()
            .track_focus(&self.form_focus)
            .border_1()
            .border_color(rgb(self.theme.border))
            .focus(|style| style.border_color(rgb(self.theme.focus)))
            .on_key_down(cx.listener(Self::edit_key))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .bg(rgb(self.theme.panel))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .when(self.fork_source.is_none(), |row| {
                        row.child(self.field(0, "Choose repository…", cx))
                    })
                    .child(self.field(1, "Task title", cx)),
            )
            .child(self.field(2, "Initial instruction (optional)", cx))
            .when(self.fork_source.is_none(), |form| {
                form.child(self.field(3, "Base ref override (optional)", cx))
            });
        if let Some(source) = &self.fork_source {
            form = form.child(div().text_sm().child(self.fork_lineage(source)));
            for (key, label, checked) in [
                (
                    "fork-local",
                    "Include uncommitted changes",
                    self.fork_include_changes,
                ),
                ("fork-queue", "Queue if no berth is free", self.fork_queue),
            ] {
                form = form.child(
                    div()
                        .id(key)
                        .cursor_pointer()
                        .p_2()
                        .bg(rgb(self.theme.button))
                        .child(format!("{} {label}", if checked { "☑" } else { "☐" }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if key == "fork-local" {
                                this.fork_include_changes = !this.fork_include_changes;
                            } else {
                                this.fork_queue = !this.fork_queue;
                            }
                            cx.notify();
                        })),
                );
            }
        }
        let mut agents = div().flex().gap_2();
        for name in ["claude", "codex", "gemini", "opencode", "aider", "shell"] {
            agents = agents.child(
                div()
                    .id(SharedString::from(format!("agent-{name}")))
                    .cursor_pointer()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(if self.agent == name {
                        self.theme.accent
                    } else {
                        self.theme.button
                    }))
                    .when(self.agent == name, |button| {
                        button.text_color(rgb(self.theme.base))
                    })
                    .child(name)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.agent = name.into();
                        cx.notify();
                    })),
            );
        }
        let blocked = self.creation_blocked_reason();
        let disabled = self.busy || blocked.is_some();
        if let Some(reason) = &blocked {
            form = form.child(
                div()
                    .text_sm()
                    .text_color(rgb(self.theme.muted))
                    .child(reason.clone()),
            );
        }
        form = form.child(agents).child(
            div()
                .id("spawn")
                .when(!disabled, |button| button.cursor_pointer())
                .when(disabled, |button| button.opacity(0.5))
                .p_2()
                .rounded_md()
                .bg(rgb(self.theme.accent))
                .text_color(rgb(self.theme.base))
                .child(if self.busy {
                    "Creating…"
                } else if blocked.is_some() {
                    "Creation unavailable"
                } else if self.fork_source.is_some() {
                    "Fork worker"
                } else {
                    "Create isolated worker"
                })
                .when(!disabled, |button| {
                    button.on_click(cx.listener(|this, _, _, cx| this.create_worker(cx)))
                }),
        );
        form.into_any_element()
    }
    /// The berths grid for all projects or the selected one.
    fn berths_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut content = div()
            .id("workspace-content")
            .overflow_y_scroll()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .gap_5()
            .px_8()
            .py_6()
            .child(self.header(cx))
            .children(self.error_banner())
            .when_some(self.task_notice.clone(), |content, notice| {
                content.child(
                    div()
                        .text_sm()
                        .text_color(rgb(self.theme.muted))
                        .child(notice),
                )
            });
        if let Some(update) = &self.available_update {
            content = content.child(self.update_notice(update, cx));
        }
        if self.recovery_open {
            content = content.child(self.recovery_panel(cx));
        }
        if self.form_open {
            content = content.child(self.new_task_form(cx));
        }
        if self.checks.worker.is_some() {
            content = content.child(self.checks_panel(cx));
        }
        content
            .child(self.agent_list(cx))
            .child(self.footer())
            .into_any_element()
    }
}
impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prepare_berth_focus(cx);
        let sidebar = self.sidebar(cx);
        let main = if self.settings_open {
            self.settings_page(cx)
        } else if self.terminal.is_some() {
            self.agent_view(cx)
        } else if self.view == View::Inbox {
            self.inbox_view(cx)
        } else {
            self.berths_view(cx)
        };
        div()
            .id("workspace")
            .track_focus(&self.workspace_focus)
            .size_full()
            .relative()
            .capture_key_down(cx.listener(Self::workspace_key))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.menu.is_some() {
                        this.menu = None;
                        cx.notify();
                    }
                }),
            )
            .flex()
            .bg(rgb(self.theme.base))
            .text_color(rgb(self.theme.text))
            .font_family(".SystemUIFont")
            .child(sidebar)
            .child(main)
    }
}
fn main() -> Result<()> {
    let args = Args::parse();
    let client = Client {
        socket: args.socket,
    };
    let startup_error = if args.no_daemon {
        None
    } else {
        bootstrap::ensure_bundled_daemon(&client, &std::env::current_exe()?)
            .err()
            .map(|error| error.to_string())
    };
    let app = Application::new().with_assets(icons::Assets);
    app.run(move |cx: &mut App| {
        sigmadock_terminal::register_fonts(cx);
        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("SigmaDock".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let mut workspace = Workspace::new(client, window, cx);
                    if startup_error.is_some() {
                        workspace.error = startup_error;
                    }
                    workspace
                })
            },
        )
        .expect("open SigmaDock window");
        cx.activate(true);
    });
    Ok(())
}

#[cfg(test)]
mod reader_tests {
    use super::*;
    use sigmadock_core::{DaemonEvent, OutputSignal};
    use std::{io::BufReader, os::unix::net::UnixListener, sync::atomic::AtomicUsize};
    #[test]
    fn terminal_fetches_after_output_events_and_does_not_poll_while_idle() {
        let path =
            std::env::temp_dir().join(format!("sigmadock-reader-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let server = std::thread::spawn(move || {
            for (cursor, bytes) in [(5, b"hello"), (10, b"world")] {
                let (mut socket, _) = listener.accept().unwrap();
                let request: serde_json::Value = serde_json::from_slice(
                    &sigmadock_core::read_frame(&mut BufReader::new(socket.try_clone().unwrap()))
                        .unwrap()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(request["method"], "output");
                count.fetch_add(1, Ordering::Relaxed);
                writeln!(socket, "{}", json!({"jsonrpc":"2.0","id":1,"result":{"cursor":cursor,"bytes":bytes.to_vec(),"exited":false,"truncated":false,"cols":80,"rows":24}})).unwrap();
            }
        });
        let feed = events::EventFeed::default();
        feed.emit_for_test(DaemonEvent::Resync);
        let mut signal = OutputSignal {
            cursor: 5,
            cols: 80,
            rows: 24,
            generation: 1,
            exited: false,
        };
        feed.emit_for_test(DaemonEvent::OutputAvailable {
            worker_id: "w".into(),
            signal,
        });
        let mut reader = RemoteReader {
            script: None,
            client: Client {
                socket: path.clone(),
            },
            worker: "w".into(),
            cursor: 0,
            pending: vec![],
            offset: 0,
            ended: false,
            connected: Arc::new(AtomicBool::new(true)),
            events: feed.clone(),
            observed: None,
            more: false,
        };
        let mut bytes = [0; 5];
        assert_eq!(reader.read(&mut bytes).unwrap(), 5);
        assert_eq!(&bytes, b"hello");
        let waiter = std::thread::spawn(move || {
            let mut bytes = [0; 5];
            assert_eq!(reader.read(&mut bytes).unwrap(), 5);
            bytes
        });
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        signal.cursor = 10;
        feed.emit_for_test(DaemonEvent::OutputAvailable {
            worker_id: "w".into(),
            signal,
        });
        assert_eq!(&waiter.join().unwrap(), b"world");
        server.join().unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        std::fs::remove_file(path).unwrap();
    }
}
