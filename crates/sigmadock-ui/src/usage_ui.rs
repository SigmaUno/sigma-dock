use crate::Workspace;
use gpui::{Context, SharedString, div, prelude::*, px, relative, rgb};
use serde_json::json;
impl Workspace {
    pub(crate) fn load_usage(&mut self, cx: &mut Context<Self>) {
        if self.usage_loading {
            return;
        }
        let Some(worker) = self.selected.clone() else {
            return;
        };
        self.usage_open = true;
        self.usage_loading = true;
        self.usage_error = None;
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let id = worker.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    client
                        .call("agent_usage", json!({"worker_id":id}))
                        .and_then(|value| {
                            Ok(serde_json::from_value::<sigmadock_core::AgentUsage>(value)?)
                        })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.usage_loading = false;
                if this.selected.as_deref() == Some(&worker) {
                    match result {
                        Ok(report) => this.usage_report = Some(report),
                        Err(error) => this.usage_error = Some(error.to_string()),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(crate) fn usage_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut panel = div()
            .id("usage-pane")
            .max_h(px(280.))
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
                    .child(
                        div()
                            .flex_1()
                            .child("Subscription usage · shared account limits"),
                    )
                    .child(
                        div()
                            .id("refresh-usage")
                            .p_1()
                            .bg(rgb(self.theme.button))
                            .cursor_pointer()
                            .child(if self.usage_loading {
                                "Loading…"
                            } else {
                                "Refresh"
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.load_usage(cx))),
                    )
                    .child(
                        div()
                            .id("close-usage")
                            .cursor_pointer()
                            .child("Close ×")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.usage_open = false;
                                cx.notify();
                            })),
                    ),
            );
        if let Some(worker) = self
            .workers
            .iter()
            .find(|worker| Some(&worker.id) == self.selected.as_ref())
        {
            if worker.agent == "codex" {
                panel = panel.child("Refresh starts a read-only Codex account query using your existing CLI sign-in. It may contact the provider; it creates no agent turn.");
            }
            if worker.agent == "claude" {
                let id = worker.id.clone();
                let enabled = !worker.usage_reporting;
                panel = panel.child("Optional collection uses a session-local Claude status line on the next launch/resume. It temporarily replaces that session’s custom status line; global settings stay unchanged.")
                    .child(div().id("configure-usage").p_1().bg(rgb(self.theme.button)).cursor_pointer().child(if enabled { "Enable on next launch" } else { "Disable collection" }).on_click(cx.listener(move |this, _, _, cx| this.run_action("configure_usage",json!({"worker_id":id,"enabled":enabled}),cx))));
            }
        }
        if let Some(error) = &self.usage_error {
            panel = panel.child(format!(
                "Usage unavailable: {error}. Check the CLI version, sign-in and connection."
            ));
        }
        let Some(report) = &self.usage_report else {
            return panel
                .child(if self.usage_loading {
                    "Reading available provider data…"
                } else {
                    "No data loaded."
                })
                .into_any_element();
        };
        let age = sigmadock_core::unix_time().saturating_sub(report.recorded_at);
        panel = panel.child(format!(
            "Source: {} · Plan: {} · observed {}s ago (Unix {})",
            report.source,
            report.plan.as_deref().unwrap_or("not provided"),
            age,
            report.recorded_at
        ));
        if age > 300 {
            panel = panel.child("This cached report is over five minutes old. Refresh Codex, or wait for new Claude status-line output.");
        }
        if report.windows.is_empty() {
            panel = panel.child(
                "Usage allowance and remaining quota are unavailable; no estimate is shown.",
            );
        }
        for (index, window) in report.windows.iter().enumerate() {
            let reset = window
                .resets_at
                .map(|at| {
                    format!(
                        " · resets in {} min (Unix {at})",
                        at.saturating_sub(sigmadock_core::unix_time()).div_ceil(60)
                    )
                })
                .unwrap_or_else(|| " · reset time not provided".into());
            let duration = window
                .duration_minutes
                .map(|minutes| format!(" · {minutes}-minute window"))
                .unwrap_or_default();
            panel = panel.child(
                div()
                    .id(SharedString::from(format!("usage-window-{index}")))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(format!(
                        "{}{}: {:.1}% used · {:.1}% remaining{}",
                        window.name,
                        duration,
                        window.used_percent,
                        100. - window.used_percent,
                        reset
                    ))
                    .child(
                        div()
                            .w_full()
                            .h(px(6.))
                            .rounded_md()
                            .bg(rgb(self.theme.border))
                            .child(
                                div()
                                    .w(relative((window.used_percent / 100.) as f32))
                                    .h_full()
                                    .rounded_md()
                                    .bg(rgb(self.theme.accent)),
                            ),
                    ),
            );
        }
        if let Some(tokens) = report.lifetime_tokens {
            panel = panel.child(format!("Provider account lifetime activity: {tokens} tokens (not the current subscription window)."));
        }
        if report.context_input_tokens.is_some() || report.context_output_tokens.is_some() {
            panel = panel.child(format!("Current worker context: {} input / {} output tokens. Context counts are not session totals or subscription consumption.",report.context_input_tokens.map(|n|n.to_string()).unwrap_or_else(||"unknown".into()),report.context_output_tokens.map(|n|n.to_string()).unwrap_or_else(||"unknown".into())));
        }
        for warning in &report.warnings {
            panel = panel.child(
                div()
                    .text_sm()
                    .text_color(rgb(self.theme.muted))
                    .child(warning.clone()),
            );
        }
        panel.into_any_element()
    }
}
