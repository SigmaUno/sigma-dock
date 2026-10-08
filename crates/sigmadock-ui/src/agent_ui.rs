//! Agent view: the worker's terminal session in the middle and its changes on the right.
use crate::ellipsis::Ellipsis;
use crate::{
    Menu, Workspace,
    berths_ui::{Action, status},
    editor::{self, Editor},
    icons::{Icon, app_icon, icon},
};
use gpui::{
    AnyElement, Context, Corner, Div, FontWeight, SharedString, Stateful, anchored, deferred, div,
    prelude::*, px, relative, rgb, rgba,
};
use serde_json::json;
use sigmadock_core::{SessionState, Worker};

const MONO: &str = "Menlo";

/// How much faster the branch chip shrinks than the title when the header runs out of room.
const BRANCH_SHRINK: f32 = 8.;

impl Workspace {
    pub(crate) fn header_button(&self, id: impl Into<gpui::ElementId>) -> Stateful<Div> {
        let theme = self.theme;
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .gap_1p5()
            .h(px(30.))
            .px_3()
            .rounded_md()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(theme.panel)))
            .text_sm()
            .whitespace_nowrap()
    }

    fn menu_item(&self, id: SharedString, label: String, checked: bool) -> Stateful<Div> {
        let theme = self.theme;
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .hover(|style| style.bg(rgb(theme.panel)))
            .child(div().w(px(14.)).when(checked, |slot| {
                slot.child(icon(Icon::Check, px(14.), rgb(theme.accent)))
            }))
            .child(label)
    }

    fn popover(&self, items: Vec<AnyElement>) -> AnyElement {
        let theme = self.theme;
        deferred(
            anchored().anchor(Corner::TopRight).snap_to_window().child(
                div()
                    .id("popover")
                    .mt_1()
                    .w(px(220.))
                    .p_1()
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(theme.border))
                    .bg(rgb(theme.surface))
                    .shadow_lg()
                    // Keep the root's click-away handler from closing the menu first.
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .children(items),
            ),
        )
        .with_priority(1)
        .into_any_element()
    }

    fn choose_editor(&mut self, editor: Option<Editor>, cx: &mut Context<Self>) {
        self.editor_override = editor;
        self.menu = None;
        cx.notify();
    }

    fn editor_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.editor_override;
        let mut items = Vec::new();
        let mut choices: Vec<Editor> = editor::detected();
        if !choices.contains(&Editor::Environment) {
            choices.push(Editor::Environment);
        }
        choices.push(Editor::System);
        if !self.preferences.editor.custom_command.trim().is_empty() {
            choices.push(Editor::Custom);
        }
        for choice in choices {
            items.push(
                self.menu_item(
                    SharedString::from(format!("editor-{choice:?}")),
                    choice.label().into(),
                    current == Some(choice),
                )
                .child(crate::editor_icons::editor_icon(
                    choice,
                    px(16.),
                    self.theme.text,
                ))
                .on_click(cx.listener(move |this, _, _, cx| this.choose_editor(Some(choice), cx)))
                .into_any_element(),
            );
        }
        items.push(
            self.menu_item(
                "editor-auto".into(),
                format!(
                    "Saved default ({})",
                    self.preferences.editor.resolved().label()
                ),
                current.is_none(),
            )
            .on_click(cx.listener(|this, _, _, cx| this.choose_editor(None, cx)))
            .into_any_element(),
        );
        items.push(
            div()
                .h(px(1.))
                .my_1()
                .bg(rgb(self.theme.border))
                .into_any_element(),
        );
        items.push(
            self.menu_item("editor-settings".into(), "Editor settings…".into(), false)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.menu = None;
                    this.open_settings(crate::settings_ui::Section::Editor, window, cx);
                }))
                .into_any_element(),
        );
        self.popover(items)
    }

    fn more_menu(&self, worker: &Worker, cx: &mut Context<Self>) -> AnyElement {
        let id = worker.id.clone();
        let mut items = vec![
            self.menu_item("more-fork".into(), "Fork…".into(), false)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.open_fork(id.clone(), window, cx)),
                )
                .into_any_element(),
        ];
        for (label, method) in [
            ("Merge readiness", "worker_checks"),
            ("Usage", "agent_usage"),
            ("CI preview", "ci_feedback"),
            ("Send CI to agent", "send_ci_feedback"),
            ("Review feedback", "review_feedback"),
            ("Conflict plan", "conflict_instruction"),
            ("Diff summary", "diff"),
            ("Archive", "archive_worker"),
        ] {
            let id = worker.id.clone();
            items.push(
                self.menu_item(
                    SharedString::from(format!("more-{method}")),
                    label.into(),
                    false,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.menu = None;
                    match method {
                        "worker_checks" => this.open_checks(id.clone(), window, cx),
                        "agent_usage" => this.load_usage(cx),
                        "ci_feedback" => this.load_ci(cx),
                        _ => this.run_action(method, json!({"worker_id": id}), cx),
                    }
                }))
                .into_any_element(),
            );
        }
        let id = worker.id.clone();
        items.push(
            self.menu_item(
                "more-session-summary".into(),
                self.summary_label(&worker.id, "Copy session summary")
                    .into(),
                false,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.menu = None;
                this.copy_summary(id.clone(), cx);
            }))
            .into_any_element(),
        );
        self.popover(items)
    }

    fn agent_header(&self, worker: Option<&Worker>, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let mut header = div()
            .h(px(56.))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .border_b_1()
            .border_color(rgb(theme.border))
            .child(
                div()
                    .id("close-terminal")
                    .flex_none()
                    .p_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(theme.panel)))
                    .child(icon(Icon::ArrowLeft, px(16.), rgb(theme.muted)))
                    .tooltip(|_, cx| crate::keyboard_ui::tooltip("Back to agents · ⌘[".into(), cx))
                    .on_click(cx.listener(|this, _, window, cx| this.close_terminal(window, cx))),
            )
            .child(app_icon(px(22.)));
        let Some(worker) = worker else {
            return header
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Session ended"),
                )
                .into_any_element();
        };
        let status = status(worker);
        let color = self.tone_color(status.tone);
        let id = worker.id.clone();
        let running = !matches!(
            worker.facts.session,
            SessionState::Exited | SessionState::Lost
        );
        header = header
            .when_some(worker.forked_from.as_deref(), |header, source| {
                header.child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme.muted))
                        .child(self.fork_lineage(source)),
                )
            })
            .child(
                // Short titles keep their natural width; the branch chip gives way first.
                div()
                    .id("agent-title")
                    .min_w(px(0.))
                    .flex_shrink()
                    .ellipsis()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(worker.title.clone())
                    .tooltip({
                        let text = format!("{} · {}", worker.title, worker.branch);
                        move |_, cx| crate::keyboard_ui::tooltip(text.clone(), cx)
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_0p5()
                    .rounded_full()
                    .bg(rgba((color << 8) | 0x26))
                    .text_xs()
                    .text_color(rgb(color))
                    .child(div().size(px(6.)).rounded_full().bg(rgb(color)))
                    .child(status.pill.clone()),
            )
            // Narrow windows keep the title readable; the title's tooltip names the branch.
            .when(!self.compact, |header| {
                header.child(
                    div()
                        .flex()
                        .min_w(px(0.))
                        .max_w(px(160.))
                        .map(|mut chip| {
                            chip.style().flex_shrink = Some(BRANCH_SHRINK);
                            chip
                        })
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_0p5()
                        .rounded_full()
                        .bg(rgb(theme.chip))
                        .text_xs()
                        .font_family(MONO)
                        .text_color(rgb(theme.muted))
                        .child(icon(Icon::GitBranch, px(12.), rgb(theme.muted)))
                        .child(div().ellipsis().child(worker.branch.clone())),
                )
            })
            .child(div().flex_1());
        let stop_id = id.clone();
        header = header.child(if running {
            self.header_button("stop-worker")
                .px_2()
                .child(icon(Icon::Stop, px(14.), rgb(theme.muted)))
                .tooltip(|_, cx| crate::keyboard_ui::tooltip("Stop agent".into(), cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.run_action("stop_worker", json!({"worker_id": stop_id}), cx)
                }))
        } else {
            self.header_button("resume-worker")
                .child("Resume")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.run_action("resume_worker", json!({"worker_id": stop_id}), cx)
                }))
        });
        let editor = self.active_editor();
        let editor_open = self.menu == Some(Menu::Editor);
        header =
            header.child(
                div()
                    .flex()
                    .flex_none()
                    .child(
                        self.header_button("open-in-editor")
                            .rounded_r_none()
                            .child(crate::editor_icons::editor_icon(
                                editor,
                                px(14.),
                                theme.accent,
                            ))
                            .when(!self.compact, |button| {
                                button.child(format!("Open in {}", editor.label()))
                            })
                            .tooltip(move |_, cx| {
                                crate::keyboard_ui::tooltip(
                                    format!("Open the worktree in {} · ⌘⇧O", editor.label()),
                                    cx,
                                )
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_in_editor(None, window, cx)
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                self.header_button("choose-editor")
                                    .px_1p5()
                                    .rounded_l_none()
                                    .border_l_0()
                                    .child(icon(Icon::ChevronDown, px(14.), rgb(theme.muted)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.menu = (!editor_open).then_some(Menu::Editor);
                                        cx.notify();
                                    })),
                            )
                            .when(editor_open, |anchor| anchor.child(self.editor_menu(cx))),
                    ),
            );
        let primary = worker
            .facts
            .pr_url
            .clone()
            .map(Action::OpenPr)
            .or(status.action.clone())
            .filter(|action| *action != Action::Reply);
        if let Some(action) = primary {
            let label = match &action {
                Action::OpenPr(_) => "Open PR",
                Action::SendCi => "Send CI to agent",
                Action::Reply => "Reply",
            };
            let id = id.clone();
            header = header.child(
                self.header_button("primary-action")
                    .bg(rgb(theme.accent))
                    .border_color(rgb(theme.accent))
                    .hover(|style| style.opacity(0.9))
                    .text_color(rgb(theme.surface))
                    .font_weight(FontWeight::MEDIUM)
                    .when(matches!(action, Action::OpenPr(_)), |button| {
                        button.child(icon(Icon::GitPullRequest, px(14.), rgb(theme.surface)))
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| match &action {
                        Action::OpenPr(url) => cx.open_url(url),
                        Action::SendCi => {
                            this.run_action("send_ci_feedback", json!({"worker_id": id}), cx)
                        }
                        Action::Reply => {}
                    })),
            );
        }
        let more_open = self.menu == Some(Menu::More);
        header
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        self.header_button("more-actions")
                            .px_2()
                            .child("⋯")
                            .tooltip(|_, cx| crate::keyboard_ui::tooltip("More actions".into(), cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.menu = (!more_open).then_some(Menu::More);
                                cx.notify();
                            })),
                    )
                    .when(more_open, |anchor| anchor.child(self.more_menu(worker, cx))),
            )
            .into_any_element()
    }

    pub(crate) fn agent_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let worker = self
            .workers
            .iter()
            .find(|worker| Some(&worker.id) == self.selected.as_ref())
            .cloned();
        let mut center = div()
            .id("agent-center")
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .flex_col()
            .child(self.agent_header(worker.as_ref(), cx));
        let mut notices = div().flex().flex_col().gap_2().px_4().pt_2();
        let mut any_notice = true;
        notices = notices.child(self.scripts_panel(cx));
        if let Some(error) = self.error_banner(cx) {
            notices = notices.child(error);
            any_notice = true;
        }
        if self.usage_open {
            notices = notices.child(self.usage_panel(cx));
            any_notice = true;
        }
        if self.ci_open {
            notices = notices.child(self.ci_panel(cx));
            any_notice = true;
        }
        if !self.details.is_empty() {
            any_notice = true;
            notices = notices.child(
                div()
                    .id("feedback-detail")
                    .max_h(px(160.))
                    .overflow_y_scroll()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(theme.panel))
                    .text_sm()
                    .text_color(rgb(theme.muted))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .mb_1()
                            .child(
                                div()
                                    .id("copy-feedback-detail")
                                    .cursor_pointer()
                                    .text_color(rgb(theme.link))
                                    .child("Copy")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            this.details.clone(),
                                        ))
                                    })),
                            )
                            .child(
                                div()
                                    .id("close-feedback-detail")
                                    .cursor_pointer()
                                    .child("×")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.details.clear();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(self.details.clone()),
            );
        }
        if any_notice {
            center = center.child(notices);
        }
        if let Some(terminal) = &self.terminal {
            center = center.child(div().flex_1().min_h(px(200.)).p_2().child(terminal.clone()));
        }
        div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .child(center)
            .child(
                // The window splits 1/6 sidebar, 3/6 session, 2/6 changes.
                div()
                    .w(relative(0.4))
                    .min_w(px(260.))
                    .max_w(px(560.))
                    .flex_none()
                    .h_full()
                    .child(match self.right_tab {
                        crate::checks_ui::RightTab::Changes => self.changes_pane(cx),
                        crate::checks_ui::RightTab::Readiness => self.readiness_pane(cx),
                    }),
            )
            .into_any_element()
    }
}
