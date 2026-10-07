//! On-demand checks and exact feedback previews for one worker.
use crate::{Workspace, theme::Theme};
use gpui::{Context, SharedString, Window, div, prelude::*, px, rgb};
use serde_json::{Value, json};
use sigmadock_core::{PullRequestState, ReadinessReport, Review, task_text};

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

fn section(name: &str, theme: Theme) -> gpui::Div {
    div()
        .p_3()
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
            .max_h(px(540.))
            .overflow_y_scroll()
            .p_3()
            .rounded_md()
            .bg(rgb(self.theme.panel))
            .flex()
            .flex_col()
            .gap_3();
        let title = self
            .checks
            .report
            .as_ref()
            .map_or("Checks · Unknown".into(), |report| {
                format!(
                    "Checks · {} · {}",
                    report.worker.title,
                    report.readiness().label()
                )
            });
        panel = panel.child(div().flex().items_center().gap_2().child(div().flex_1().text_lg().child(title))
            .child(self.checks_control("checks-reload".into(),"Reload",Action::Refresh,cx))
            .child(self.checks_control("checks-refresh-facts".into(),"Refresh forge facts",Action::RefreshFacts,cx))
            .child(self.checks_control("checks-close".into(),"Close · Esc",Action::Close,cx)))
            .child("Advisory snapshot. Protected-branch rules are not verified. SigmaDock never merges automatically.");
        if let Some(error) = &self.checks.error {
            panel = panel.child(error.clone());
        }
        if self.checks.loading {
            panel = panel.child("Loading checks…");
        }
        if let Some(preview) = &self.checks.preview {
            panel = panel.child(
                section("Preview — exact text to send", self.theme)
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
                                "checks-cancel-send".into(),
                                "Cancel",
                                Action::Cancel,
                                cx,
                            )),
                    ),
            );
        }
        let Some(report) = &self.checks.report else {
            return panel.into_any_element();
        };
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
        let mut git = section("1. Git", self.theme);
        if let Some(state) = &report.git {
            git = git
                .child(format!(
                    "HEAD {} · base {} (cached; no fetch)",
                    state.head,
                    state.base.as_deref().unwrap_or("unknown")
                ))
                .child(format!(
                    "Ahead {} · behind {} · unpushed {}",
                    number(state.ahead),
                    number(state.behind),
                    number(state.unpushed)
                ));
            if state.dirty.is_empty() {
                git = git.child("No uncommitted changes");
            }
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
                if show && let Some(preview) = self.message_preview(text.into()) {
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
        panel = panel.child(git);
        let facts = &report.worker.facts;
        let mut pr =
            section("2. Pull request", self.theme).child(format!("Observed state: {:?}", facts.pr));
        if let Some(url) = &facts.pr_url {
            pr = pr.child(self.checks_control(
                "checks-pr-link".into(),
                "Open pull request",
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
            && let Some(preview) = self.message_preview(text.into())
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
        panel = panel.child(pr);
        let mut ci =
            section("3. CI", self.theme).child(format!("Observed checks: {:?}", facts.checks));
        if let Some(error) = &report.ci_error {
            ci = ci.child(format!("Unknown: {error}"));
        }
        if let Some(results) = &report.ci {
            ci = ci.child(format!(
                "Commit {} · inspected at Unix {}",
                results.head_sha, results.refreshed_at
            ));
            for entry in &results.entries {
                let mut check = div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(format!("{} · {}", entry.name, entry.state));
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
                        "Open check",
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
        panel = panel.child(ci);
        let mut review =
            section("4. Review", self.theme).child(format!("Observed review: {:?}", facts.review));
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
                && let Some(preview) = self.message_preview(results.text.clone())
            {
                review = review.child(self.checks_control(
                    "checks-review-send".into(),
                    "Send all review comments — preview",
                    Action::Preview(preview),
                    cx,
                ));
            }
        }
        if matches!(facts.review, Review::ChangesRequested | Review::Pending) && let Some(preview) = self.message_preview(if facts.review == Review::Pending { "The observed PR is awaiting approval. Inspect its review state and request review from the appropriate reviewer.".into() } else { "The review requests changes. Inspect the PR's review, address the requested changes, run relevant tests and report what you changed.".into() }) {
            review = review.child(self.checks_control("checks-review-plan".into(),"Send review plan — preview",Action::Preview(preview),cx));
        }
        panel = panel.child(review);
        let mut conflicts = section("5. Conflicts", self.theme).child(match facts.mergeable {
            Some(false) => "Merge conflict reported",
            Some(true) => "No merge conflict observed",
            None => "Unknown mergeability",
        });
        if facts.mergeable == Some(false) {
            conflicts = conflicts.child(self.checks_control(
                "checks-conflict-send".into(),
                "Send conflict instruction — preview",
                Action::Fetch("conflict_instruction"),
                cx,
            ));
        }
        panel.child(conflicts).into_any_element()
    }
}
fn number(number: Option<u64>) -> String {
    number.map_or_else(|| "unknown".into(), |n| n.to_string())
}
