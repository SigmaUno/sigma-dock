//! Native board and reconnectable terminal, backed by the daemon's PTYs.
mod appearance_ui;
mod bootstrap;
mod ci_ui;
mod preferences;
mod recovery_ui;
mod theme;
mod update_ui;
mod updates;

use anyhow::Result;
use clap::Parser;
use gpui::{
    App, Application, Bounds, Context, Entity, SharedString, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, rgb, size,
};
use serde_json::json;
use sigma_dock_core::{Client, Column, Output, Worker, column, socket_path};
use sigma_dock_terminal::{TerminalConfig, TerminalView};
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
struct Workspace {
    client: Client,
    workers: Vec<Worker>,
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
    ci_open: bool,
    ci_report: Option<sigma_dock_core::CiPreview>,
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
        let initial = client.workers();
        let (workers, error) = match initial {
            Ok(workers) => (workers, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        let poll_client = client.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let client = poll_client.clone();
                let result =
                    cx.background_executor()
                        .spawn(async move {
                            (client.workers(), client.call("list_unfinished", json!({})))
                        })
                        .await;
                if this
                    .update(cx, |this, cx| {
                        let (result, recovery) = result;
                        match recovery {
                            Ok(value) => {
                                this.recovery_entries =
                                    serde_json::from_value(value).unwrap_or_default();
                                this.recovery_error = None;
                            }
                            Err(error) => this.recovery_error = Some(error.to_string()),
                        }
                        match result {
                            Ok(workers) => {
                                this.workers = workers;
                                // Preserve action errors until the next explicit action.
                            }
                            Err(error) => {
                                this.error = Some(error.to_string());
                            }
                        }
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
        let preferences_path = sigma_dock_core::state_dir().join("preferences.json");
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
        Self {
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
            workers,
            error,
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
        }
    }
    fn open_worker(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.details.clear();
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
    fn create_worker(&mut self, cx: &mut Context<Self>) {
        if self.busy {
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
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                let project = client.call("add_project", json!({"path":path}))?;
                client.call("spawn_worker", json!({"project_id":project["id"],"title":title,"agent":agent,"prompt":if prompt.is_empty() { None } else { Some(prompt) }}))
            }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(value) => {
                        if let Ok(worker) = serde_json::from_value::<Worker>(value) { this.workers.push(worker); }
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
        let mut sidebar = div()
            .w(px(220.))
            .h_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(rgb(self.theme.sidebar))
            .child(
                div()
                    .text_xl()
                    .text_color(rgb(self.theme.accent))
                    .child("SigmaDock"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(self.theme.muted))
                    .child("LOCAL WORKSPACE"),
            );
        for worker in &self.workers {
            let id = worker.id.clone();
            sidebar = sidebar.child(
                div()
                    .id(SharedString::from(format!("side-{id}")))
                    .cursor_pointer()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(self.theme.card))
                    .child(worker.title.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_worker(id.clone(), window, cx)
                    })),
            );
        }
        sidebar = sidebar.child(
            div()
                .id("new-worker")
                .cursor_pointer()
                .p_3()
                .rounded_md()
                .bg(rgb(self.theme.accent))
                .text_color(rgb(self.theme.base))
                .child("+ New task")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.form_open = !this.form_open;
                    this.active_field = 0;
                    this.form_focus.focus(window);
                    cx.notify();
                })),
        );
        sidebar = sidebar.child(
            div()
                .id("show-unfinished")
                .p_2()
                .rounded_md()
                .bg(rgb(self.theme.button))
                .cursor_pointer()
                .child("Unfinished sessions")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.recovery_open = !this.recovery_open;
                    cx.notify();
                })),
        );
        let mut content = div()
            .id("workspace-content")
            .overflow_y_scroll()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_5();
        content = content.child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .child(div().text_xl().child("Workspace board"))
                .child(
                    div()
                        .id("terminal-settings")
                        .tab_index(0)
                        .border_1()
                        .border_color(rgb(self.theme.border))
                        .focus(|style| style.border_color(rgb(self.theme.focus)))
                        .p_2()
                        .rounded_md()
                        .bg(rgb(self.theme.button))
                        .hover(|style| style.bg(rgb(self.theme.selection)))
                        .cursor_pointer()
                        .child("⚙ Settings")
                        .tooltip(|_, cx| cx.new(|_| appearance_ui::SettingsTooltip).into())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.toggle_settings(window, cx)),
                        )
                        .on_key_down(cx.listener(
                            |this, event: &gpui::KeyDownEvent, window, cx| {
                                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                    this.toggle_settings(window, cx);
                                    cx.stop_propagation();
                                }
                            },
                        )),
                ),
        );
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
            form = form.child(agents).child(
                div()
                    .id("spawn")
                    .cursor_pointer()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(self.theme.accent))
                    .text_color(rgb(self.theme.base))
                    .child(if self.busy {
                        "Creating…"
                    } else {
                        "Create isolated worker"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.create_worker(cx))),
            );
            content = content.child(form);
        }
        let mut board = div().flex().gap_3();
        for status in Column::ALL {
            let status_color = match status {
                Column::Working => self.theme.link,
                Column::NeedsYou => self.theme.error,
                Column::InReview => self.theme.warning,
                Column::ReadyToMerge => self.theme.success,
            };
            let mut lane = div()
                .flex_1()
                .min_h(px(170.))
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded_lg()
                .bg(rgb(self.theme.panel))
                .border_t_2()
                .border_color(rgb(status_color))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(self.theme.muted))
                        .child(status.label()),
                );
            for worker in self
                .workers
                .iter()
                .filter(|worker| column(&worker.facts) == status)
            {
                let id = worker.id.clone();
                lane = lane.child(
                    div()
                        .id(SharedString::from(format!("card-{id}")))
                        .cursor_pointer()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(self.theme.button))
                        .hover(|style| style.bg(rgb(self.theme.selection)))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(worker.title.clone())
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(self.theme.muted))
                                .child(format!("{} · :{}", worker.agent, worker.port)),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_worker(id.clone(), window, cx)
                        })),
                );
            }
            board = board.child(lane);
        }
        content = content.child(board);
        if let Some(terminal) = &self.terminal {
            let selected = self
                .workers
                .iter()
                .find(|w| Some(&w.id) == self.selected.as_ref())
                .cloned();
            if let Some(worker) = selected {
                let id = worker.id.clone();
                let mut actions = div().flex().gap_2().items_center().child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(rgb(self.theme.muted))
                        .child(format!(
                            "{} · {:?} · checks {:?} · review {:?}",
                            worker.title,
                            worker.facts.session,
                            worker.facts.checks,
                            worker.facts.review
                        )),
                );
                for (label, method) in [
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
                                if method == "ci_feedback" {
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
            content = content.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(self.theme.muted))
                    .child("Select a worker to connect to its terminal"),
            );
        }
        div()
            .size_full()
            .relative()
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == ","
                    && (event.keystroke.modifiers.platform || event.keystroke.modifiers.control)
                {
                    this.toggle_settings(window, cx);
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
    Application::new().run(move |cx: &mut App| {
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
