use crate::Workspace;
use gpui::{Context, SharedString, div, prelude::*, px, rgb};
use serde_json::json;
impl Workspace {
    pub(crate) fn load_ci(&mut self, cx: &mut Context<Self>) {
        if self.ci_loading {
            return;
        }
        let Some(worker) = self.selected.clone() else {
            return;
        };
        self.ci_open = true;
        self.ci_loading = true;
        self.ci_error = None;
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let request_worker = worker.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    client
                        .call("ci_preview", json!({"worker_id":request_worker}))
                        .and_then(|value| {
                            Ok(serde_json::from_value::<sigmadock_core::CiPreview>(value)?)
                        })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.ci_loading = false;
                if this.selected.as_deref() == Some(&worker) {
                    match result {
                        Ok(report) => {
                            this.ci_report = Some(report);
                            this.ci_expanded = None;
                        }
                        Err(error) => this.ci_error = Some(error.to_string()),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(crate) fn ci_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut panel = div()
            .id("ci-details-pane")
            .max_h(px(300.))
            .overflow_y_scroll()
            .p_3()
            .rounded_md()
            .bg(rgb(self.theme.panel))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .gap_3()
                    .items_center()
                    .child(div().flex_1().child("CI results"))
                    .child(
                        div()
                            .id("refresh-ci")
                            .p_1()
                            .bg(rgb(self.theme.button))
                            .cursor_pointer()
                            .child(if self.ci_loading {
                                "Loading…"
                            } else {
                                "Refresh"
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.load_ci(cx))),
                    )
                    .child(
                        div()
                            .id("close-ci")
                            .cursor_pointer()
                            .child("Close ×")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.ci_open = false;
                                cx.notify();
                            })),
                    ),
            );
        if let Some(error) = &self.ci_error {
            panel=panel.child(format!("Could not load CI: {error}. Check forge configuration, credentials and endpoint support."));
        }
        let Some(report) = &self.ci_report else {
            return panel
                .child(if self.ci_loading {
                    "Fetching results for the selected worker…"
                } else {
                    "No result loaded."
                })
                .into_any_element();
        };
        let observed = self
            .workers
            .iter()
            .find(|worker| Some(&worker.id) == self.selected.as_ref())
            .and_then(|worker| worker.facts.head_sha.as_deref());
        let stale = report.head_sha != report.current_head
            || observed.is_some_and(|head| head != report.head_sha);
        panel = panel.child(format!(
            "Inspected commit: {} · {}",
            report.head_sha,
            crate::berths_ui::relative_time(report.refreshed_at, sigmadock_core::unix_time())
        ));
        if stale {
            panel = panel
                .child("These results belong to an older commit. Refresh before using feedback.");
        }
        for warning in &report.warnings {
            panel = panel.child(
                div()
                    .text_sm()
                    .text_color(rgb(self.theme.muted))
                    .child(warning.clone()),
            );
        }
        if report.entries.is_empty() {
            panel = panel.child(
                "No checks were returned by the available provider endpoints for this commit.",
            );
        }
        if report.truncated {
            panel = panel.child(
                "Result list truncated at 200 entries; open provider links for the complete view.",
            );
        }
        for item in &report.entries {
            let id = item.id.clone();
            let color = match item.state.as_str() {
                "failed" | "cancelled" => self.theme.error,
                "passed" => self.theme.success,
                "running" | "pending" => self.theme.warning,
                _ => self.theme.border,
            };
            let mut card = div()
                .p_2()
                .rounded_md()
                .bg(rgb(self.theme.card))
                .border_l_2()
                .border_color(rgb(color))
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .id(SharedString::from(format!("ci-entry-{id}")))
                        .cursor_pointer()
                        .child(format!("{} · {} · {} ▸", item.kind, item.name, item.state))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ci_expanded = if this.ci_expanded.as_deref() == Some(&id) {
                                None
                            } else {
                                Some(id.clone())
                            };
                            cx.notify();
                        })),
                );
            if self.ci_expanded.as_deref() == Some(&item.id) {
                card = card.child(
                    div()
                        .id(SharedString::from(format!("ci-text-{}", item.id)))
                        .max_h(px(160.))
                        .overflow_y_scroll()
                        .font_family("monospace")
                        .text_sm()
                        .child(if item.details.trim().is_empty() {
                            "No detail supplied by the provider.".into()
                        } else {
                            item.details.clone()
                        }),
                );
                if item.truncated {
                    card = card.child("Detail truncated to 8 KiB.");
                }
                let details = item.details.clone();
                let mut actions = div().flex().gap_2().child(
                    div()
                        .id(SharedString::from(format!("copy-ci-{}", item.id)))
                        .p_1()
                        .bg(rgb(self.theme.button))
                        .cursor_pointer()
                        .child("Copy details")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(details.clone()))
                        }),
                );
                if let Some(url) = &item.url {
                    let url = url.clone();
                    actions = actions.child(
                        div()
                            .id(SharedString::from(format!("open-ci-{}", item.id)))
                            .p_1()
                            .bg(rgb(self.theme.button))
                            .cursor_pointer()
                            .child("Open source")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    );
                }
                card = card.child(actions);
            }
            panel = panel.child(card);
        }
        if !stale && let Some(worker) = &self.selected {
            let id = worker.clone();
            panel = panel.child(
                div()
                    .id("preview-ci-feedback")
                    .p_2()
                    .bg(rgb(self.theme.button))
                    .cursor_pointer()
                    .child("Preview feedback before sending")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_action("ci_feedback", json!({"worker_id":id}), cx)
                    })),
            );
        }
        panel.into_any_element()
    }
}
