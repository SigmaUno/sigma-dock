//! On-demand checks and exact feedback previews for one worker.
use crate::{Workspace, theme::Theme};
use gpui::{Context, SharedString, Window, div, prelude::*, px, rgb};
use serde_json::{Value, json};
use sigmadock_core::{Checks, PullRequestState, ReadinessReport, Review, task_text};

#[derive(Default)]
pub(crate) struct ChecksPane {
    pub worker: Option<String>,
    report: Option<ReadinessReport>,
    loading: bool,
    sending: bool,
    error: Option<String>,
    preview: Option<SendPreview>,
    request: u64,
    checked_at: Option<u64>,
}
impl ChecksPane {
    pub(crate) fn reset_for_worker(&mut self, worker: String) {
        let request = self.request + 1;
        let sending = self.sending;
        *self = Self {
            worker: Some(worker),
            request,
            sending,
            ..Self::default()
        };
    }
    pub(crate) fn blocker_count(&self) -> usize {
        self.report
            .as_ref()
            .map_or(0, |report| report.readiness().blockers.len())
    }
}
#[derive(Clone)]
struct SendPreview {
    section: &'static str,
    worker: String,
    text: String,
    method: &'static str,
    params: Value,
}
#[derive(Clone)]
enum Action {
    RefreshFacts,
    Close,
    Preview(SendPreview),
    Fetch(&'static str),
    Send,
    Cancel,
    Open(String),
    Copy(String),
}

fn section(name: &str, theme: Theme) -> gpui::Div {
    div()
        .p_3()
        .flex_none()
        .rounded_md()
        .bg(rgb(theme.card))
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(name.to_owned()),
        )
}
impl Workspace {
    pub(crate) fn open_checks(
        &mut self,
        worker: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected.as_ref() != Some(&worker) || self.terminal.is_none() {
            self.open_worker(worker.clone(), window, cx);
        }
        if self.checks.worker.as_ref() != Some(&worker) {
            self.checks.reset_for_worker(worker);
        }
        self.agent_tab = crate::agent_ui::AgentTab::Readiness;
        self.checks_focus.focus(window);
        self.load_checks(false, cx);
        cx.notify();
    }
    pub(crate) fn load_checks(&mut self, refresh_facts: bool, cx: &mut Context<Self>) {
        if self.checks.loading || self.checks.sending {
            return;
        }
        let Some(worker) = self.checks.worker.clone() else {
            return;
        };
        self.checks.request += 1;
        let request = self.checks.request;
        self.checks.loading = true;
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
                    Ok(report) => {
                        this.checks.report = Some(report);
                        this.checks.checked_at = Some(sigmadock_core::unix_time());
                    }
                    Err(error) => this.checks.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn message_preview(&self, section: &'static str, text: String) -> Option<SendPreview> {
        let report = self.checks.report.as_ref()?;
        let text = task_text(&text, 32000);
        Some(SendPreview {
            section,
            worker: report.worker.id.clone(),
            text: text.clone(),
            method: "message_worker",
            params: json!({"worker_id":report.worker.id,"message":text,"expected_git_head":report.git.as_ref().map(|git| &git.head),"expected_pr_head":report.worker.facts.head_sha}),
        })
    }
    fn checks_action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if self.checks.sending
            || (self.checks.loading
                && !matches!(action, Action::Close | Action::Open(_) | Action::Copy(_)))
        {
            return;
        }
        match action {
            Action::RefreshFacts => self.load_checks(true, cx),
            Action::Close => {
                self.checks.request += 1;
                self.checks.loading = false;
                self.agent_tab = crate::agent_ui::AgentTab::Changes;
                self.checks.preview = None;
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
            Action::Copy(text) => cx.write_to_clipboard(gpui::ClipboardItem::new_string(text)),
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
                                        Ok(report) => this.checks.preview = Some(SendPreview { section:"CI", worker:worker.clone(),text:report.text.clone(),method:"send_ci_feedback",params:json!({"worker_id":worker,"expected_text":report.text}) }),
                                        Err(error) => this.checks.error = Some(error.to_string()),
                                    }
                                } else if let Some(text) = value.as_str() { this.checks.preview = this.message_preview("Review", text.into()); }
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
                            if this.selected == this.checks.worker && this.terminal.is_some() {
                                this.load_checks(false, cx);
                            }
                            cx.notify();
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
            .p_2()
            .rounded_md()
            .bg(rgb(self.theme.button))
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
    fn readiness_preview(&self, section_name: &str, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let preview = self
            .checks
            .preview
            .as_ref()
            .filter(|preview| preview.section == section_name)?;
        Some(
            section("Exact text to send", self.theme)
                .child(
                    div()
                        .id("checks-preview-text")
                        .max_h(px(180.))
                        .overflow_y_scroll()
                        .font_family("monospace")
                        .text_sm()
                        .child(preview.text.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(self.checks_control(
                            "checks-confirm-send".into(),
                            if self.checks.sending {
                                "Sending…"
                            } else {
                                "Send to agent"
                            },
                            Action::Send,
                            cx,
                        ))
                        .child(self.checks_control(
                            "checks-copy-preview".into(),
                            "Copy",
                            Action::Copy(preview.text.clone()),
                            cx,
                        ))
                        .child(self.checks_control(
                            "checks-cancel-send".into(),
                            "Cancel",
                            Action::Cancel,
                            cx,
                        )),
                ),
        )
    }
    pub(crate) fn checks_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut panel = div()
            .id("worker-checks-pane")
            .track_focus(&self.checks_focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.checks_action(Action::Close, window, cx);
                    cx.stop_propagation();
                }
            }))
            .h_full()
            .overflow_y_scroll()
            .p_3()
            .rounded_md()
            .bg(rgb(self.theme.panel))
            .flex()
            .flex_col()
            .gap_3();
        let base = self
            .checks
            .report
            .as_ref()
            .and_then(|report| report.git.as_ref())
            .and_then(|git| git.base.as_deref())
            .unwrap_or("unknown base");
        let checked = self.checks.checked_at.map_or_else(
            || "not checked yet".into(),
            |at| checked_age(at, sigmadock_core::unix_time()),
        );
        panel = panel.child(div().flex().flex_wrap().items_center().gap_2()
            .child(div().flex_1().child(format!("Merge readiness · {checked} against {base}")))
            .child(self.checks_control("checks-refresh-facts".into(), "Refresh", Action::RefreshFacts, cx)))
            .child(div().text_sm().text_color(rgb(self.theme.muted)).child("Advisory snapshot; base comparison uses cached refs. Protected-branch rules are not verified."));
        if let Some(error) = &self.checks.error {
            panel = panel.child(error.clone());
        }
        if self.checks.loading {
            panel = panel.child("Loading readiness…");
        }
        let Some(report) = &self.checks.report else {
            return panel.into_any_element();
        };
        let stale = self
            .workers
            .iter()
            .find(|worker| worker.id == report.worker.id)
            .is_some_and(|worker| worker.facts.head_sha != report.worker.facts.head_sha);
        if stale {
            panel = panel.child(div().text_color(rgb(self.theme.warning)).child(
                "The pull request commit has changed. Refresh readiness before using feedback.",
            ));
        }
        let readiness = report.readiness();
        for (kind, reason) in &readiness.blockers {
            panel = panel.child(format!("{kind}: {reason}"));
        }
        for reason in &readiness.unknown {
            panel = panel.child(
                div()
                    .text_color(rgb(self.theme.warning))
                    .child(format!("Unknown: {reason}")),
            );
        }
        let mut git = readiness_card("Git", &["Git"], &readiness, self.theme);
        if let Some(state) = &report.git {
            git = git
                .child(format!(
                    "Commit {} · base {} (cached; no fetch)",
                    short_commit(&state.head),
                    state.base.as_deref().unwrap_or("unknown")
                ))
                .child(format!(
                    "Ahead {} · behind {} · unpushed {}",
                    number(state.ahead),
                    number(state.behind),
                    number(state.unpushed)
                ));
            if state.dirty.is_empty() {
                git = git.child(if state.dirty_truncated {
                    "Changed-file list incomplete"
                } else {
                    "Worktree clean"
                });
            }
            git = git.child(match state.unpushed {
                Some(0) => "All commits pushed",
                Some(_) => "Local commits still need pushing",
                None => "Push status unavailable",
            });
            for path in &state.dirty {
                git = git.child(path.clone());
            }
            for (key, show, label, text) in [
                (
                    "dirty",
                    !state.dirty.is_empty(),
                    "Send uncommitted-changes plan",
                    "Inspect the uncommitted changes in your worktree. Preserve user edits, finish the task, run relevant tests and commit only the intended changes.",
                ),
                (
                    "push",
                    state.unpushed.is_some_and(|n| n > 0),
                    "Send unpushed-commits plan",
                    "Inspect your local commits and remote branch. Run relevant checks and push the intended worker commits. Ask before rewriting remote history.",
                ),
                (
                    "behind",
                    state.behind.is_some_and(|n| n > 0),
                    "Send behind-base plan",
                    "Fetch the PR target branch, inspect and preserve uncommitted changes, then bring your worker branch up to date and run relevant tests. Ask before rewriting remote history.",
                ),
            ] {
                if show && let Some(preview) = self.message_preview("Git", text.into()) {
                    git = git.child(self.checks_control(
                        format!("checks-git-{key}"),
                        label,
                        Action::Preview(preview),
                        cx,
                    ));
                }
            }
        } else {
            git = git.child(format!(
                "Unknown: {}",
                report
                    .git_error
                    .as_deref()
                    .unwrap_or("Git state unavailable")
            ));
        }
        panel = panel.child(git.children(self.readiness_preview("Git", cx)));
        let facts = &report.worker.facts;
        let mut pr = readiness_card("Pull request", &["Pull request"], &readiness, self.theme)
            .child(pr_label(&facts.pr));
        if let Some(url) = &facts.pr_url {
            if let Some(number) = url
                .rsplit('/')
                .find(|part| !part.is_empty())
                .filter(|part| part.chars().all(|ch| ch.is_ascii_digit()))
            {
                pr = pr.child(format!("Pull request #{number}"));
            }
            pr = pr.child(self.checks_control(
                "checks-pr-link".into(),
                "Open PR",
                Action::Open(url.clone()),
                cx,
            ));
        }
        let instruction = match facts.pr {
            PullRequestState::None => Some((
                "Ask agent to open PR",
                "Inspect the worker changes, run relevant tests, push the branch and open a pull request against the intended target branch. Include a concise description and validation results.",
            )),
            PullRequestState::Draft => Some((
                "Ask agent to prepare PR",
                "Inspect the draft pull request, finish outstanding work and run relevant tests. Mark it ready for review once the task is complete.",
            )),
            PullRequestState::Closed => Some((
                "Ask agent to inspect closed PR",
                "The observed pull request is closed. Inspect why it was closed and report the next step before reopening it or creating another pull request.",
            )),
            _ => None,
        };
        if let Some((label, text)) = instruction
            && let Some(preview) = self.message_preview("Pull request", text.into())
        {
            pr = pr.child(self.checks_control(
                "checks-pr-plan".into(),
                label,
                Action::Preview(preview),
                cx,
            ));
        }
        if let Some(error) = &facts.forge_error {
            pr = pr.child(format!("Forge error: {error}"));
        }
        panel = panel.child(pr.children(self.readiness_preview("Pull request", cx)));
        let mut ci = readiness_card("CI", &["CI"], &readiness, self.theme)
            .child(checks_label(&facts.checks));
        if let Some(error) = &report.ci_error {
            ci = ci.child(format!("Unknown: {error}"));
        }
        if let Some(results) = &report.ci {
            ci = ci.child(format!(
                "{} of {} checks failing · commit {} · {}",
                results
                    .entries
                    .iter()
                    .filter(|entry| matches!(entry.state.as_str(), "failed" | "cancelled"))
                    .count(),
                results.entries.len(),
                short_commit(&results.head_sha),
                checked_age(results.refreshed_at, sigmadock_core::unix_time())
            ));
            for entry in &results.entries {
                let mut check = div().flex().flex_col().gap_1().child(format!(
                    "{} · {}",
                    entry.name,
                    ci_label(&entry.state)
                ));
                if matches!(entry.state.as_str(), "failed" | "cancelled") {
                    check = check
                        .child(
                            div()
                                .font_family("monospace")
                                .text_sm()
                                .child(entry.details.clone()),
                        )
                        .child(self.checks_control(
                            format!("checks-ci-{}", entry.id),
                            "Send to agent — preview",
                            Action::Fetch("ci_feedback"),
                            cx,
                        ));
                }
                if let Some(url) = &entry.url {
                    check = check.child(self.checks_control(
                        format!("checks-ci-source-{}", entry.id),
                        "Open run",
                        Action::Open(url.clone()),
                        cx,
                    ));
                }
                ci = ci.child(check);
            }
            for warning in &results.warnings {
                ci = ci.child(warning.clone());
            }
        }
        if facts.checks == sigmadock_core::Checks::Failed {
            ci = ci.child(self.checks_control(
                "checks-ci-all".into(),
                "Preview CI feedback",
                Action::Fetch("ci_feedback"),
                cx,
            ));
        }
        panel = panel.child(ci.children(self.readiness_preview("CI", cx)));
        let mut review = readiness_card("Review", &["Review", "Conflicts"], &readiness, self.theme)
            .child(review_label(&facts.review));
        if let Some(error) = &report.review_error {
            review = review.child(format!("Unknown: {error}"));
        }
        if let Some(results) = &report.review {
            let mut file = None;
            if results.comments.is_empty() {
                review = review.child("No unresolved inline comments returned");
            }
            for comment in &results.comments {
                if file != Some(&comment.path) {
                    review = review.child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(comment.path.clone()),
                    );
                    file = Some(&comment.path);
                }
                review = review.child(format!(
                    "Line {} · {}\n{}",
                    number(comment.line),
                    if comment.resolved == Some(false) {
                        "Unresolved"
                    } else {
                        "Resolution unknown"
                    },
                    comment.body
                ));
            }
            if !results.comments.is_empty()
                && let Some(preview) = self.message_preview("Review", results.text.clone())
            {
                review = review.child(self.checks_control(
                    "checks-review-send".into(),
                    "Send all review comments — preview",
                    Action::Preview(preview),
                    cx,
                ));
            }
        }
        if matches!(facts.review, Review::ChangesRequested | Review::Pending) && let Some(preview) = self.message_preview("Review", if facts.review == Review::Pending { "The observed PR is awaiting approval. Inspect its review state and request review from the appropriate reviewer.".into() } else { "The review requests changes. Inspect the PR's review, address the requested changes, run relevant tests and report what you changed.".into() }) {
            review = review.child(self.checks_control("checks-review-plan".into(),"Send review plan — preview",Action::Preview(preview),cx));
        }
        review = review.child(match facts.mergeable {
            Some(false) => "Merge conflict reported",
            Some(true) => "No merge conflict observed",
            None => "Unknown mergeability",
        });
        if facts.mergeable == Some(false) {
            review = review.child(self.checks_control(
                "checks-conflict-send".into(),
                "Send conflict instruction — preview",
                Action::Fetch("conflict_instruction"),
                cx,
            ));
        }
        panel
            .child(review.children(self.readiness_preview("Review", cx)))
            .into_any_element()
    }
}
fn number(number: Option<u64>) -> String {
    number.map_or_else(|| "unknown".into(), |n| n.to_string())
}

fn checked_age(at: u64, now: u64) -> String {
    let seconds = now.saturating_sub(at);
    match seconds {
        0..60 => format!("checked {seconds}s ago"),
        60..3600 => format!("checked {}m ago", seconds / 60),
        3600..86400 => format!("checked {}h ago", seconds / 3600),
        _ => format!("checked {}d ago", seconds / 86400),
    }
}
fn short_commit(commit: &str) -> String {
    commit.chars().take(12).collect()
}
fn pr_label(state: &PullRequestState) -> &'static str {
    match state {
        PullRequestState::None => "No pull request observed",
        PullRequestState::Draft => "Draft · not ready for review",
        PullRequestState::Open => "Open for review",
        PullRequestState::Merged => "Already merged",
        PullRequestState::Closed => "Closed without merging",
    }
}
fn checks_label(state: &Checks) -> &'static str {
    match state {
        Checks::Unknown => "Checks unavailable",
        Checks::Pending => "Checks pending",
        Checks::Passed => "Checks passed",
        Checks::Failed => "Checks failing",
    }
}
fn review_label(state: &Review) -> &'static str {
    match state {
        Review::Unknown => "Review decision unavailable",
        Review::Pending => "Awaiting approval",
        Review::Approved => "Approved",
        Review::ChangesRequested => "Changes requested",
    }
}
fn ci_label(state: &str) -> &'static str {
    match state {
        "passed" => "Passed",
        "failed" => "Failed",
        "cancelled" => "Cancelled",
        "running" => "Running",
        "pending" => "Pending",
        _ => "Unknown",
    }
}
fn card_state(sections: &[&str], readiness: &sigmadock_core::Readiness) -> &'static str {
    if readiness
        .blockers
        .iter()
        .any(|(section, _)| sections.contains(section))
    {
        "Blocked"
    } else if !readiness.unknown.is_empty() {
        "Unknown"
    } else {
        "Passed"
    }
}
fn readiness_card(
    name: &str,
    sections: &[&str],
    readiness: &sigmadock_core::Readiness,
    theme: Theme,
) -> gpui::Div {
    let state = card_state(sections, readiness);
    let (symbol, color) = match state {
        "Blocked" => ("×", theme.error),
        "Passed" => ("✓", theme.success),
        _ => ("?", theme.warning),
    };
    section(&format!("{symbol}  {name} · {state}"), theme)
        .border_l_2()
        .border_color(rgb(color))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_times_are_relative_and_handle_clock_skew() {
        assert_eq!(checked_age(100, 112), "checked 12s ago");
        assert_eq!(checked_age(100, 90), "checked 0s ago");
        assert_eq!(checked_age(100, 220), "checked 2m ago");
        assert_eq!(checked_age(100, 7300), "checked 2h ago");
        assert_eq!(checked_age(100, 172900), "checked 2d ago");
    }
    #[test]
    fn incomplete_snapshots_never_show_a_passing_card() {
        let mut readiness = sigmadock_core::Readiness {
            blockers: vec![],
            unknown: vec!["CI unavailable".into()],
        };
        assert_eq!(card_state(&["Git"], &readiness), "Unknown");
        readiness
            .blockers
            .push(("Review", "Changes requested".into()));
        assert_eq!(card_state(&["Review", "Conflicts"], &readiness), "Blocked");
        readiness.unknown.clear();
        assert_eq!(card_state(&["Git"], &readiness), "Passed");
    }
    #[test]
    fn worker_switch_discards_feedback_and_invalidates_pending_results() {
        let mut pane = ChecksPane {
            worker: Some("old".into()),
            request: 4,
            loading: true,
            sending: true,
            preview: Some(SendPreview {
                section: "Git",
                worker: "old".into(),
                text: "old feedback".into(),
                method: "message_worker",
                params: json!({}),
            }),
            ..Default::default()
        };
        pane.reset_for_worker("new".into());
        assert_eq!(pane.worker.as_deref(), Some("new"));
        assert_eq!(pane.request, 5);
        assert!(pane.preview.is_none());
        assert!(!pane.loading);
        assert!(pane.sending);
    }
    #[test]
    fn labels_expand_multiword_states() {
        assert_eq!(review_label(&Review::ChangesRequested), "Changes requested");
        assert_eq!(
            pr_label(&PullRequestState::None),
            "No pull request observed"
        );
        assert_eq!(checks_label(&Checks::Unknown), "Checks unavailable");
        assert_eq!(ci_label("unrecognised"), "Unknown");
    }
}
