//! Native board and reconnectable terminal, backed by the daemon's PTYs.
mod bootstrap;

use anyhow::Result;
use clap::Parser;
use gpui::{
    App, Application, Bounds, Context, Entity, SharedString, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, rgb, size,
};
use gpui_terminal::{TerminalConfig, TerminalView};
use serde_json::json;
use sigma_dock_core::{Client, Column, Output, Worker, column, socket_path};
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
    details: String,
    connection: Arc<AtomicBool>,
}
impl Workspace {
    fn new(client: Client, cx: &mut Context<Self>) -> Self {
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
                let result = cx
                    .background_executor()
                    .spawn(async move { client.workers() })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        match result {
                            Ok(workers) => {
                                this.workers = workers;
                                // Preserve action errors until the next explicit action.
                            }
                            Err(error) => {
                                this.error = Some(error.to_string());
                            }
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
        Self {
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
            details: String::new(),
            connection: Arc::new(AtomicBool::new(false)),
        }
    }
    fn open_worker(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
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
            TerminalView::new(writer, reader, TerminalConfig::default(), cx).with_resize_callback(
                move |cols, rows| {
                    let _ = resize_client.call(
                        "resize",
                        json!({"worker_id":resize_worker,"cols":cols,"rows":rows}),
                    );
                },
            )
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
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { client.call(method, params) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(value) => {
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
                0x2c4058
            } else {
                0x1d2b3e
            }))
            .cursor_text()
            .child(text)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.active_field = index;
                this.form_focus.focus(window);
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
            .bg(rgb(0x121925))
            .child(div().text_xl().text_color(rgb(0x69e2bd)).child("SigmaDock"))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x8b99ad))
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
                    .bg(rgb(0x1b2535))
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
                .bg(rgb(0x275f54))
                .child("+ New task")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.form_open = !this.form_open;
                    this.active_field = 0;
                    this.form_focus.focus(window);
                    cx.notify();
                })),
        );
        let mut content = div().flex_1().h_full().flex().flex_col().gap_4().p_5();
        content = content.child(
            div()
                .flex()
                .justify_between()
                .child(div().text_xl().child("Workspace board"))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x69e2bd))
                        .child("LOCAL · NO ANALYTICS"),
                ),
        );
        if let Some(error) = &self.error {
            content = content.child(div().p_3().bg(rgb(0x502634)).child(error.clone()));
        }
        if self.form_open {
            let mut form = div()
                .track_focus(&self.form_focus)
                .on_key_down(cx.listener(Self::edit_key))
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded_lg()
                .bg(rgb(0x172031))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(self.field(0, "Repository path", cx))
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
                            0x275f54
                        } else {
                            0x243248
                        }))
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
                    .bg(rgb(0x275f54))
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
            let mut lane = div()
                .flex_1()
                .min_h(px(170.))
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded_lg()
                .bg(rgb(0x172031))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xa6b4c8))
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
                        .bg(rgb(0x243248))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(worker.title.clone())
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(0x90a5bf))
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
                        .text_color(rgb(0x8b99ad))
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
                            .bg(rgb(0x243248))
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.run_action(method, json!({"worker_id":id}), cx)
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
                            .bg(rgb(0x275f54))
                            .child("Open PR")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    );
                }
                content = content.child(actions);
            }
            if !self.details.is_empty() {
                content = content.child(
                    div()
                        .max_h(px(80.))
                        .overflow_hidden()
                        .text_sm()
                        .text_color(rgb(0x8b99ad))
                        .child(self.details.clone()),
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
                    .text_color(rgb(0x8b99ad))
                    .child("Select a worker to connect to its terminal"),
            );
        }
        div()
            .size_full()
            .flex()
            .bg(rgb(0x0d1420))
            .text_color(rgb(0xe1e8f2))
            .font_family(".SystemUIFont")
            .child(sidebar)
            .child(content)
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
            |_, cx| {
                cx.new(|cx| {
                    let mut workspace = Workspace::new(client, cx);
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
