use crate::Workspace;
use gpui::{Context, SharedString, div, prelude::*, px, rgb};
use serde_json::json;
fn timestamp(value: &serde_json::Value) -> String {
    let Some(seconds) = value.as_u64() else {
        return "not recorded".into();
    };
    let elapsed = sigmadock_core::unix_time().saturating_sub(seconds);
    let age = if elapsed < 60 {
        format!("{elapsed}s ago")
    } else if elapsed < 3600 {
        format!("{}m ago", elapsed / 60)
    } else if elapsed < 86400 {
        format!("{}h ago", elapsed / 3600)
    } else {
        format!("{}d ago", elapsed / 86400)
    };
    format!("{age} (Unix {seconds})")
}
impl Workspace {
    pub(crate) fn recovery_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut panel = div().id("unfinished-sessions").max_h(px(320.)).overflow_y_scroll().flex().flex_col().gap_2().p_3().rounded_md().bg(rgb(self.theme.panel))
            .child(div().flex().justify_between().child("Unfinished sessions · local checkpoints")
                .child(div().id("close-recovery").cursor_pointer().child("Close ×").on_click(cx.listener(|this,_,_,cx| { this.recovery_open = false; cx.notify(); }))))
            .child(div().text_sm().text_color(rgb(self.theme.muted)).child("Saved output is a timestamped transcript, not a live terminal. Retained up to 7 days / 512 sessions / 16 KiB each."));
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
            let mut card = div()
                .id(SharedString::from(format!("recovery-{id}")))
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .bg(rgb(self.theme.card))
                .child(
                    div()
                        .id(SharedString::from(format!("inspect-recovery-{id}")))
                        .cursor_pointer()
                        .child(format!(
                            "{} · {} · {}",
                            worker["title"].as_str().unwrap_or("Session"),
                            worker["agent"].as_str().unwrap_or("unknown"),
                            runtime
                        ))
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| {
                                this.recovery_selected = Some(id.clone());
                                cx.notify();
                            }
                        })),
                )
                .child(div().text_sm().child(format!(
                    "Project: {} · branch: {}\nWorktree: {}",
                    entry["project"]["path"].as_str().unwrap_or("unknown"),
                    worker["branch"].as_str().unwrap_or("unknown"),
                    worker["worktree"].as_str().unwrap_or("unknown")
                )))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(self.theme.muted))
                        .child(format!(
                            "Recorded status: {} · last activity: {} · checkpoint: {}",
                            context["state"].as_str().unwrap_or("no checkpoint"),
                            timestamp(&context["last_activity"]),
                            timestamp(&context["recorded_at"])
                        )),
                );
            if selected {
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
                            .font_family("monospace")
                            .text_sm()
                            .child(text.clone()),
                    )
                    .when(context["truncated"] == true, |card| {
                        card.child("Earlier transcript omitted (bounded tail).")
                    });
                let mut actions = div().flex().gap_2();
                actions = actions.child(
                    div()
                        .id(SharedString::from(format!("copy-context-{id}")))
                        .p_1()
                        .cursor_pointer()
                        .bg(rgb(self.theme.button))
                        .child("Copy context")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()))
                        }),
                );
                if runtime == "running" {
                    actions = actions.child(
                        div()
                            .id(SharedString::from(format!("attach-recovery-{id}")))
                            .p_1()
                            .cursor_pointer()
                            .bg(rgb(self.theme.button))
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
                        card = card.child(div().text_sm().child(format!("Old process state is unknown (recorded PID {}). Verify it stopped before starting a replacement.",context["pid"])));
                    }
                    if entry["conversation_supported"] != true {
                        card = card.child(div().text_sm().child("This harness cannot continue a saved conversation. Resume starts a fresh process in the existing worktree."));
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
                        actions = actions.child(div().id(SharedString::from(format!("recover-{id}-{continue_session}"))).p_1().cursor_pointer().bg(rgb(self.theme.button)).child(label)
                            .on_click(cx.listener(move |this,_,_,cx| this.run_action("resume_worker",json!({"worker_id":id,"continue":continue_session,"acknowledge_unknown":true}),cx))));
                    }
                    let id = id.clone();
                    actions = actions.child(
                        div()
                            .id(SharedString::from(format!("archive-recovery-{id}")))
                            .p_1()
                            .cursor_pointer()
                            .bg(rgb(self.theme.button))
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
                        .p_1()
                        .cursor_pointer()
                        .bg(rgb(self.theme.button))
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
