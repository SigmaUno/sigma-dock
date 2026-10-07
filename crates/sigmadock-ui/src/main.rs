//! Native board and reconnectable terminal, backed by the daemon's PTYs.
mod appearance_ui;
mod berths_ui;
mod bootstrap;
mod ci_ui;
mod icons;
mod preferences;
mod recovery_ui;
mod theme;
mod update_ui;
mod updates;
mod usage_ui;

use anyhow::Result;
use clap::Parser;
use gpui::{
    App, Application, Bounds, Context, Entity, SharedString, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, rgb, size,
};
use icons::{Icon, icon};
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
    thread,
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
    client: Client,
    worker: String,
}
impl Write for RemoteWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.client
            .call("input", json!({"worker_id":self.worker,"bytes":bytes}))
            .map_err(io::Error::other)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct RemoteReader {
    client: Client,
    worker: String,
    cursor: u64,
    pending: Vec<u8>,
    offset: usize,
    ended: bool,
    connected: Arc<AtomicBool>,
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
            let output: Output = serde_json::from_value(
                self.client
                    .call(
                        "output",
                        json!({"worker_id":self.worker,"cursor":self.cursor}),
                    )
                    .map_err(io::Error::other)?,
            )
            .map_err(io::Error::other)?;
            self.cursor = output.cursor;
            self.ended = output.exited;
            self.pending = output.bytes;
            self.offset = 0;
            if output.truncated {
                self.pending.splice(0..0, b"\x1bc".iter().copied());
            }
            if self.pending.is_empty() && !self.ended {
                thread::sleep(Duration::from_millis(25));
            }
        }
    }
}
fn full_capacity_message(max_workers: usize) -> String {
    format!(
        "All {max_workers} berths are in use. Wait for a session to finish or stop a worker before creating this task. Automatic queuing is not available yet."
    )
}

struct Workspace {
    client: Client,
    workers: Vec<Worker>,
    /// Archived workers, for the "Departed today" list.
    departed: Vec<Worker>,
    capacity: Capacity,
    previews: std::collections::HashMap<String, berths_ui::Preview>,
    /// Berth highlighted from the needs-you strip or the last closed terminal.
    focused_berth: Option<String>,
    projects: Vec<Project>,
    /// `None` shows all berths.
    selected_project: Option<String>,
    daemon_connected: bool,
    error: Option<String>,
    terminal: Option<Entity<TerminalView>>,
    selected: Option<String>,
    form_open: bool,
    fields: [String; 4],
    active_field: usize,
    agent: String,
    form_focus: gpui::FocusHandle,
    busy: bool,
    repo_picker_open: bool,
    details: String,
    connection: Arc<AtomicBool>,
    preferences: preferences::Preferences,
    preferences_path: PathBuf,
    settings_open: bool,
    settings_focus: gpui::FocusHandle,
    settings_editor: Option<(usize, String)>,
    settings_error: Option<String>,
    usage_open: bool,
    usage_report: Option<sigmadock_core::AgentUsage>,
    usage_loading: bool,
    usage_error: Option<String>,
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
}
impl Workspace {
    fn new(client: Client, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let initial = berths_ui::Snapshot::load(&client);
        let poll_client = client.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let client = poll_client.clone();
                let snapshot = cx
                    .background_executor()
                    .spawn(async move { berths_ui::Snapshot::load(&client) })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.apply_snapshot(snapshot);
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
        let mut workspace = Self {
            usage_open: false,
            usage_report: None,
            usage_loading: false,
            usage_error: None,
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
            available_update: None,
            preferences: loaded.unwrap_or_default(),
            preferences_path,
            settings_open: false,
            settings_focus: cx.focus_handle(),
            settings_editor: None,
            settings_error,
            client,
            workers: Vec::new(),
            departed: Vec::new(),
            capacity: Capacity::default(),
            previews: Default::default(),
            focused_berth: None,
            projects: Vec::new(),
            selected_project: None,
            daemon_connected: false,
            error: None,
            terminal: None,
            selected: None,
            form_open: false,
            fields: [String::new(), String::new(), String::new(), String::new()],
            active_field: 0,
            agent: "claude".into(),
            form_focus: cx.focus_handle(),
            busy: false,
            repo_picker_open: false,
            details: String::new(),
            connection: Arc::new(AtomicBool::new(false)),
        };
        workspace.apply_snapshot(initial);
        workspace.spawn_preview_loop(cx);
        workspace
    }
    fn open_worker(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.focused_berth = Some(id.clone());
        self.details.clear();
        self.usage_open = false;
        self.usage_report = None;
        self.usage_error = None;
        self.ci_open = false;
        self.ci_report = None;
        self.ci_error = None;
        self.ci_expanded = None;
        self.connection.store(false, Ordering::Relaxed);
        self.connection = Arc::new(AtomicBool::new(true));
        let reader = RemoteReader {
            client: self.client.clone(),
            worker: id.clone(),
            cursor: 0,
            pending: Vec::new(),
            offset: 0,
            ended: false,
            connected: self.connection.clone(),
        };
        let writer = RemoteWriter {
            client: self.client.clone(),
            worker: id.clone(),
        };
        let resize_client = self.client.clone();
        let resize_worker = id.clone();
        let terminal = cx.new(|cx| {
            TerminalView::new(
                writer,
                reader,
                self.theme
                    .terminal(&self.preferences.appearance)
                    .apply(TerminalConfig::default()),
                cx,
            )
            .with_resize_callback(move |cols, rows| {
                let _ = resize_client.call(
                    "resize",
                    json!({"worker_id":resize_worker,"cols":cols,"rows":rows}),
                );
            })
        });
        terminal.read(cx).focus_handle().focus(window);
        self.terminal = Some(terminal);
        self.selected = Some(id.clone());
        self.run_action("diff", json!({"worker_id":id}), cx);
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
                        if let Some(text) = value
                            .as_str()
                            .or_else(|| value.get("text").and_then(serde_json::Value::as_str))
                        {
                            this.details = text.into();
                        }
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn creation_blocked_reason(&self) -> Option<String> {
        if !self.daemon_connected {
            Some("Connect to the daemon before creating a task.".into())
        } else if self.capacity.live.len() >= self.capacity.max_workers {
            Some(full_capacity_message(self.capacity.max_workers))
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
        self.busy = true;
        self.error = None;
        let client = self.client.clone();
        let path = self.fields[0].clone();
        let title = self.fields[1].clone();
        let prompt = self.fields[2].clone();
        let agent = self.agent.clone();
        let last_capacity = self.capacity.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                // Recheck global capacity: the form snapshot may be up to two seconds old.
                let capacity: Capacity = match client.call("capacity", json!({})) {
                    Ok(value) => serde_json::from_value(value)?,
                    // Preserve the snapshot's fallback for older API-v1 daemons.
                    Err(error) if error.to_string() == "unknown method capacity" => last_capacity,
                    Err(error) => return Err(error),
                };
                if capacity.live.len() >= capacity.max_workers {
                    anyhow::bail!("{}", full_capacity_message(capacity.max_workers));
                }
                let project = client.call("add_project", json!({"path":path}))?;
                let worker = client.call("spawn_worker", json!({"project_id":project["id"],"title":title,"agent":agent,"prompt":if prompt.is_empty() { None } else { Some(prompt) }})).map_err(|error| {
                    // Another client can take the final berth after the capacity check.
                    if error.to_string().contains("maximum concurrent workers reached") {
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
                        if let Ok(worker) = serde_json::from_value::<Worker>(value) {
                            if !this.capacity.live.contains(&worker.id) { this.capacity.live.push(worker.id.clone()); }
                            this.workers.push(worker);
                        }
                        this.form_open = false; this.fields[1].clear(); this.fields[2].clear();
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
            self.active_field = (self.active_field + 1) % 3;
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
impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar = self.sidebar(cx);
        let mut content = div()
            .id("workspace-content")
            .overflow_y_scroll()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .gap_5()
            .px_8()
            .py_6();
        let selected = self
            .workers
            .iter()
            .find(|w| Some(&w.id) == self.selected.as_ref())
            .cloned();
        if self.terminal.is_none() {
            content = content.child(self.header(cx));
        } else {
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .id("close-terminal")
                            .cursor_pointer()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .bg(rgb(self.theme.button))
                            .hover(|style| style.bg(rgb(self.theme.selection)))
                            .child(icon(Icon::ArrowLeft, px(14.), rgb(self.theme.text)))
                            .child("Berths")
                            .on_click(cx.listener(|this, _, _, cx| this.close_terminal(cx))),
                    )
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(selected.as_ref().map_or_else(
                                || "Session ended".to_owned(),
                                |worker| worker.title.clone(),
                            )),
                    ),
            );
        }
        if let Some(error) = &self.error {
            content = content.child(
                div()
                    .p_3()
                    .bg(rgb(self.theme.card))
                    .text_color(rgb(self.theme.error))
                    .border_l_4()
                    .border_color(rgb(self.theme.error))
                    .child(error.clone()),
            );
        }
        if let Some(update) = &self.available_update {
            content = content.child(self.update_notice(update, cx));
        }
        if self.recovery_open {
            content = content.child(self.recovery_panel(cx));
        }
        if self.form_open {
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
                        .child(self.field(0, "Choose repository…", cx))
                        .child(self.field(1, "Task title", cx)),
                )
                .child(self.field(2, "Initial instruction (optional)", cx));
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
                    } else {
                        "Create isolated worker"
                    })
                    .when(!disabled, |button| {
                        button.on_click(cx.listener(|this, _, _, cx| this.create_worker(cx)))
                    }),
            );
            content = content.child(form);
        }
        if let Some(terminal) = &self.terminal {
            if let Some(worker) = selected {
                let id = worker.id.clone();
                let mut actions = div().flex().gap_2().items_center().child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(rgb(self.theme.muted))
                        .child(format!(
                            "{} · {} · :{} · checks {:?} · review {:?}",
                            berths_ui::agent_label(&worker.agent),
                            worker.branch,
                            worker.port,
                            worker.facts.checks,
                            worker.facts.review
                        )),
                );
                for (label, method) in [
                    ("Usage", "agent_usage"),
                    ("Diff", "diff"),
                    ("CI preview", "ci_feedback"),
                    ("Send CI", "send_ci_feedback"),
                    ("Review", "review_feedback"),
                    ("Conflict plan", "conflict_instruction"),
                    ("Stop", "stop_worker"),
                    ("Resume", "resume_worker"),
                    ("Archive", "archive_worker"),
                ] {
                    let id = id.clone();
                    actions = actions.child(
                        div()
                            .id(SharedString::from(format!("action-{method}")))
                            .cursor_pointer()
                            .p_2()
                            .rounded_md()
                            .bg(rgb(self.theme.button))
                            .hover(|style| style.bg(rgb(self.theme.selection)))
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if method == "agent_usage" {
                                    this.load_usage(cx);
                                } else if method == "ci_feedback" {
                                    this.load_ci(cx);
                                } else {
                                    this.run_action(method, json!({"worker_id":id}), cx)
                                }
                            })),
                    );
                }
                if let Some(url) = worker.facts.pr_url {
                    actions = actions.child(
                        div()
                            .id("open-pr")
                            .cursor_pointer()
                            .p_2()
                            .rounded_md()
                            .bg(rgb(self.theme.accent))
                            .text_color(rgb(self.theme.base))
                            .child("Open PR")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    );
                }
                content = content.child(actions);
            }
            if self.usage_open {
                content = content.child(self.usage_panel(cx));
            }
            if self.ci_open {
                content = content.child(self.ci_panel(cx));
            }
            if !self.details.is_empty() {
                content = content.child(
                    div()
                        .id("feedback-detail")
                        .max_h(px(120.))
                        .overflow_y_scroll()
                        .text_sm()
                        .text_color(rgb(self.theme.muted))
                        .child(self.details.clone())
                        .child(
                            div()
                                .id("copy-feedback-detail")
                                .cursor_pointer()
                                .child("Copy feedback")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        this.details.clone(),
                                    ))
                                })),
                        ),
                );
            }
            content = content.child(div().flex_1().min_h(px(200.)).child(terminal.clone()));
        } else {
            content = content.child(self.needs_strip(cx)).child(
                div()
                    .flex()
                    .items_start()
                    .gap_6()
                    .child(div().flex_1().min_w(px(0.)).child(self.grid(cx)))
                    .child(self.side_panel(cx)),
            );
        }
        content = content.child(self.footer());
        div()
            .size_full()
            .relative()
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == ","
                    && (event.keystroke.modifiers.platform || event.keystroke.modifiers.control)
                {
                    this.toggle_settings(window, cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "n" && event.keystroke.modifiers.platform {
                    if !this.form_open {
                        this.open_new_task(window, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .flex()
            .bg(rgb(self.theme.base))
            .text_color(rgb(self.theme.text))
            .font_family(".SystemUIFont")
            .child(sidebar)
            .child(content)
            .when(self.settings_open, |root| {
                root.child(self.settings_panel(cx))
            })
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
