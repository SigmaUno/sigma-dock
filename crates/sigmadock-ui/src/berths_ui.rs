//! Berths: one fixed slot per live session, a project sidebar and a needs-you strip.
use crate::Workspace;
use crate::icons::{Icon, icon};
use anyhow::Result;
use gpui::{
    BoxShadow, Context, FontWeight, SharedString, Window, div, point, prelude::*, px, rgb, rgba,
};
use serde_json::json;
use sigmadock_core::{
    Capacity, Checks, Client, Output, Project, PullRequestState, Review, SessionState,
    Status as DerivedStatus, Worker, status as derived_status, unix_time,
};
use sigmadock_terminal::{GpuiEventProxy, TerminalState};
use std::time::Duration;

const MONO: &str = sigmadock_terminal::DEFAULT_MONOSPACE_FONT;
/// Matches the daemon's initial PTY size so previews lay out like the session.
const PREVIEW_COLS: usize = 120;
const PREVIEW_ROWS: usize = 30;
const PREVIEW_LINES: usize = 6;
const PREVIEW_INTERVAL: Duration = Duration::from_millis(750);
/// The daemon returns at most this many bytes per `output` call.
const OUTPUT_CHUNK: usize = 64 * 1024;

pub(crate) fn agent_label(agent: &str) -> &str {
    match agent {
        "claude" => "Claude Code",
        "codex" => "Codex",
        "gemini" => "Gemini CLI",
        "opencode" => "OpenCode",
        "aider" => "Aider",
        "shell" => "Shell",
        other => other,
    }
}

pub(crate) fn relative_time(at: u64, now: u64) -> String {
    let elapsed = now.saturating_sub(at);
    match elapsed {
        0..60 => "Just now".into(),
        60..3600 => format!("{}m ago", elapsed / 60),
        3600..86400 => format!("{}h ago", elapsed / 3600),
        _ => format!("{}d ago", elapsed / 86400),
    }
}

/// Start of the local calendar day containing `now`.
pub(crate) fn local_midnight(now: u64) -> u64 {
    let time = now as libc::time_t;
    // SAFETY: localtime_r only writes the provided, zero-initialised tm.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return now - now % 86_400;
    }
    now.saturating_sub((tm.tm_hour * 3600 + tm.tm_min * 60 + tm.tm_sec) as u64)
}

/// Numbered berth slots up to the larger of capacity and the highest docked berth.
fn slot_layout(berths: Vec<&Worker>, max_workers: usize) -> Vec<(usize, Option<&Worker>)> {
    let max_slot = berths
        .iter()
        .filter_map(|worker| worker.berth)
        .map(usize::from)
        .max()
        .unwrap_or(0)
        .max(max_workers);
    (1..=max_slot)
        .map(|number| {
            let worker = berths
                .iter()
                .find(|worker| worker.berth == Some(number as u8))
                .copied();
            (number, worker)
        })
        .collect()
}

fn pr_number(worker: &Worker) -> Option<&str> {
    worker
        .facts
        .pr_url
        .as_deref()
        .and_then(|url| url.trim_end_matches('/').rsplit('/').next())
        .filter(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Working,
    Input,
    Blocked,
    Review,
    Ready,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Reply,
    SendCi,
    OpenPr(String),
}

/// Presentation of the daemon-derived status; no new status rules live here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub pill: String,
    pub tone: Tone,
    pub action: Option<Action>,
}

pub(crate) fn status(worker: &Worker) -> Status {
    let facts = &worker.facts;
    let (pill, tone, action): (String, _, _) = match derived_status(facts) {
        DerivedStatus::NeedsYou => match () {
            _ if facts.session == SessionState::NeedsInput => {
                ("Needs input".into(), Tone::Input, Some(Action::Reply))
            }
            _ if facts.checks == Checks::Failed => {
                ("CI failed".into(), Tone::Blocked, Some(Action::SendCi))
            }
            _ if facts.review == Review::ChangesRequested => {
                ("Changes requested".into(), Tone::Blocked, None)
            }
            _ if facts.mergeable == Some(false) => ("Conflict".into(), Tone::Blocked, None),
            _ if facts.session == SessionState::Lost => {
                ("Session lost".into(), Tone::Blocked, None)
            }
            _ if facts.forge_error.is_some() => ("Forge error".into(), Tone::Blocked, None),
            _ if facts.pr == PullRequestState::Closed => ("PR closed".into(), Tone::Blocked, None),
            _ => (
                facts
                    .exit_code
                    .map_or_else(|| "Needs you".into(), |code| format!("Exited {code}")),
                Tone::Blocked,
                None,
            ),
        },
        DerivedStatus::InReview => ("In review".into(), Tone::Review, None),
        DerivedStatus::ReadyToMerge if facts.pr == PullRequestState::Merged => {
            ("Merged".into(), Tone::Ready, None)
        }
        DerivedStatus::ReadyToMerge => (
            "Ready to merge".into(),
            Tone::Ready,
            facts.pr_url.clone().map(Action::OpenPr),
        ),
        DerivedStatus::Working => (
            match facts.session {
                SessionState::Idle => "Idle",
                SessionState::Exited => "Exited",
                _ => "Working",
            }
            .into(),
            Tone::Working,
            None,
        ),
    };
    Status { pill, tone, action }
}

/// Headless screen for one berth, fed incrementally from the daemon's replay buffer.
pub(crate) struct Preview {
    screen: TerminalState,
    cursor: u64,
    generation: u64,
    geometry: (usize, usize),
    pub lines: Vec<String>,
}

impl Preview {
    fn new() -> Self {
        let (events, _) = std::sync::mpsc::channel();
        Self {
            screen: TerminalState::new(PREVIEW_COLS, PREVIEW_ROWS, GpuiEventProxy::new(events)),
            cursor: 0,
            generation: 0,
            geometry: (PREVIEW_COLS, PREVIEW_ROWS),
            lines: Vec::new(),
        }
    }
    fn feed(&mut self, output: &Output) -> bool {
        self.cursor = output.cursor;
        // Keep the last known geometry when talking to an older daemon.
        let geometry = match (output.cols, output.rows) {
            (Some(cols @ 1..=1000), Some(rows @ 1..=1000)) => (cols as usize, rows as usize),
            _ => self.geometry,
        };
        let resized = geometry != self.geometry;
        if resized {
            self.screen.resize(geometry.0, geometry.1);
            self.geometry = geometry;
        }
        if output.bytes.is_empty() && !resized && !output.truncated {
            return false;
        }
        if output.truncated {
            self.screen.process_bytes(b"\x1bc");
        }
        self.screen.process_bytes(&output.bytes);
        let mut lines: Vec<_> = self
            .screen
            .screen_text()
            .into_iter()
            .filter(|line| !line.trim().is_empty())
            .collect();
        let skip = lines.len().saturating_sub(PREVIEW_LINES);
        self.lines = lines.split_off(skip);
        true
    }
}

fn fetch_output(client: &Client, worker: &str, mut cursor: u64) -> Result<Vec<Output>> {
    let mut outputs = Vec::new();
    // Preserve each response's geometry instead of combining differently sized chunks.
    // Catch up without stalling a tick on a full 1 MiB replay.
    for _ in 0..24 {
        let output: Output = serde_json::from_value(
            client.call("output", json!({"worker_id": worker, "cursor": cursor}))?,
        )?;
        let read = output.bytes.len();
        cursor = output.cursor;
        outputs.push(output);
        if read < OUTPUT_CHUNK {
            break;
        }
    }
    Ok(outputs)
}

/// Refreshed together so one poll gives a consistent workspace.
pub(crate) struct Snapshot {
    pub workers: Result<Vec<Worker>>,
    pub recovery: Result<serde_json::Value>,
    pub projects: Result<Vec<Project>>,
    pub capacity: Result<Capacity>,
}

impl Snapshot {
    pub(crate) fn load(client: &Client) -> Self {
        if let Err(error) = client.check_version() {
            let message = error.to_string();
            return Self {
                workers: Err(anyhow::anyhow!(message.clone())),
                recovery: Err(anyhow::anyhow!(message.clone())),
                projects: Err(anyhow::anyhow!(message.clone())),
                capacity: Err(anyhow::anyhow!(message)),
            };
        }
        Self {
            workers: client
                .call("list_workers", json!({"include_archived": true}))
                .and_then(|value| Ok(serde_json::from_value(value)?)),
            recovery: client.call("list_unfinished", json!({})),
            projects: client
                .call("list_projects", json!({}))
                .and_then(|value| Ok(serde_json::from_value(value)?)),
            capacity: client
                .call("capacity", json!({}))
                .and_then(|value| Ok(serde_json::from_value(value)?)),
        }
    }
}

impl Workspace {
    pub(crate) fn apply_snapshot(&mut self, snapshot: Snapshot) {
        self.daemon_connected = snapshot.workers.is_ok();
        if let Ok(projects) = snapshot.projects {
            self.projects = projects;
        }
        match snapshot.recovery {
            Ok(value) => {
                self.recovery_entries = serde_json::from_value(value).unwrap_or_default();
                self.recovery_error = None;
            }
            Err(error) => self.recovery_error = Some(error.to_string()),
        }
        match snapshot.workers {
            Ok(workers) => {
                let (departed, workers) = workers.into_iter().partition(|worker| worker.archived);
                self.workers = workers;
                self.departed = departed;
                // Preserve action errors until the next explicit action.
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        self.capacity = match snapshot.capacity {
            Ok(capacity) => capacity,
            // A daemon without `capacity` predates berths; approximate from session facts.
            Err(_) if self.daemon_connected => {
                let live: Vec<_> = self
                    .workers
                    .iter()
                    .filter(|worker| worker.facts.session != SessionState::Exited)
                    .map(|worker| worker.id.clone())
                    .collect();
                Capacity {
                    max_workers: live.len().max(6),
                    live,
                    ..Default::default()
                }
            }
            Err(_) => std::mem::take(&mut self.capacity),
        };
        self.previews
            .retain(|id, _| self.capacity.live.contains(id));
    }

    pub(crate) fn spawn_preview_loop(&self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PREVIEW_INTERVAL).await;
                let Ok(targets) = this.update(cx, |this, _| {
                    this.capacity
                        .live
                        .iter()
                        .filter_map(|id| {
                            let (connected, dirty, signal) = this.events.feed.preview_request(id);
                            let preview = this.previews.get(id);
                            if connected && !dirty && preview.is_some() {
                                return None;
                            }
                            let generation = signal.map_or(0, |signal| signal.generation);
                            let cursor = preview
                                .filter(|preview| preview.generation == generation)
                                .map_or(0, |preview| preview.cursor);
                            Some((id.clone(), generation, cursor))
                        })
                        .collect::<Vec<_>>()
                }) else {
                    break;
                };
                if targets.is_empty() {
                    continue;
                }
                let client = client.clone();
                let Ok(feed) = this.update(cx, |this, _| this.events.feed.clone()) else {
                    break;
                };
                let updates = cx
                    .background_executor()
                    .spawn(async move {
                        targets
                            .into_iter()
                            .filter_map(|(id, generation, cursor)| {
                                match fetch_output(&client, &id, cursor) {
                                    Ok(out) => Some((id, generation, out)),
                                    Err(_) => {
                                        feed.retry_output(id);
                                        None
                                    }
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        let mut changed = false;
                        for (id, generation, outputs) in updates {
                            if !this.capacity.live.contains(&id) {
                                continue;
                            }
                            let preview = this.previews.entry(id).or_insert_with(Preview::new);
                            if preview.generation != generation {
                                *preview = Preview::new();
                                preview.generation = generation;
                            }
                            for output in outputs {
                                changed |= preview.feed(&output);
                            }
                        }
                        // Skip repaint while the full terminal covers the grid.
                        if changed && this.terminal.is_none() {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn current_project(&self) -> Option<&Project> {
        self.selected_project
            .as_ref()
            .and_then(|id| self.projects.iter().find(|project| &project.id == id))
    }

    fn in_scope(&self, worker: &Worker) -> bool {
        self.current_project()
            .is_none_or(|project| project.id == worker.project_id)
    }

    pub(crate) fn worker(&self, id: &str) -> Option<&Worker> {
        self.workers.iter().find(|worker| worker.id == id)
    }

    /// Live workers in berth order, optionally limited to one project.
    pub(crate) fn berths(&self, project: Option<&str>) -> Vec<&Worker> {
        self.capacity
            .live
            .iter()
            .filter_map(|id| self.worker(id))
            .filter(|worker| project.is_none_or(|id| worker.project_id == id))
            .collect()
    }

    /// Whether the project's own berths are all taken; projects do not share berths.
    pub(crate) fn project_full(&self, project: &str) -> bool {
        self.berths(Some(project)).len() >= self.capacity.max_workers
    }

    fn project_name(&self, id: &str) -> Option<&str> {
        self.projects
            .iter()
            .find(|project| project.id == id)
            .map(|project| project.name.as_str())
    }

    pub(crate) fn tone_color(&self, tone: Tone) -> u32 {
        match tone {
            Tone::Working => self.theme.link,
            Tone::Input => self.theme.error,
            Tone::Blocked => self.theme.error,
            Tone::Review => self.theme.review,
            Tone::Ready => self.theme.success,
        }
    }

    pub(crate) fn open_new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.form_open = !self.form_open;
        self.active_field = 0;
        if let Some(project) = self.current_project()
            && self.fields[0].is_empty()
        {
            self.fields[0] = project.path.to_string_lossy().into_owned();
            self.active_field = 1;
        }
        self.form_focus.focus(window);
        cx.notify();
    }

    pub(crate) fn close_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.connection
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.terminal = None;
        self.focused_berth = self.selected.take();
        self.workspace_focus.focus(window);
        if let Some(id) = &self.focused_berth
            && let Some(handle) = self.berth_focus.get(&format!("berth-{id}"))
        {
            handle.focus(window);
        }
        self.details.clear();
        self.usage_open = false;
        self.ci_open = false;
        cx.notify();
    }

    pub(crate) fn run_berth_action(
        &mut self,
        action: &Action,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            Action::Reply => self.open_worker(id, window, cx),
            Action::SendCi => self.run_action("send_ci_feedback", json!({"worker_id": id}), cx),
            Action::OpenPr(url) => cx.open_url(url),
        }
    }

    fn add_repository(&mut self, cx: &mut Context<Self>) {
        if self.repo_picker_open {
            return;
        }
        self.repo_picker_open = true;
        let selection = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add repository".into()),
        });
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let path = match selection.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let added = match path {
                Some(path) => Some(
                    cx.background_executor()
                        .spawn(async move {
                            let path = path
                                .to_str()
                                .ok_or_else(|| anyhow::anyhow!("repository path is not UTF-8"))?
                                .to_owned();
                            let value = client.call("add_project", json!({"path": path}))?;
                            Ok::<Project, anyhow::Error>(serde_json::from_value(value)?)
                        })
                        .await,
                ),
                None => None,
            };
            let _ = this.update(cx, |this, cx| {
                this.repo_picker_open = false;
                match added {
                    Some(Ok(project)) => {
                        this.selected_project = Some(project.id.clone());
                        if !this.projects.iter().any(|known| known.id == project.id) {
                            this.projects.push(project);
                        }
                    }
                    Some(Err(error)) => this.error = Some(error.to_string()),
                    None => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn dot(&self, color: u32, size: f32) -> gpui::Div {
        div().size(px(size)).rounded_full().bg(rgb(color))
    }

    /// Inbox badge: workers that need you plus reviews requested from you.
    pub(crate) fn inbox_count(&self) -> usize {
        let needs = self
            .workers
            .iter()
            .filter(|worker| derived_status(&worker.facts) == DerivedStatus::NeedsYou)
            .count();
        needs + self.inbox.review_requests()
    }

    fn agent_row(&self, worker: &Worker, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let status = status(worker);
        let active = self.terminal.is_some() && self.selected.as_deref() == Some(&worker.id);
        let id = worker.id.clone();
        div()
            .id(SharedString::from(format!("agent-{id}")))
            .flex()
            .items_center()
            .gap_2()
            .ml_5()
            .pl_2()
            .pr_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .border_l_2()
            .border_color(if active {
                rgb(theme.accent).into()
            } else {
                gpui::transparent_black()
            })
            .when(active, |row| row.bg(rgb(theme.base)))
            .when(!active, |row| row.hover(|style| style.bg(rgb(theme.panel))))
            .child(crate::icons::app_icon(px(16.)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_sm()
                    .text_color(rgb(if active { theme.text } else { theme.muted }))
                    .child(worker.title.clone()),
            )
            .child(self.dot(self.tone_color(status.tone), 7.))
            .tooltip({
                let text = format!(
                    "{} · {} · Enter to open",
                    status.pill,
                    agent_label(&worker.agent)
                );
                move |_, cx| crate::keyboard_ui::tooltip(text.clone(), cx)
            })
            .tab_index(0)
            .on_key_down(cx.listener({
                let id = id.clone();
                move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.open_worker(id.clone(), window, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .on_click(
                cx.listener(move |this, _, window, cx| this.open_worker(id.clone(), window, cx)),
            )
            .into_any_element()
    }

    fn project_tree(
        &self,
        index: usize,
        project: &Project,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = self.theme;
        // Rotate through the terminal palette so each project keeps a stable color.
        let color = theme.ansi[[5, 6, 3, 4, 2, 1][index % 6]];
        let agents: Vec<&Worker> = self
            .workers
            .iter()
            .filter(|worker| worker.project_id == project.id)
            .collect();
        let live = agents
            .iter()
            .filter(|worker| self.capacity.live.contains(&worker.id))
            .count();
        let collapsed = self.collapsed.contains(&project.id);
        let active = self.terminal.is_none()
            && self.view == crate::View::Berths
            && self.selected_project.as_deref() == Some(&project.id);
        let toggle = project.id.clone();
        let select = project.id.clone();
        let mut tree = div().flex().flex_col().child(
            div()
                .id(SharedString::from(format!("project-{}", project.id)))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(active, |row| row.bg(rgb(theme.selection)))
                .when(!active, |row| row.hover(|style| style.bg(rgb(theme.panel))))
                .child(
                    div()
                        .id(SharedString::from(format!("toggle-{}", project.id)))
                        .flex_none()
                        .child(icon(
                            if collapsed {
                                Icon::ChevronRight
                            } else {
                                Icon::ChevronDown
                            },
                            px(12.),
                            rgb(theme.muted),
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if !this.collapsed.remove(&toggle) {
                                this.collapsed.insert(toggle.clone());
                            }
                            cx.notify();
                        })),
                )
                .child(div().size(px(9.)).flex_none().rounded_sm().bg(rgb(color)))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_sm()
                        .child(project.name.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(rgb(theme.muted))
                        .child(match live {
                            0 => "idle".to_owned(),
                            n => format!("{n}/{}", self.capacity.max_workers),
                        }),
                )
                .tooltip({
                    let hint = format!(
                        "{} · Enter to select · ⌘{} to jump",
                        project.path.display(),
                        (index + 2).min(9)
                    );
                    move |_, cx| crate::keyboard_ui::tooltip(hint.clone(), cx)
                })
                .tab_index(0)
                .border_1()
                .border_color(gpui::transparent_black())
                .focus(|style| style.border_color(rgb(theme.focus)))
                .on_key_down(cx.listener({
                    let select = select.clone();
                    move |this, event: &gpui::KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.view = crate::View::Berths;
                            this.select_project(Some(select.clone()), window, cx);
                            cx.stop_propagation();
                        }
                    }
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.view = crate::View::Berths;
                    this.select_project(Some(select.clone()), window, cx);
                })),
        );
        if !collapsed {
            for worker in agents {
                tree = tree.child(self.agent_row(worker, cx));
            }
        }
        tree.into_any_element()
    }

    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let in_use = self.capacity.live.len();
        let max = self.capacity.max_workers;
        let scope = self.selected_project.as_deref();
        let inbox_active = self.terminal.is_none() && self.view == crate::View::Inbox;
        let inbox_count = self.inbox_count();
        let mut projects = div().flex().flex_col().gap_0p5();
        for (index, project) in self.projects.iter().enumerate() {
            projects = projects.child(self.project_tree(index, project, cx));
        }
        if self.projects.is_empty() {
            projects = projects.child(
                div()
                    .px_2()
                    .text_sm()
                    .text_color(rgb(theme.muted))
                    .child("Add a repository to dock agents"),
            );
        }
        // Berths are per project: show the selected project's slots, or nothing for All berths.
        let mut capacity = div().flex().gap_1();
        for (_, worker) in scope.map(|id| self.slots(Some(id))).unwrap_or_default() {
            let color = worker.map_or(theme.border, |worker| self.tone_color(status(worker).tone));
            capacity = capacity.child(
                div()
                    .h(px(6.))
                    .flex_1()
                    .max_w(px(22.))
                    .rounded_full()
                    .bg(rgb(color)),
            );
        }
        let unfinished = self.recovery_entries.len();
        div()
            .id("sidebar")
            // One sixth of the window, matching the agent view's 1:3:2 split.
            .w(gpui::relative(1. / 6.))
            .min_w(px(220.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.sidebar))
            .border_r_1()
            .border_color(rgb(theme.border))
            .child(
                div()
                    .id("brand")
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .pt_4()
                    .pb_3()
                    .cursor_pointer()
                    .child(crate::icons::app_icon(px(26.)))
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("SigmaDock"),
                    )
                    .child(
                        div()
                            .px_2()
                            .rounded_full()
                            .bg(rgb(theme.chip))
                            .text_xs()
                            .text_color(rgb(theme.muted))
                            .child(format!("{in_use} live")),
                    )
                    .tooltip(|_, cx| crate::keyboard_ui::tooltip("All berths · ⌘1".into(), cx))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.view = crate::View::Berths;
                        this.select_project(None, window, cx);
                    })),
            )
            .child(
                div().px_2().child(
                    div()
                        .id("inbox")
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .cursor_pointer()
                        .border_l_2()
                        .border_color(if inbox_active {
                            rgb(theme.accent).into()
                        } else {
                            gpui::transparent_black()
                        })
                        .when(inbox_active, |row| row.bg(rgb(theme.base)))
                        .when(!inbox_active, |row| {
                            row.hover(|style| style.bg(rgb(theme.panel)))
                        })
                        .child(icon(Icon::Inbox, px(16.), rgb(theme.accent)))
                        .child(
                            div()
                                .flex_1()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_sm()
                                .child("Inbox"),
                        )
                        .when(inbox_count > 0, |row| {
                            row.child(
                                div()
                                    .px_1p5()
                                    .rounded_full()
                                    .bg(rgb(theme.accent))
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(theme.surface))
                                    .child(inbox_count.to_string()),
                            )
                        })
                        .tab_index(0)
                        .tooltip(|_, cx| crate::keyboard_ui::tooltip("Inbox · ⌘I".into(), cx))
                        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                this.open_inbox(window, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .on_click(cx.listener(|this, _, window, cx| this.open_inbox(window, cx))),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .pt_4()
                    .pb_1()
                    .text_xs()
                    .text_color(rgb(theme.muted))
                    .child("PROJECTS")
                    .child(
                        div()
                            .id("add-repository")
                            .p_0p5()
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(theme.panel)))
                            .child(icon(Icon::Plus, px(14.), rgb(theme.muted)))
                            .tooltip(|_, cx| {
                                crate::keyboard_ui::tooltip("Add repository…".into(), cx)
                            })
                            .tab_index(0)
                            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                    this.add_repository(cx);
                                    cx.stop_propagation();
                                }
                            }))
                            .on_click(cx.listener(|this, _, _, cx| this.add_repository(cx))),
                    ),
            )
            .child(
                div()
                    .id("project-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .px_2()
                    .child(projects),
            )
            .when(unfinished > 0, |sidebar| {
                sidebar.child(
                    div()
                        .id("show-unfinished")
                        .flex()
                        .items_center()
                        .gap_2()
                        .mx_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .text_sm()
                        .text_color(rgb(theme.muted))
                        .hover(|style| style.bg(rgb(theme.panel)))
                        .child(icon(Icon::Terminal, px(14.), rgb(theme.muted)))
                        .child(format!("Unfinished sessions  {unfinished}"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.terminal.is_some() {
                                this.close_terminal(window, cx);
                            }
                            this.view = crate::View::Berths;
                            this.recovery_open = !this.recovery_open;
                            cx.notify();
                        })),
                )
            })
            .child(
                div()
                    .flex()
                    .items_end()
                    .justify_between()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(theme.border))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .text_xs()
                            .text_color(rgb(theme.muted))
                            .child(match scope {
                                Some(id) => {
                                    format!("Berths · {} of {max}", self.berths(Some(id)).len())
                                }
                                None => format!("{max} berths per project"),
                            })
                            .child(capacity),
                    )
                    .child(
                        div()
                            .id("terminal-settings")
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_xs()
                            .text_color(rgb(if self.settings_open {
                                theme.accent
                            } else {
                                theme.muted
                            }))
                            .when(self.settings_open, |row| row.bg(rgb(theme.base)))
                            .hover(|style| style.bg(rgb(theme.panel)))
                            .tab_index(0)
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .focus(|style| style.border_color(rgb(theme.focus)))
                            .child(icon(
                                Icon::Settings,
                                px(14.),
                                rgb(if self.settings_open {
                                    theme.accent
                                } else {
                                    theme.muted
                                }),
                            ))
                            .child("Settings")
                            .tooltip(|_, cx| {
                                cx.new(|_| crate::appearance_ui::SettingsTooltip).into()
                            })
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
            )
            .into_any_element()
    }

    pub(crate) fn header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let project = self.current_project();
        let name = project.map_or_else(|| "All berths".to_owned(), |p| p.name.clone());
        let here = self.berths(project.map(|p| p.id.as_str())).len();
        let max = self.capacity.max_workers;
        let summary = match project {
            Some(_) => format!("{here} of {max} berths in use"),
            None => format!(
                "{here} berth{} live across projects · {max} per project",
                if here == 1 { "" } else { "s" }
            ),
        };
        let mut pips = div().flex().items_center().gap_1();
        let slots = project.map(|p| self.slots(Some(&p.id))).unwrap_or_default();
        for (_, worker) in slots {
            pips = pips.child(match worker {
                Some(worker) => self.dot(self.tone_color(status(worker).tone), 9.),
                None => div()
                    .size(px(9.))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(theme.border)),
            });
        }
        div()
            .flex()
            .justify_between()
            .items_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .text_sm()
                            .text_color(rgb(theme.muted))
                            .child(summary)
                            .child(pips),
                    ),
            )
            .child(
                div()
                    .id("new-worker")
                    .cursor_pointer()
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(theme.accent))
                    .text_color(rgb(theme.base))
                    .font_weight(FontWeight::MEDIUM)
                    .child("+  New task")
                    .tab_index(0)
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .focus(|style| style.border_color(rgb(theme.focus)))
                    .tooltip(|_, cx| crate::keyboard_ui::tooltip("New task · ⌘N".into(), cx))
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.open_new_task(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .on_click(cx.listener(|this, _, window, cx| this.open_new_task(window, cx))),
            )
            .into_any_element()
    }

    pub(crate) fn needs_strip(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let needs: Vec<_> = self
            .workers
            .iter()
            .filter(|worker| {
                self.in_scope(worker) && derived_status(&worker.facts) == DerivedStatus::NeedsYou
            })
            .collect();
        if needs.is_empty() {
            return div()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(rgb(theme.muted))
                .child(icon(Icon::Check, px(16.), rgb(theme.success)))
                .child("Nothing needs you")
                .into_any_element();
        }
        let mut items = div().flex().flex_wrap().gap_2();
        for worker in needs {
            let status = status(worker);
            let color = self.tone_color(status.tone);
            let id = worker.id.clone();
            let key_id = id.clone();
            let hint = format!("{} · {} · Enter to focus berth", worker.title, status.pill);
            items = items.child(
                div()
                    .id(SharedString::from(format!("needs-{id}")))
                    .tab_index(0)
                    .focus(|style| style.border_color(rgb(theme.focus)))
                    .tooltip(move |_, cx| crate::keyboard_ui::tooltip(hint.clone(), cx))
                    .on_key_down(cx.listener(
                        move |this, event: &gpui::KeyDownEvent, window, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                this.focus_worker_berth(&key_id, window, cx);
                                cx.stop_propagation();
                            }
                        },
                    ))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .rounded_full()
                    .cursor_pointer()
                    .bg(rgb(theme.surface))
                    .border_1()
                    .border_color(rgb(color))
                    .text_sm()
                    .child(self.dot(color, 7.))
                    .child(worker.title.clone())
                    .child(div().text_color(rgb(color)).child(status.pill))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.focus_worker_berth(&id, window, cx);
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .bg(rgb(theme.panel))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme.attention))
                    .child("Needs you"),
            )
            .child(items)
            .into_any_element()
    }

    /// `index` is the grid position for keyboard moves; `number` is the berth shown.
    fn berth(
        &self,
        index: usize,
        number: usize,
        worker: &Worker,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = self.theme;
        let status = status(worker);
        let color = self.tone_color(status.tone);
        let glow = matches!(status.tone, Tone::Input | Tone::Blocked);
        let focused = self.focused_berth.as_deref() == Some(&worker.id);
        let id = worker.id.clone();
        let focus_key = format!("berth-{id}");
        let hint = format!(
            "Berth {number}: {} · {} · Enter: terminal · ⌘Enter: action · Arrow keys: move",
            worker.title, status.pill
        );
        let mut preview = div()
            .h(px(PREVIEW_LINES as f32 * 16. + 16.))
            .p_2()
            .rounded_md()
            .overflow_hidden()
            .bg(rgb(theme.sidebar))
            .flex()
            .flex_col()
            .justify_end()
            .text_xs()
            .font_family(MONO)
            .text_color(rgb(theme.muted));
        match self.previews.get(&worker.id) {
            Some(screen) if !screen.lines.is_empty() => {
                for line in &screen.lines {
                    preview = preview.child(div().whitespace_nowrap().child(line.clone()));
                }
            }
            _ => preview = preview.child("Waiting for output…"),
        }
        let action = status.action.clone().map(|action| {
            let label = match &action {
                Action::Reply => "Reply",
                Action::SendCi => "Send CI to agent",
                Action::OpenPr(_) => "Open PR",
            };
            let pr = matches!(action, Action::OpenPr(_));
            let id = id.clone();
            let key_id = id.clone();
            let key_action = action.clone();
            div()
                .id(SharedString::from(format!("berth-action-{id}")))
                .tab_index(0)
                .border_2()
                .border_color(gpui::transparent_black())
                .focus(|style| style.border_color(rgb(theme.focus)))
                .tooltip(move |_, cx| {
                    crate::keyboard_ui::tooltip(
                        format!("{label} · ⌘Enter from berth, Enter on action"),
                        cx,
                    )
                })
                .on_key_down(
                    cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.run_berth_action(&key_action, key_id.clone(), window, cx);
                            cx.stop_propagation();
                        }
                    }),
                )
                .flex()
                .items_center()
                .gap_1p5()
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .bg(rgb(color))
                .text_color(rgb(theme.surface))
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .when(pr, |button| {
                    button.child(icon(Icon::GitPullRequest, px(14.), rgb(theme.surface)))
                })
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.run_berth_action(&action, id.clone(), window, cx);
                }))
        });
        div()
            .id(SharedString::from(focus_key.clone()))
            .tab_index(0)
            .track_focus(&self.berth_focus[&focus_key])
            .focus(|style| style.border_color(rgb(theme.focus)))
            .tooltip(move |_, cx| crate::keyboard_ui::tooltip(hint.clone(), cx))
            .on_key_down(
                cx.listener(move |this, event, window, cx| {
                    this.berth_key(index, event, window, cx)
                }),
            )
            .cursor_pointer()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(rgb(theme.surface))
            .border_1()
            .border_color(rgb(if focused || glow { color } else { theme.border }))
            .when(focused, |berth| berth.border_2())
            .when(glow, |berth| {
                berth.shadow(vec![BoxShadow {
                    color: rgba((color << 8) | 0x66).into(),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(14.),
                    spread_radius: px(1.),
                }])
            })
            .hover(|style| style.border_color(rgb(theme.focus)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(30.))
                            .flex_none()
                            .rounded_full()
                            .border_2()
                            .border_color(rgb(color))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(number.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(worker.title.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px_2()
                            .py_0p5()
                            .rounded_full()
                            .bg(rgba((color << 8) | 0x26))
                            .text_xs()
                            .text_color(rgb(color))
                            .child(status.pill),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child(
                        div()
                            .flex_none()
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .bg(rgb(theme.chip))
                            .text_xs()
                            .text_color(rgb(theme.muted))
                            .child(agent_label(&worker.agent).to_owned()),
                    )
                    .when_some(
                        self.selected_project
                            .is_none()
                            .then(|| self.project_name(&worker.project_id))
                            .flatten(),
                        |row, name| {
                            row.child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(name.to_owned()),
                            )
                        },
                    )
                    .child(icon(Icon::GitBranch, px(14.), rgb(theme.muted)))
                    .child(
                        div()
                            .min_w(px(0.))
                            .truncate()
                            .font_family(MONO)
                            .text_color(rgb(theme.muted))
                            .child(worker.branch.clone()),
                    ),
            )
            .when_some(worker.base_warning.clone(), |berth, warning| {
                berth.child(
                    div()
                        .id(SharedString::from(format!("base-warning-{}", worker.id)))
                        .text_sm()
                        .text_color(rgb(theme.warning))
                        .child("Using cached remote base")
                        .tooltip(move |_, cx| crate::keyboard_ui::tooltip(warning.clone(), cx)),
                )
            })
            .child({
                let checks_id = worker.id.clone();
                let key_id = checks_id.clone();
                div()
                    .id(SharedString::from(format!("berth-checks-{checks_id}")))
                    .tab_index(0)
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .focus(|style| style.border_color(rgb(theme.focus)))
                    .p_1()
                    .rounded_md()
                    .bg(rgb(theme.button))
                    .child("Checks · ⌘⇧K")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_checks(checks_id.clone(), window, cx);
                    }))
                    .on_key_down(cx.listener(
                        move |this, event: &gpui::KeyDownEvent, window, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                this.open_checks(key_id.clone(), window, cx);
                                cx.stop_propagation();
                            }
                        },
                    ))
            })
            .child(preview)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(28.))
                    .text_xs()
                    .font_family(MONO)
                    .text_color(rgb(theme.muted))
                    .child(format!(":{}", worker.port))
                    .children(action),
            )
            .on_click(
                cx.listener(move |this, _, window, cx| this.open_worker(id.clone(), window, cx)),
            )
            .into_any_element()
    }

    fn empty_berth(&self, number: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let project = self.selected_project.as_deref();
        let in_use = project.map_or(0, |id| self.berths(Some(id)).len());
        let full =
            project.is_none_or(|id| self.project_full(id)) || number > self.capacity.max_workers;
        div()
            .id(SharedString::from(format!("empty-berth-{number}")))
            .tab_index(0)
            .track_focus(&self.berth_focus[&format!("empty-berth-{number}")])
            .focus(|style| style.border_color(rgb(theme.focus)))
            .tooltip(move |_, cx| {
                crate::keyboard_ui::tooltip(
                    format!(
                        "Berth {number}: {} · Arrow keys: move",
                        if full {
                            "No free berth"
                        } else {
                            "Enter: dock a task · ⌘N: new task"
                        }
                    ),
                    cx,
                )
            })
            .on_key_down(cx.listener(move |this, event, window, cx| {
                this.berth_key(number - 1, event, window, cx)
            }))
            .min_h(px(250.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .rounded_lg()
            .border_2()
            .border_dashed()
            .border_color(rgb(theme.empty))
            .text_color(rgb(theme.muted))
            .child(
                div()
                    .text_sm()
                    .font_family(MONO)
                    .child(format!("Berth {number}")),
            )
            .when(full, |berth| {
                berth
                    .opacity(0.55)
                    .child(if number > self.capacity.max_workers {
                        "Outside current capacity"
                    } else {
                        "No free berth"
                    })
                    .child(div().text_xs().child(format!(
                        "{in_use} of {} in use in this project",
                        self.capacity.max_workers
                    )))
            })
            .when(!full, |berth| {
                berth
                    .cursor_pointer()
                    .hover(|style| style.border_color(rgb(theme.focus)))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(theme.text))
                            .child("Dock a task"),
                    )
                    .child(div().text_xs().font_family(MONO).child("⌘N"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        if !this.form_open {
                            this.open_new_task(window, cx);
                        }
                    }))
            })
            .into_any_element()
    }

    /// Grid slots in berth-number order: the worker docked at each number, if any.
    /// Workers keep stable berth numbers, so occupied slots can have gaps between them.
    /// Numbers are per project, so All berths lists live berths without free slots.
    pub(crate) fn slots(&self, project: Option<&str>) -> Vec<(usize, Option<&Worker>)> {
        match project {
            Some(_) => slot_layout(self.berths(project), self.capacity.max_workers),
            None => self
                .berths(None)
                .into_iter()
                .map(|worker| (worker.berth.map_or(0, usize::from), Some(worker)))
                .collect(),
        }
    }

    pub(crate) fn grid(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let project = self.current_project().map(|p| p.id.clone());
        let mut grid = div().grid().grid_cols(3).gap_4();
        if project.is_none() && self.capacity.live.is_empty() {
            return div()
                .text_sm()
                .text_color(rgb(self.theme.muted))
                .child("No live berths. Pick a project to dock a task.")
                .into_any_element();
        }
        for (index, (number, worker)) in self.slots(project.as_deref()).into_iter().enumerate() {
            grid = grid.child(match worker {
                Some(worker) => self.berth(index, number, worker, cx),
                None => self.empty_berth(number, cx),
            });
        }
        grid.into_any_element()
    }

    fn side_row(
        &self,
        worker: &Worker,
        clickable: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = self.theme;
        let status = status(worker);
        let color = self.tone_color(status.tone);
        let id = worker.id.clone();
        let focused = self.focused_berth.as_deref() == Some(&worker.id);
        div()
            .id(SharedString::from(format!("side-{id}")))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(if focused { color } else { theme.border }))
            .bg(rgb(theme.surface))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.dot(color, 7.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_sm()
                            .child(worker.title.clone()),
                    ),
            )
            .child(div().text_xs().text_color(rgb(theme.muted)).child({
                let mut text = status.pill;
                if let Some(number) = pr_number(worker) {
                    text.push_str(&format!(" · #{number}"));
                }
                if let Some(at) = worker.archived_at {
                    text.push_str(&format!(" · {}", relative_time(at, unix_time())));
                }
                text
            }))
            .when(!clickable, |row| {
                let id = id.clone();
                row.child(
                    div()
                        .id(SharedString::from(format!("summary-{id}")))
                        .text_xs()
                        .text_color(rgb(theme.accent))
                        .cursor_pointer()
                        .hover(|style| style.underline())
                        .child(self.summary_label(&id, "Copy summary"))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.copy_summary(id.clone(), cx)),
                        ),
                )
            })
            .when(clickable, |row| {
                row.cursor_pointer()
                    .hover(|style| style.border_color(rgb(theme.focus)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_worker(id.clone(), window, cx)
                    }))
            })
            .into_any_element()
    }

    pub(crate) fn side_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let now = unix_time();
        let midnight = local_midnight(now);
        let off_berth: Vec<_> = self
            .workers
            .iter()
            .filter(|worker| {
                self.in_scope(worker)
                    && worker.role != sigmadock_core::WorkerRole::Orchestrator
                    && !self.capacity.live.contains(&worker.id)
            })
            .collect();
        let (merged, docked): (Vec<_>, Vec<_>) = off_berth
            .into_iter()
            .partition(|worker| worker.facts.pr == PullRequestState::Merged);
        let departed = self
            .departed
            .iter()
            .filter(|worker| self.in_scope(worker))
            .filter(|worker| worker.archived_at.is_some_and(|at| at >= midnight));
        let section = |title: &'static str, hint: &'static str| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_family(MONO)
                        .text_color(rgb(theme.muted))
                        .child(title),
                )
                .child(div().text_xs().text_color(rgb(theme.muted)).child(hint))
        };
        let mut supervisors = div().flex().flex_col().gap_2();
        for worker in self.workers.iter().filter(|worker| {
            self.in_scope(worker) && worker.role == sigmadock_core::WorkerRole::Orchestrator
        }) {
            supervisors = supervisors.child(self.side_row(worker, true, cx));
        }
        let mut docked_list = div().flex().flex_col().gap_2();
        for worker in &docked {
            docked_list = docked_list.child(self.side_row(worker, true, cx));
        }
        if docked.is_empty() {
            docked_list = docked_list.child(
                div()
                    .text_sm()
                    .text_color(rgb(theme.muted))
                    .child("Nothing moored"),
            );
        }
        let mut departed_list = div().flex().flex_col().gap_2();
        let mut any_departed = false;
        for worker in merged.into_iter().chain(departed) {
            any_departed = true;
            departed_list = departed_list.child(self.side_row(worker, false, cx));
        }
        if !any_departed {
            departed_list = departed_list.child(
                div()
                    .text_sm()
                    .text_color(rgb(theme.muted))
                    .child("No departures yet today"),
            );
        }
        div()
            .id("side-panel")
            .w(px(250.))
            .flex_none()
            .flex()
            .flex_col()
            .gap_4()
            .child(section(
                "ORCHESTRATORS",
                "Separate allowance: one per project",
            ))
            .child(supervisors)
            .child(section("MOORED", "Session ended, work not yet archived"))
            .child(docked_list)
            .child(section("DEPARTED TODAY", "Merged or archived"))
            .child(departed_list)
            .into_any_element()
    }

    pub(crate) fn footer(&self) -> gpui::AnyElement {
        let theme = self.theme;
        let (color, label) = if self.daemon_connected {
            (theme.success, "Daemon connected")
        } else {
            (theme.error, "Daemon unreachable")
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .pt_4()
            .border_t_1()
            .border_color(rgb(theme.border))
            .text_sm()
            .font_family(MONO)
            .text_color(rgb(theme.muted))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.dot(color, 8.))
                    .child(label),
            )
            .child(format!(
                "{} of {} berths in use · local workspace",
                self.capacity.live.len(),
                self.capacity.max_workers
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigmadock_core::Facts;

    fn worker(facts: Facts) -> Worker {
        serde_json::from_value(serde_json::json!({
            "id": "w", "project_id": "p", "title": "t", "agent": "claude",
            "branch": "sigma/t", "worktree": "/tmp/w", "port": 1, "created_at": 0,
            "archived": false, "facts": facts,
        }))
        .unwrap()
    }

    #[test]
    fn slots_keep_berth_numbers_with_gaps() {
        let mut first = worker(Facts::default());
        first.berth = Some(3);
        let mut second = worker(Facts::default());
        second.id = "x".into();
        second.berth = Some(7);
        let slots = slot_layout(vec![&first, &second], 5);
        assert_eq!(slots.len(), 7);
        let docked: Vec<_> = slots
            .iter()
            .map(|(number, worker)| (*number, worker.map(|w| w.id.as_str())))
            .filter(|(_, id)| id.is_some())
            .collect();
        assert_eq!(docked, [(3, Some("w")), (7, Some("x"))]);
        assert!(slots[1].1.is_none());
        assert_eq!(slot_layout(vec![], 2).len(), 2);
    }

    #[test]
    fn relative_time_buckets() {
        assert_eq!(relative_time(100, 130), "Just now");
        assert_eq!(relative_time(0, 125), "2m ago");
        assert_eq!(relative_time(0, 7200), "2h ago");
        assert_eq!(relative_time(200, 100), "Just now");
    }

    #[test]
    fn local_midnight_is_within_the_last_day() {
        let now = unix_time();
        let midnight = local_midnight(now);
        assert!(midnight <= now && now - midnight < 86_400 + 3600);
    }

    #[test]
    fn status_follows_the_derived_status() {
        let input = status(&worker(Facts {
            session: SessionState::NeedsInput,
            ..Facts::default()
        }));
        assert_eq!(
            (input.tone, input.action),
            (Tone::Input, Some(Action::Reply))
        );
        let ci = status(&worker(Facts {
            checks: Checks::Failed,
            ..Facts::default()
        }));
        assert_eq!((ci.tone, ci.action), (Tone::Blocked, Some(Action::SendCi)));
        let url = "https://github.com/acme/web/pull/42".to_owned();
        let ready = worker(Facts {
            pr: PullRequestState::Open,
            review: Review::Approved,
            checks: Checks::Passed,
            mergeable: Some(true),
            pr_url: Some(url.clone()),
            ..Facts::default()
        });
        assert_eq!(status(&ready).action, Some(Action::OpenPr(url)));
        assert_eq!(pr_number(&ready), Some("42"));
        let review = status(&worker(Facts {
            pr: PullRequestState::Open,
            ..Facts::default()
        }));
        assert_eq!((review.tone, review.action), (Tone::Review, None));
        assert_eq!(status(&worker(Facts::default())).tone, Tone::Working);
    }

    #[test]
    fn preview_follows_resize_before_tui_redraw_and_without_new_bytes() {
        let mut preview = Preview::new();
        let mut output = Output {
            cursor: 0,
            bytes: b"\x1b[2J\x1b[30;120HX".to_vec(),
            rows: Some(30),
            cols: Some(120),
            truncated: false,
            exited: false,
        };
        assert!(preview.feed(&output));
        assert!(preview.screen.screen_text()[29].ends_with('X'));
        output.rows = Some(24);
        output.cols = Some(80);
        output.bytes = b"\x1b[2J\x1b[24;80HY".to_vec();
        assert!(preview.feed(&output));
        let screen = preview.screen.screen_text();
        assert_eq!(screen.len(), 24);
        assert_eq!(screen[23].chars().count(), 80);
        assert!(screen[23].ends_with('Y'));
        output.bytes.clear();
        output.cols = Some(100);
        assert!(preview.feed(&output));
        assert_eq!(preview.geometry, (100, 24));
        assert!(!preview.feed(&output));
        let legacy: Output = serde_json::from_value(json!({
            "cursor": 0, "bytes": [], "truncated": false, "exited": false
        }))
        .unwrap();
        assert!(!preview.feed(&legacy));
        assert_eq!(preview.geometry, (100, 24));
    }

    #[test]
    fn preview_keeps_the_last_non_blank_rows() {
        let mut preview = Preview::new();
        let bytes: Vec<u8> = (1..=10)
            .flat_map(|n| format!("line {n}\r\n").into_bytes())
            .collect();
        preview.feed(&Output {
            cursor: bytes.len() as u64,
            bytes: bytes.clone(),
            rows: Some(30),
            cols: Some(120),
            truncated: false,
            exited: false,
        });
        assert_eq!(preview.lines.len(), PREVIEW_LINES);
        assert_eq!(preview.lines.last().map(String::as_str), Some("line 10"));
        assert_eq!(preview.cursor, bytes.len() as u64);
    }
}
