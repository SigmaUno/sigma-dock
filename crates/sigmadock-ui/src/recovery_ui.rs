use crate::Workspace;
use crate::ellipsis::Ellipsis;
use gpui::{Context, SharedString, div, prelude::*, px, rgb};
use serde_json::json;
fn timestamp(value: &serde_json::Value) -> String {
    value.as_u64().map_or_else(
        || "not recorded".into(),
        |seconds| crate::berths_ui::relative_time(seconds, sigmadock_core::unix_time()),
    )
}
impl Workspace {
    pub(crate) fn recovery_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let mut panel = div()
            .id("unfinished-sessions")
            // In the scrolling column a shrinkable panel is squeezed to a sliver.
            .flex_none()
            .max_h(px(360.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.panel))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Unfinished sessions"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_xs()
                            .text_color(rgb(theme.muted))
                            .ellipsis()
                            .child("Saved transcripts, not live terminals · kept 7 days"),
                    )
                    .child(
                        div()
                            .id("close-recovery")
                            .flex_none()
                            .px_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_sm()
                            .text_color(rgb(theme.muted))
                            .hover(|style| style.bg(rgb(theme.base)))
                            .child("×")
                            .tooltip(|_, cx| {
                                crate::keyboard_ui::tooltip("Hide unfinished sessions".into(), cx)
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.recovery_open = false;
                                cx.notify();
                            })),
                    ),
            );
        if let Some(error) = &self.recovery_error {
            panel = panel.child(div().child(format!("Recovery data unavailable: {error}. An older daemon may need a manual restart after its workers finish.")));
        }
        if self.recovery_entries.is_empty() && self.recovery_error.is_none() {
            panel = panel.child("No unfinished sessions.");
        }
        for entry in &self.recovery_entries {
            let worker = &entry["worker"];
            let id = worker["id"].as_str().unwrap_or_default().to_owned();
            let runtime = entry["runtime"].as_str().unwrap_or("unknown");
            let context = &entry["context"];
            let selected = self.recovery_selected.as_deref() == Some(&id);
            let project = self
                .project_name(worker["project_id"].as_str().unwrap_or_default())
                .unwrap_or("unknown project")
                .to_owned();
            let mut card = div()
                .id(SharedString::from(format!("recovery-{id}")))
                .flex()
                .flex_col()
                .gap_1()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(rgb(self.theme.card))
                .child(
                    div()
                        .id(SharedString::from(format!("inspect-recovery-{id}")))
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .ellipsis()
                                .child(worker["title"].as_str().unwrap_or("Session").to_owned()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(rgb(self.theme.muted))
                                .child(format!(
                                    "{runtime} · {}",
                                    timestamp(&context["last_activity"])
                                )),
                        )
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| {
                                // Clicking an open session collapses it again.
                                this.recovery_selected = (this.recovery_selected.as_deref()
                                    != Some(&id))
                                .then(|| id.clone());
                                cx.notify();
                            }
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(self.theme.muted))
                        .ellipsis()
                        .child(format!(
                            "{project} · {} · checkpoint {}",
                            worker["agent"].as_str().unwrap_or("unknown"),
                            timestamp(&context["recorded_at"])
                        )),
                );
            if selected {
                card = card
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(self.theme.muted))
                            .ellipsis()
                            .child(format!(
                                "Branch {} · recorded {}",
                                worker["branch"].as_str().unwrap_or("unknown"),
                                context["state"].as_str().unwrap_or("no checkpoint")
                            )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(self.theme.muted))
                            .ellipsis()
                            .child(format!(
                                "Worktree {}",
                                worker["worktree"].as_str().unwrap_or("unknown")
                            )),
                    );
                let text = context["text"]
                    .as_str()
                    .unwrap_or(
                        "No saved output. Start or interact with a session to record context.",
                    )
                    .to_owned();
                card = card
                    .child(
                        div()
                            .id(SharedString::from(format!("saved-output-{id}")))
                            .max_h(px(130.))
                            .overflow_y_scroll()
                            .p_2()
                            .rounded_md()
                            .bg(rgb(self.theme.base))
                            .font_family(sigmadock_terminal::DEFAULT_MONOSPACE_FONT)
                            .text_xs()
                            .child(
                                text.lines()
                                    .map(crate::berths_ui::chrome_text)
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            ),
                    )
                    .when(context["truncated"] == true, |card| {
                        card.child("Earlier transcript omitted (bounded tail).")
                    });
                let mut actions = div().flex().gap_2();
                actions = actions.child(
                    div()
                        .id(SharedString::from(format!("copy-context-{id}")))
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(self.theme.border))
                        .text_xs()
                        .cursor_pointer()
                        .bg(rgb(self.theme.surface))
                        .hover(|style| style.bg(rgb(self.theme.button)))
                        .child("Copy context")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()))
                        }),
                );
                if runtime == "running" {
                    actions = actions.child(
                        div()
                            .id(SharedString::from(format!("attach-recovery-{id}")))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(self.theme.border))
                            .text_xs()
                            .cursor_pointer()
                            .bg(rgb(self.theme.surface))
                            .hover(|style| style.bg(rgb(self.theme.button)))
                            .child("Attach live terminal")
                            .on_click(cx.listener({
                                let id = id.clone();
                                move |this, _, window, cx| {
                                    this.recovery_open = false;
                                    this.open_worker(id.clone(), window, cx);
                                }
                            })),
                    );
                } else {
                    if runtime == "unknown" {
                        card = card.child(div().text_xs().text_color(rgb(self.theme.warning)).child(format!("Old process state is unknown (recorded PID {}). Verify it stopped before starting a replacement.",context["pid"])));
                    }
                    if entry["conversation_supported"] != true {
                        card = card.child(div().text_xs().text_color(rgb(self.theme.muted)).child("This harness cannot continue a saved conversation. Resume starts a fresh process in the existing worktree."));
                    }
                    for (continue_session, label) in [
                        (
                            false,
                            if runtime == "unknown" {
                                "Verified stopped: start fresh"
                            } else {
                                "Start fresh"
                            },
                        ),
                        (true, "Verified stopped: continue latest conversation"),
                    ] {
                        if continue_session && entry["conversation_supported"] != true {
                            continue;
                        }
                        let id = id.clone();
                        actions = actions.child(div().id(SharedString::from(format!("recover-{id}-{continue_session}"))).px_2().py_0p5().rounded_md().border_1().border_color(rgb(self.theme.border)).text_xs().cursor_pointer().bg(rgb(self.theme.surface)).hover(|style| style.bg(rgb(self.theme.button))).child(label)
                            .on_click(cx.listener(move |this,_,_,cx| this.run_action("resume_worker",json!({"worker_id":id,"continue":continue_session,"acknowledge_unknown":true}),cx))));
                    }
                    let id = id.clone();
                    actions = actions.child(
                        div()
                            .id(SharedString::from(format!("archive-recovery-{id}")))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(self.theme.border))
                            .text_xs()
                            .cursor_pointer()
                            .bg(rgb(self.theme.surface))
                            .hover(|style| style.bg(rgb(self.theme.button)))
                            .child("Archive")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.run_action("archive_worker", json!({"worker_id":id}), cx)
                            })),
                    );
                }
                let id = id.clone();
                actions = actions.child(
                    div()
                        .id(SharedString::from(format!("clear-context-{id}")))
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(self.theme.border))
                        .text_xs()
                        .cursor_pointer()
                        .bg(rgb(self.theme.surface))
                        .hover(|style| style.bg(rgb(self.theme.button)))
                        .child("Clear saved context")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.run_action("clear_session_context", json!({"worker_id":id}), cx)
                        })),
                );
                card = card.child(actions);
            }
            panel = panel.child(card);
        }
        panel.into_any_element()
    }
}
