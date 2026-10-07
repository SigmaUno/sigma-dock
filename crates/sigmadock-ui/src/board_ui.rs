//! Project sidebar, kanban lanes and status footer.
use crate::Workspace;
use gpui::{Context, FontWeight, SharedString, div, prelude::*, px, rgb};
use sigmadock_core::{
    Checks, Column, Project, PullRequestState, Review, SessionState, Worker, column, unix_time,
};

const MONO: &str = "Menlo";

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

pub(crate) fn relative_time(created_at: u64, now: u64) -> String {
    let elapsed = now.saturating_sub(created_at);
    match elapsed {
        0..60 => "Just now".into(),
        60..3600 => format!("{}m ago", elapsed / 60),
        3600..86400 => format!("{}h ago", elapsed / 3600),
        _ => format!("{}d ago", elapsed / 86400),
    }
}

/// The single most useful line about a worker, phrased for its lane.
pub(crate) enum Activity {
    Terminal(String),
    PullRequest(String),
    Passing,
}

pub(crate) fn activity(worker: &Worker) -> Activity {
    let facts = &worker.facts;
    let pr_label = || {
        facts
            .pr_url
            .as_deref()
            .and_then(|url| url.trim_end_matches('/').rsplit('/').next())
            .filter(|number| number.bytes().all(|b| b.is_ascii_digit()) && !number.is_empty())
            .map_or_else(
                || "Pull request".to_owned(),
                |n| format!("Pull request #{n}"),
            )
    };
    let text = match column(facts) {
        Column::NeedsYou => match () {
            _ if facts.session == SessionState::NeedsInput => "Waiting for your input".into(),
            _ if facts.session == SessionState::Lost => "Session lost".into(),
            _ if facts.checks == Checks::Failed => "Checks failing".into(),
            _ if facts.review == Review::ChangesRequested => "Changes requested".into(),
            _ if facts.mergeable == Some(false) => "Merge conflicts".into(),
            _ if facts.forge_error.is_some() => "Forge unavailable".into(),
            _ if facts.pr == PullRequestState::Closed => "Pull request closed".into(),
            _ => match facts.exit_code {
                Some(code) => format!("Exited with code {code}"),
                None => "Needs attention".into(),
            },
        },
        Column::ReadyToMerge if facts.pr == PullRequestState::Merged => {
            return Activity::PullRequest(format!("{} merged", pr_label()));
        }
        Column::ReadyToMerge => return Activity::Passing,
        Column::InReview => return Activity::PullRequest(pr_label()),
        Column::Working => match facts.session {
            SessionState::Running => "Running".into(),
            SessionState::Idle => "Idle".into(),
            SessionState::Exited => "Exited".into(),
            SessionState::NeedsInput => "Waiting for your input".into(),
            SessionState::Lost => "Session lost".into(),
        },
    };
    Activity::Terminal(text)
}

impl Workspace {
    pub(crate) fn current_project(&self) -> Option<&Project> {
        self.selected_project
            .as_ref()
            .and_then(|id| self.projects.iter().find(|project| &project.id == id))
            .or_else(|| {
                self.projects
                    .iter()
                    .find(|project| self.workers.iter().any(|w| w.project_id == project.id))
            })
            .or_else(|| self.projects.first())
    }

    fn lane_color(&self, status: Column) -> u32 {
        match status {
            Column::Working => self.theme.link,
            Column::NeedsYou => self.theme.attention,
            Column::InReview => self.theme.review,
            Column::ReadyToMerge => self.theme.success,
        }
    }

    fn open_new_task(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
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

    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let current = self.current_project().map(|project| project.id.clone());
        let mut projects = div().flex().flex_col().gap_1();
        for project in &self.projects {
            let id = project.id.clone();
            let active = current.as_ref() == Some(&project.id);
            projects = projects.child(
                div()
                    .id(SharedString::from(format!("project-{id}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(rgb(if active { theme.accent } else { theme.muted }))
                    .when(active, |row| row.bg(rgb(theme.selection)))
                    .when(!active, |row| row.hover(|style| style.bg(rgb(theme.panel))))
                    .child(div().text_sm().child("▣"))
                    .child(div().truncate().child(project.name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_project = Some(id.clone());
                        cx.notify();
                    })),
            );
        }
        if self.projects.is_empty() {
            projects = projects.child(
                div()
                    .px_3()
                    .text_sm()
                    .text_color(rgb(theme.muted))
                    .child("No projects yet"),
            );
        }
        div()
            .w(px(240.))
            .h_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(rgb(theme.sidebar))
            .border_r_1()
            .border_color(rgb(theme.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1()
                    .py_1()
                    .child(div().text_color(rgb(theme.muted)).child("◈"))
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("My workspace"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_1()
                    .child(
                        div()
                            .text_xs()
                            .font_family(MONO)
                            .text_color(rgb(theme.muted))
                            .child("PROJECTS"),
                    )
                    .child(
                        div()
                            .id("add-project")
                            .px_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_color(rgb(theme.muted))
                            .hover(|style| style.bg(rgb(theme.panel)))
                            .child("+")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.fields[0].clear();
                                this.open_new_task(window, cx);
                            })),
                    ),
            )
            .child(projects)
            .child(div().h(px(1.)).bg(rgb(theme.border)))
            .child(
                div()
                    .id("show-unfinished")
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(rgb(theme.muted))
                    .hover(|style| style.bg(rgb(theme.panel)))
                    .child(div().font_family(MONO).text_sm().child(">_"))
                    .child(div().flex_1().child("Unfinished sessions"))
                    .child(
                        div()
                            .text_sm()
                            .child(self.recovery_entries.len().to_string()),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.recovery_open = !this.recovery_open;
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("terminal-settings")
                    .tab_index(0)
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .focus(|style| style.border_color(rgb(theme.focus)))
                    .hover(|style| style.bg(rgb(theme.panel)))
                    .cursor_pointer()
                    .text_color(rgb(theme.muted))
                    .child("⚙")
                    .child("Settings")
                    .tooltip(|_, cx| cx.new(|_| crate::appearance_ui::SettingsTooltip).into())
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_settings(window, cx)))
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.toggle_settings(window, cx);
                            cx.stop_propagation();
                        }
                    })),
            )
            .into_any_element()
    }

    pub(crate) fn board_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let project = self.current_project();
        let name = project.map_or_else(|| "Workspace".to_owned(), |p| p.name.clone());
        let count = self.project_workers().count();
        div()
            .flex()
            .justify_between()
            .items_start()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .text_sm()
                            .text_color(rgb(theme.muted))
                            .child("Projects")
                            .child("›")
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .gap_3()
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(name),
                            )
                            .child(div().pb_1().text_sm().text_color(rgb(theme.muted)).child(
                                format!("{count} worker{}", if count == 1 { "" } else { "s" }),
                            )),
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
                    .on_click(cx.listener(|this, _, window, cx| this.open_new_task(window, cx))),
            )
            .into_any_element()
    }

    pub(crate) fn project_workers(&self) -> impl Iterator<Item = &Worker> {
        let project = self.current_project().map(|project| project.id.clone());
        self.workers
            .iter()
            .filter(move |worker| project.as_ref().is_none_or(|id| &worker.project_id == id))
    }

    fn card(&self, worker: &Worker, now: u64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let id = worker.id.clone();
        let selected = self.selected.as_ref() == Some(&worker.id);
        let (glyph, text, color) = match activity(worker) {
            Activity::Terminal(text) => {
                let color = if column(&worker.facts) == Column::NeedsYou {
                    theme.attention
                } else {
                    theme.link
                };
                (">_", text, color)
            }
            Activity::PullRequest(text) => ("⇄", text, theme.review),
            Activity::Passing => ("✓", "All checks passing".into(), theme.success),
        };
        div()
            .id(SharedString::from(format!("card-{id}")))
            .cursor_pointer()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(rgb(theme.surface))
            .border_1()
            .border_color(rgb(if selected { theme.focus } else { theme.border }))
            .hover(|style| style.border_color(rgb(theme.focus)))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(worker.title.clone()),
            )
            .child(
                div().flex().child(
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .bg(rgb(theme.chip))
                        .text_xs()
                        .text_color(rgb(theme.muted))
                        .child(agent_label(&worker.agent).to_owned()),
                ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .text_sm()
                    .font_family(MONO)
                    .text_color(rgb(theme.muted))
                    .child("⑂")
                    .child(div().truncate().child(worker.branch.clone())),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .text_sm()
                    .text_color(rgb(color))
                    .child(div().font_family(MONO).child(glyph))
                    .child(div().truncate().child(text)),
            )
            .child(
                div()
                    .pt_3()
                    .border_t_1()
                    .border_color(rgb(theme.border))
                    .text_xs()
                    .text_color(rgb(theme.muted))
                    .child(relative_time(worker.created_at, now)),
            )
            .on_click(
                cx.listener(move |this, _, window, cx| this.open_worker(id.clone(), window, cx)),
            )
            .into_any_element()
    }

    pub(crate) fn board(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let now = unix_time();
        let mut board = div().flex().gap_5().items_start();
        for status in Column::ALL {
            let workers: Vec<_> = self
                .project_workers()
                .filter(|worker| column(&worker.facts) == status)
                .collect();
            let mut lane = div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .pb_1()
                        .child(
                            div()
                                .size(px(7.))
                                .rounded_full()
                                .bg(rgb(self.lane_color(status))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(status.label()),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(theme.muted))
                                .child(workers.len().to_string()),
                        ),
                );
            if workers.is_empty() {
                lane = lane.child(
                    div()
                        .p_4()
                        .rounded_lg()
                        .border_1()
                        .border_dashed()
                        .border_color(rgb(theme.border))
                        .text_sm()
                        .text_color(rgb(theme.muted))
                        .child("Nothing here"),
                );
            }
            for worker in workers {
                lane = lane.child(self.card(worker, now, cx));
            }
            board = board.child(lane);
        }
        board.into_any_element()
    }

    pub(crate) fn footer(&self) -> gpui::AnyElement {
        let theme = self.theme;
        let (color, label) = if self.daemon_connected {
            (theme.success, "Daemon connected")
        } else {
            (theme.error, "Daemon unreachable")
        };
        let sessions = self.workers.len();
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
                    .child(div().size(px(8.)).rounded_full().bg(rgb(color)))
                    .child(label),
            )
            .child(format!(
                "{sessions} session{} · local workspace",
                if sessions == 1 { "" } else { "s" }
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
    fn relative_time_buckets() {
        assert_eq!(relative_time(100, 130), "Just now");
        assert_eq!(relative_time(0, 125), "2m ago");
        assert_eq!(relative_time(0, 7200), "2h ago");
        assert_eq!(relative_time(200, 100), "Just now");
    }

    #[test]
    fn activity_names_the_pull_request_and_blocker() {
        let review = worker(Facts {
            pr: PullRequestState::Open,
            pr_url: Some("https://github.com/acme/web/pull/42".into()),
            ..Facts::default()
        });
        assert!(matches!(activity(&review), Activity::PullRequest(t) if t == "Pull request #42"));
        let blocked = worker(Facts {
            session: SessionState::NeedsInput,
            ..Facts::default()
        });
        assert!(
            matches!(activity(&blocked), Activity::Terminal(t) if t == "Waiting for your input")
        );
    }
}
