//! On-demand checks and exact feedback previews for one worker.
use crate::{
    Workspace,
    berths_ui::relative_time,
    ellipsis::Ellipsis,
    icons::{Icon, icon},
};
use gpui::{Context, FontWeight, SharedString, Window, div, prelude::*, px, rgb, rgba};
use serde_json::{Value, json};
use sigmadock_core::{Checks, PullRequestState, ReadinessReport, Review, task_text, unix_time};

#[derive(Default)]
pub(crate) struct ChecksPane {
    pub worker: Option<String>,
    report: Option<ReadinessReport>,
    loading: bool,
    sending: bool,
    error: Option<String>,
    preview: Option<SendPreview>,
    request: u64,
}
#[derive(Clone)]
struct SendPreview {
    worker: String,
    text: String,
    method: &'static str,
    params: Value,
}
#[derive(Clone)]
enum Action {
    Refresh,
    RefreshFacts,
    Close,
    Preview(SendPreview),
    Fetch(&'static str),
    Send,
    Cancel,
    Open(String),
}

/// Right pane of the agent view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum RightTab {
    #[default]
    Changes,
    Readiness,
}

/// Outcome shown on a readiness card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    Pass,
    Fail,
    Pending,
    Unknown,
}

/// One checklist card: what it covers, how it stands and a one-line summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Card {
    pub title: String,
    pub mark: Mark,
    pub summary: String,
}

pub(crate) fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn plural(count: u64, word: &str) -> String {
    format!("{count} {word}{}", if count == 1 { "" } else { "s" })
}

fn pr_number(url: Option<&str>) -> Option<&str> {
    url.and_then(|url| url.trim_end_matches('/').rsplit('/').next())
        .filter(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

/// Git, pull request, CI, review and conflict cards, in display order.
pub(crate) fn cards(report: &ReadinessReport) -> Vec<Card> {
    let facts = &report.worker.facts;
    let git = match &report.git {
        None => Card {
            title: "Git".into(),
            mark: Mark::Unknown,
            summary: report
                .git_error
                .clone()
                .unwrap_or_else(|| "Git state unavailable".into()),
        },
        Some(git) => {
            let mut parts = Vec::new();
            let mut mark = Mark::Pass;
            if git.dirty.is_empty() {
                parts.push("Clean worktree".to_owned());
            } else {
                mark = Mark::Fail;
                let count = git.dirty.len() as u64;
                parts.push(format!(
                    "{}{} uncommitted",
                    plural(count, "file"),
                    if git.dirty_truncated { "+" } else { "" }
                ));
            }
            match (git.ahead, git.behind) {
                (Some(ahead), Some(behind)) => {
                    if behind > 0 {
                        mark = Mark::Fail;
                    }
                    parts.push(format!(
                        "{} ahead, {behind} behind",
                        plural(ahead, "commit")
                    ));
                }
                _ => {
                    if mark == Mark::Pass {
                        mark = Mark::Unknown;
                    }
                    parts.push("comparison with base unknown".into());
                }
            }
            match git.unpushed {
                Some(0) => parts.push("all pushed".into()),
                Some(count) => {
                    mark = Mark::Fail;
                    parts.push(format!("{} unpushed", plural(count, "commit")));
                }
                None => parts.push("push state unknown".into()),
            }
            Card {
                title: "Git".into(),
                mark,
                summary: parts.join(" · "),
            }
        }
    };
    let number = pr_number(facts.pr_url.as_deref());
    let (pr_mark, pr_summary) = match facts.pr {
        PullRequestState::None => (Mark::Fail, "No pull request yet".to_owned()),
        PullRequestState::Draft => (Mark::Fail, "Draft, not ready for review".to_owned()),
        PullRequestState::Closed => (Mark::Fail, "Closed without merging".to_owned()),
        PullRequestState::Merged => (Mark::Pass, "Merged".to_owned()),
        PullRequestState::Open => (Mark::Pass, "Open".to_owned()),
    };
    let pr = Card {
        title: number.map_or_else(
            || "Pull request".to_owned(),
            |n| format!("Pull request #{n}"),
        ),
        mark: if facts.forge_error.is_some() {
            Mark::Unknown
        } else {
            pr_mark
        },
        summary: facts.forge_error.clone().unwrap_or(pr_summary),
    };
    let ci = match &report.ci {
        Some(ci) => {
            let counted: Vec<_> = ci.entries.iter().filter(|e| e.state != "skipped").collect();
            let failed = counted
                .iter()
                .filter(|e| matches!(e.state.as_str(), "failed" | "cancelled"))
                .count();
            let pending = counted
                .iter()
                .filter(|e| !matches!(e.state.as_str(), "failed" | "cancelled" | "passed"))
                .count();
            let total = counted.len();
            let (mark, text) = if total == 0 {
                (Mark::Unknown, "No checks reported".to_owned())
            } else if failed > 0 {
                (Mark::Fail, format!("{failed} of {total} checks failing"))
            } else if pending > 0 {
                (
                    Mark::Pending,
                    format!("{pending} of {total} checks still running"),
                )
            } else {
                (Mark::Pass, format!("All {total} checks passed"))
            };
            Card {
                title: "CI".into(),
                mark,
                summary: format!("{text} · commit {}", short_sha(&ci.head_sha)),
            }
        }
        None => {
            let (mark, text) = match facts.checks {
                Checks::Passed => (Mark::Pass, "Checks passing"),
                Checks::Failed => (Mark::Fail, "Checks failing"),
                Checks::Pending => (Mark::Pending, "Checks running"),
                Checks::Unknown => (Mark::Unknown, "No check results"),
            };
            Card {
                title: "CI".into(),
                mark,
                summary: report.ci_error.clone().unwrap_or_else(|| text.into()),
            }
        }
    };
    let comments = report
        .review
        .as_ref()
        .map_or(0, |r| r.comments.len() as u64);
    let (review_mark, review_text) = match facts.review {
        Review::Approved => (Mark::Pass, "Approved".to_owned()),
        Review::ChangesRequested => (Mark::Fail, "Changes requested".to_owned()),
        Review::Pending => (Mark::Pending, "Awaiting review".to_owned()),
        Review::Unknown => (Mark::Unknown, "No review yet".to_owned()),
    };
    let review = Card {
        title: "Review".into(),
        mark: review_mark,
        summary: if comments > 0 {
            format!("{review_text} · {}", plural(comments, "open comment"))
        } else {
            review_text
        },
    };
    let conflicts = Card {
        title: "Conflicts".into(),
        mark: match facts.mergeable {
            Some(true) => Mark::Pass,
            Some(false) => Mark::Fail,
            None => Mark::Unknown,
        },
        summary: match facts.mergeable {
            Some(true) => "Merges cleanly",
            Some(false) => "Conflicts with the target branch",
            None => "Mergeability not reported yet",
        }
        .into(),
    };
    vec![git, pr, ci, review, conflicts]
}

impl Workspace {
    pub(crate) fn open_checks(
        &mut self,
        worker: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.terminal.is_none() || self.selected.as_ref() != Some(&worker) {
            self.open_worker(worker.clone(), window, cx);
        }
        self.right_tab = RightTab::Readiness;
        self.focused_berth = Some(worker.clone());
        self.checks.request += 1;
        self.checks.worker = Some(worker);
        self.checks.report = None;
        self.checks.preview = None;
        self.checks.loading = false;
        self.checks.error = None;
        self.checks_focus.focus(window);
        self.load_checks(false, cx);
    }
    fn load_checks(&mut self, refresh_facts: bool, cx: &mut Context<Self>) {
        if self.checks.loading || self.checks.sending {
            return;
        }
        let Some(worker) = self.checks.worker.clone() else {
            return;
        };
        self.checks.request += 1;
        let request = self.checks.request;
        self.checks.loading = true;
        self.checks.report = None;
        self.checks.preview = None;
        self.checks.error = None;
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if refresh_facts {
                        client.call("refresh_facts", json!({"worker_id":worker}))?;
                    }
                    let value = client.call("worker_checks", json!({"worker_id":worker}))?;
                    Ok::<_, anyhow::Error>(serde_json::from_value::<ReadinessReport>(value)?)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.checks.request != request {
                    return;
                }
                this.checks.loading = false;
                match result {
                    Ok(report) => this.checks.report = Some(report),
                    Err(error) => this.checks.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn message_preview(&self, text: String) -> Option<SendPreview> {
        let report = self.checks.report.as_ref()?;
        let text = task_text(&text, 32000);
        Some(SendPreview {
            worker: report.worker.id.clone(),
            text: text.clone(),
            method: "message_worker",
            params: json!({"worker_id":report.worker.id,"message":text,"expected_git_head":report.git.as_ref().map(|git| &git.head),"expected_pr_head":report.worker.facts.head_sha}),
        })
    }
    fn checks_action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if self.checks.sending
            || (self.checks.loading && !matches!(action, Action::Close | Action::Open(_)))
        {
            return;
        }
        match action {
            Action::Refresh => self.load_checks(false, cx),
            Action::RefreshFacts => self.load_checks(true, cx),
            Action::Close => {
                self.checks.request += 1;
                self.checks.worker = None;
                self.checks.preview = None;
                self.right_tab = RightTab::Changes;
                self.workspace_focus.focus(window);
                if let Some(terminal) = &self.terminal {
                    terminal.read(cx).focus_handle().focus(window);
                } else if let Some(id) = self.focused_berth.clone() {
                    self.focus_worker_berth(&id, window, cx);
                }
            }
            Action::Preview(preview) => {
                self.checks.preview = Some(preview);
                self.checks.error = None;
            }
            Action::Cancel => {
                self.checks.preview = None;
                self.checks.error = None;
            }
            Action::Open(url) => cx.open_url(&url),
            Action::Fetch(method) => {
                if self.checks.loading {
                    return;
                }
                let Some(worker) = self.checks.worker.clone() else {
                    return;
                };
                let request = self.checks.request;
                let client = self.client.clone();
                self.checks.loading = true;
                self.checks.preview = None;
                self.checks.error = None;
                cx.spawn(async move |this,cx| {
                    let request_worker = worker.clone();
                    let result = cx.background_executor().spawn(async move { client.call(method,json!({"worker_id":request_worker})) }).await;
                    let _ = this.update(cx, |this,cx| {
                        if this.checks.request != request { return; }
                        this.checks.loading = false;
                        match result {
                            Ok(value) => {
                                if method == "ci_feedback" {
                                    match serde_json::from_value::<sigmadock_core::CiFeedback>(value) {
                                        Ok(report) => this.checks.preview = Some(SendPreview { worker:worker.clone(),text:report.text.clone(),method:"send_ci_feedback",params:json!({"worker_id":worker,"expected_text":report.text}) }),
                                        Err(error) => this.checks.error = Some(error.to_string()),
                                    }
                                } else if let Some(text) = value.as_str() { this.checks.preview = this.message_preview(text.into()); }
                                else { this.checks.error = Some("Invalid feedback response".into()); }
                            },
                            Err(error) => this.checks.error = Some(error.to_string()),
                        }
                        cx.notify();
                    });
                }).detach();
            }
            Action::Send => {
                let Some(preview) = self.checks.preview.clone() else {
                    return;
                };
                if self.checks.worker.as_ref() != Some(&preview.worker) {
                    return;
                }
                let request = self.checks.request;
                let client = self.client.clone();
                self.checks.sending = true;
                self.checks.error = None;
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { client.call(preview.method, preview.params) })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.checks.sending = false;
                        if this.checks.request != request {
                            return;
                        }
                        match result {
                            Ok(_) => {
                                this.checks.preview = None;
                                this.checks.error = Some("Feedback sent to this worker.".into());
                            }
                            Err(error) => this.checks.error = Some(error.to_string()),
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }
    fn checks_control(
        &self,
        id: String,
        label: &str,
        action: Action,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let keyboard_action = action.clone();
        div()
            .id(SharedString::from(id))
            .tab_index(0)
            .border_1()
            .border_color(gpui::transparent_black())
            .focus(|style| style.border_color(rgb(self.theme.focus)))
            .flex_none()
            .px_2()
            .py_1()
            .rounded_md()
            .text_xs()
            .bg(rgb(self.theme.button))
            .hover(|style| style.bg(rgb(self.theme.selection)))
            .cursor_pointer()
            .child(label.to_owned())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.checks_action(action.clone(), window, cx)
            }))
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.checks_action(keyboard_action.clone(), window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
    }
    /// Changes | Readiness switcher shared by both right-pane headers.
    pub(crate) fn right_tabs(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let blockers = self
            .checks
            .report
            .as_ref()
            .filter(|report| self.selected.as_ref() == Some(&report.worker.id))
            .map(|report| report.readiness().blockers.len());
        let tab = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_1p5()
                .px_2p5()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .text_sm()
                .when(on, |tab| {
                    tab.bg(rgb(theme.base)).font_weight(FontWeight::SEMIBOLD)
                })
                .when(!on, |tab| {
                    tab.text_color(rgb(theme.muted))
                        .hover(|style| style.bg(rgb(theme.panel)))
                })
                .child(label)
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                tab(
                    "tab-changes",
                    "Changes",
                    self.right_tab == RightTab::Changes,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.right_tab = RightTab::Changes;
                    cx.notify();
                })),
            )
            .child(
                tab(
                    "tab-readiness",
                    "Readiness",
                    self.right_tab == RightTab::Readiness,
                )
                .when_some(blockers.filter(|n| *n > 0), |tab, count| {
                    tab.child(
                        div()
                            .px_1p5()
                            .rounded_full()
                            .bg(rgba((theme.error << 8) | 0x22))
                            .text_xs()
                            .text_color(rgb(theme.error))
                            .child(if count == 1 {
                                "1 blocker".to_owned()
                            } else {
                                format!("{count} blockers")
                            }),
                    )
                })
                .tooltip(|_, cx| crate::keyboard_ui::tooltip("Readiness · ⌘⇧K".into(), cx))
                .on_click(cx.listener(|this, _, window, cx| {
                    if let Some(id) = this.selected.clone() {
                        this.open_checks(id, window, cx);
                    }
                })),
            )
            .into_any_element()
    }

    fn card_view(&self, card: &Card, details: Vec<gpui::AnyElement>) -> gpui::Div {
        let theme = self.theme;
        let (color, glyph) = match card.mark {
            Mark::Pass => (theme.success, Icon::Check),
            Mark::Fail => (theme.error, Icon::X),
            Mark::Pending => (theme.link, Icon::Refresh),
            Mark::Unknown => (theme.muted, Icon::Question),
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(if card.mark == Mark::Fail {
                rgba((theme.error << 8) | 0x66)
            } else {
                rgb(theme.border)
            })
            .bg(rgb(theme.base))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2p5()
                    .child(
                        div()
                            .size(px(22.))
                            .flex_none()
                            .rounded_full()
                            .bg(rgb(color))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(glyph, px(13.), rgb(theme.surface))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(card.title.clone()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(theme.muted))
                                    .child(card.summary.clone()),
                            ),
                    ),
            )
            .when(!details.is_empty(), |card| {
                card.child(
                    div()
                        .pl(px(34.))
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .children(details),
                )
            })
    }

    fn action_row(&self, buttons: Vec<gpui::Stateful<gpui::Div>>) -> Option<gpui::AnyElement> {
        (!buttons.is_empty()).then(|| {
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(buttons)
                .into_any_element()
        })
    }

    /// The Readiness tab: a checklist of what stands between this agent and a merge.
    pub(crate) fn readiness_pane(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let muted = |text: String| {
            div()
                .text_xs()
                .text_color(rgb(theme.muted))
                .child(text)
                .into_any_element()
        };
        let checked = self
            .checks
            .report
            .as_ref()
            .and_then(|report| report.ci.as_ref())
            .map(|ci| {
                format!(
                    "checked {}",
                    relative_time(ci.refreshed_at, unix_time()).to_lowercase()
                )
            });
        let header = div()
            .h(px(56.))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .border_b_1()
            .border_color(rgb(theme.border))
            .child(self.right_tabs(cx))
            .child(div().flex_1())
            .children(checked.map(|text| div().text_xs().text_color(rgb(theme.muted)).child(text)))
            .child(self.checks_control("checks-reload".into(), "Reload", Action::Refresh, cx))
            .child(self.checks_control(
                "checks-refresh-facts".into(),
                "Refresh from forge",
                Action::RefreshFacts,
                cx,
            ));
        let mut body = div()
            .id("readiness-body")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2p5()
            .p_3();
        if let Some(error) = &self.checks.error {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(rgb(theme.warning))
                    .child(error.clone()),
            );
        }
        if self.checks.loading {
            body = body.child(muted("Checking readiness…".into()));
        }
        if let Some(preview) = &self.checks.preview {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(theme.accent))
                    .bg(rgb(theme.base))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Will send to the agent"),
                    )
                    .child(
                        div()
                            .id("checks-preview-text")
                            .max_h(px(180.))
                            .overflow_y_scroll()
                            .p_2()
                            .rounded_md()
                            .bg(rgb(theme.panel))
                            .font_family("Menlo")
                            .text_xs()
                            .child(preview.text.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                self.checks_control(
                                    "checks-confirm-send".into(),
                                    if self.checks.sending {
                                        "Sending…"
                                    } else {
                                        "Send to agent"
                                    },
                                    Action::Send,
                                    cx,
                                )
                                .bg(rgb(theme.accent))
                                .text_color(rgb(theme.surface)),
                            )
                            .child(self.checks_control(
                                "checks-cancel-send".into(),
                                "Cancel",
                                Action::Cancel,
                                cx,
                            )),
                    ),
            );
        }
        let Some(report) = &self.checks.report else {
            return div()
                .id("readiness-pane")
                .track_focus(&self.checks_focus)
                .h_full()
                .flex()
                .flex_col()
                .bg(rgb(theme.sidebar))
                .border_l_1()
                .border_color(rgb(theme.border))
                .child(header)
                .child(body)
                .into_any_element();
        };
        let facts = &report.worker.facts;
        let cards = cards(report);
        // Git
        let mut git = Vec::new();
        if let Some(state) = &report.git {
            git.push(muted(format!(
                "HEAD {} · base {} (cached, not fetched)",
                short_sha(&state.head),
                state.base.as_deref().unwrap_or("unknown")
            )));
            for path in state.dirty.iter().take(8) {
                git.push(
                    div()
                        .text_xs()
                        .font_family("Menlo")
                        .ellipsis()
                        .child(path.clone())
                        .into_any_element(),
                );
            }
            if state.dirty.len() > 8 {
                git.push(muted(format!("and {} more", state.dirty.len() - 8)));
            }
            let mut buttons = Vec::new();
            for (key, show, label, text) in [
                (
                    "dirty",
                    !state.dirty.is_empty(),
                    "Ask agent to commit",
                    "Inspect the uncommitted changes in your worktree. Preserve user edits, finish the task, run relevant tests and commit only the intended changes.",
                ),
                (
                    "push",
                    state.unpushed.is_some_and(|n| n > 0),
                    "Ask agent to push",
                    "Inspect your local commits and remote branch. Run relevant checks and push the intended worker commits. Ask before rewriting remote history.",
                ),
                (
                    "behind",
                    state.behind.is_some_and(|n| n > 0),
                    "Ask agent to update from base",
                    "Fetch the PR target branch, inspect and preserve uncommitted changes, then bring your worker branch up to date and run relevant tests. Ask before rewriting remote history.",
                ),
            ] {
                if show && let Some(preview) = self.message_preview(text.into()) {
                    buttons.push(self.checks_control(
                        format!("checks-git-{key}"),
                        label,
                        Action::Preview(preview),
                        cx,
                    ));
                }
            }
            git.extend(self.action_row(buttons));
        }
        // Pull request
        let mut pr = Vec::new();
        let mut buttons = Vec::new();
        if let Some(url) = &facts.pr_url {
            buttons.push(self.checks_control(
                "checks-pr-link".into(),
                "Open PR",
                Action::Open(url.clone()),
                cx,
            ));
        }
        let instruction = match facts.pr {
            PullRequestState::None => Some((
                "Ask agent to open a PR",
                "Inspect the worker changes, run relevant tests, push the branch and open a pull request against the intended target branch. Include a concise description and validation results.",
            )),
            PullRequestState::Draft => Some((
                "Ask agent to finish the PR",
                "Inspect the draft pull request, finish outstanding work and run relevant tests. Mark it ready for review once the task is complete.",
            )),
            PullRequestState::Closed => Some((
                "Ask agent why it closed",
                "The observed pull request is closed. Inspect why it was closed and report the next step before reopening it or creating another pull request.",
            )),
            _ => None,
        };
        if let Some((label, text)) = instruction
            && let Some(preview) = self.message_preview(text.into())
        {
            buttons.push(self.checks_control(
                "checks-pr-plan".into(),
                label,
                Action::Preview(preview),
                cx,
            ));
        }
        if report.worker.forge.is_none() {
            pr.push(muted(
                "Connect a forge for this project to see pull requests and CI.".into(),
            ));
        }
        pr.extend(self.action_row(buttons));
        // CI
        let mut ci = Vec::new();
        if let Some(results) = &report.ci {
            for entry in results.entries.iter().filter(|e| e.state != "skipped") {
                let failed = matches!(entry.state.as_str(), "failed" | "cancelled");
                let (glyph, color) = match entry.state.as_str() {
                    "passed" => ("✓", theme.success),
                    "failed" | "cancelled" => ("✕", theme.error),
                    _ => ("•", theme.link),
                };
                let mut row = div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child(
                        div()
                            .w(px(12.))
                            .flex_none()
                            .text_color(rgb(color))
                            .child(glyph),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .ellipsis()
                            .child(entry.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(rgb(theme.muted))
                            .child(entry.state.clone()),
                    );
                if let Some(url) = &entry.url {
                    row = row.child(self.checks_control(
                        format!("checks-ci-source-{}", entry.id),
                        "Open",
                        Action::Open(url.clone()),
                        cx,
                    ));
                }
                ci.push(row.into_any_element());
                if failed && !entry.details.trim().is_empty() {
                    ci.push(
                        div()
                            .ml(px(20.))
                            .p_2()
                            .rounded_md()
                            .bg(rgb(theme.panel))
                            .font_family("Menlo")
                            .text_xs()
                            .text_color(rgb(theme.muted))
                            .child(task_text(&entry.details, 600))
                            .into_any_element(),
                    );
                }
            }
            for warning in &results.warnings {
                ci.push(muted(warning.clone()));
            }
        }
        if facts.checks == Checks::Failed {
            ci.extend(self.action_row(vec![self.checks_control(
                "checks-ci-all".into(),
                "Preview CI feedback for the agent",
                Action::Fetch("ci_feedback"),
                cx,
            )]));
        }
        // Review
        let mut review = Vec::new();
        if let Some(error) = &report.review_error {
            review.push(muted(error.clone()));
        }
        if let Some(results) = &report.review {
            let mut file = None;
            for comment in &results.comments {
                if file != Some(&comment.path) {
                    review.push(
                        div()
                            .text_xs()
                            .font_family("Menlo")
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(comment.path.clone())
                            .into_any_element(),
                    );
                    file = Some(&comment.path);
                }
                review.push(
                    div()
                        .text_sm()
                        .child(format!(
                            "{}{}",
                            comment
                                .line
                                .map_or_else(String::new, |line| format!("Line {line}: ")),
                            comment.body
                        ))
                        .into_any_element(),
                );
            }
            if !results.comments.is_empty()
                && let Some(preview) = self.message_preview(results.text.clone())
            {
                review.extend(self.action_row(vec![self.checks_control(
                    "checks-review-send".into(),
                    "Send comments to agent",
                    Action::Preview(preview),
                    cx,
                )]));
            }
        }
        if matches!(facts.review, Review::ChangesRequested | Review::Pending)
            && let Some(preview) = self.message_preview(if facts.review == Review::Pending {
                "The observed PR is awaiting approval. Inspect its review state and request review from the appropriate reviewer.".into()
            } else {
                "The review requests changes. Inspect the PR's review, address the requested changes, run relevant tests and report what you changed.".into()
            })
        {
            review.extend(self.action_row(vec![self.checks_control("checks-review-plan".into(), "Ask agent to handle review", Action::Preview(preview), cx)]));
        }
        // Conflicts
        let mut conflicts = Vec::new();
        if facts.mergeable == Some(false) {
            conflicts.extend(self.action_row(vec![self.checks_control(
                "checks-conflict-send".into(),
                "Ask agent to resolve",
                Action::Fetch("conflict_instruction"),
                cx,
            )]));
        }
        let readiness = report.readiness();
        let mut details = [git, pr, ci, review, conflicts].into_iter();
        for card in &cards {
            body = body.child(self.card_view(card, details.next().unwrap_or_default()));
        }
        for reason in &readiness.unknown {
            body = body.child(muted(format!("Not verified: {reason}")));
        }
        body = body.child(muted(
            "Advisory only: protected-branch rules aren't checked, and SigmaDock never merges for you.".into(),
        ));
        div()
            .id("readiness-pane")
            .track_focus(&self.checks_focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.checks_action(Action::Close, window, cx);
                    cx.stop_propagation();
                }
            }))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.sidebar))
            .border_l_1()
            .border_color(rgb(theme.border))
            .child(header)
            .child(body)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigmadock_core::{CiEntry, CiPreview, Facts, GitReadiness, Worker};

    fn report(facts: Facts, git: Option<GitReadiness>, ci: Option<CiPreview>) -> ReadinessReport {
        let worker: Worker = serde_json::from_value(serde_json::json!({
            "id": "w", "project_id": "p", "title": "t", "agent": "claude",
            "branch": "sigma/t", "worktree": "/tmp/w", "port": 1, "created_at": 0,
            "archived": false, "facts": facts, "forge": null,
        }))
        .unwrap();
        ReadinessReport {
            worker,
            git,
            git_error: None,
            ci,
            ci_error: None,
            review: None,
            review_error: None,
        }
    }

    fn git(dirty: usize, ahead: u64, behind: u64, unpushed: u64) -> GitReadiness {
        GitReadiness {
            head: "a91c2e4f00d".into(),
            base: Some("origin/main".into()),
            dirty: (0..dirty).map(|i| format!("file{i}.rs")).collect(),
            dirty_truncated: false,
            ahead: Some(ahead),
            behind: Some(behind),
            unpushed: Some(unpushed),
        }
    }

    fn entry(name: &str, state: &str) -> CiEntry {
        CiEntry {
            id: name.into(),
            kind: "check_run".into(),
            name: name.into(),
            state: state.into(),
            url: None,
            details: String::new(),
            truncated: false,
        }
    }

    #[test]
    fn clean_pushed_open_passing_work_reads_as_ready() {
        let facts = Facts {
            pr: PullRequestState::Open,
            checks: Checks::Passed,
            review: Review::Approved,
            mergeable: Some(true),
            pr_url: Some("https://github.com/o/r/pull/61".into()),
            ..Facts::default()
        };
        let ci = CiPreview {
            complete: true,
            head_sha: "a91c2e4f00d".into(),
            current_head: "a91c2e4f00d".into(),
            refreshed_at: 0,
            entries: vec![
                entry("test", "passed"),
                entry("lint", "passed"),
                entry("docs", "skipped"),
            ],
            warnings: Vec::new(),
            truncated: false,
        };
        let cards = cards(&report(facts, Some(git(0, 3, 0, 0)), Some(ci)));
        let titles: Vec<_> = cards.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Git", "Pull request #61", "CI", "Review", "Conflicts"]
        );
        assert!(
            cards.iter().all(|card| card.mark == Mark::Pass),
            "{cards:?}"
        );
        assert_eq!(
            cards[0].summary,
            "Clean worktree · 3 commits ahead, 0 behind · all pushed"
        );
        assert_eq!(cards[2].summary, "All 2 checks passed · commit a91c2e4");
    }

    #[test]
    fn blockers_are_described_in_words() {
        let facts = Facts {
            checks: Checks::Failed,
            review: Review::ChangesRequested,
            mergeable: Some(false),
            ..Facts::default()
        };
        let ci = CiPreview {
            complete: true,
            head_sha: "b".into(),
            current_head: "b".into(),
            refreshed_at: 0,
            entries: vec![
                entry("notarize", "failed"),
                entry("test", "passed"),
                entry("lint", "pending"),
            ],
            warnings: Vec::new(),
            truncated: false,
        };
        let cards = cards(&report(facts, Some(git(2, 1, 4, 1)), Some(ci)));
        assert_eq!(cards[0].mark, Mark::Fail);
        assert_eq!(
            cards[0].summary,
            "2 files uncommitted · 1 commit ahead, 4 behind · 1 commit unpushed"
        );
        assert_eq!(
            (cards[1].mark, cards[1].summary.as_str()),
            (Mark::Fail, "No pull request yet")
        );
        assert_eq!(
            (cards[2].mark, cards[2].summary.as_str()),
            (Mark::Fail, "1 of 3 checks failing · commit b")
        );
        assert_eq!(
            (cards[3].mark, cards[3].summary.as_str()),
            (Mark::Fail, "Changes requested")
        );
        assert_eq!(cards[4].mark, Mark::Fail);
        for card in &cards {
            assert!(
                !card.summary.contains("Some(") && !card.summary.contains("None"),
                "{card:?}"
            );
        }
    }

    #[test]
    fn missing_data_is_unknown_not_failed() {
        let cards = cards(&report(Facts::default(), None, None));
        assert_eq!(cards[0].mark, Mark::Unknown);
        assert_eq!(
            (cards[2].mark, cards[2].summary.as_str()),
            (Mark::Unknown, "No check results")
        );
        assert_eq!(cards[3].summary, "No review yet");
        assert_eq!(cards[4].summary, "Mergeability not reported yet");
        assert_eq!(short_sha("abc"), "abc");
    }
}
