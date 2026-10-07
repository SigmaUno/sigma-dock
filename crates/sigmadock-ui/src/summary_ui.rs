use crate::Workspace;
use gpui::Context;
use serde_json::json;
use std::time::Duration;
/// How long a summary button reads "Copied" after a successful copy.
const CONFIRMATION: Duration = Duration::from_secs(2);
impl Workspace {
    /// Copy a worker's Markdown session summary to the clipboard, and show it in the
    /// detail area when that worker is open.
    pub(crate) fn copy_summary(&mut self, worker_id: String, cx: &mut Context<Self>) {
        if self.summary_loading.is_some() {
            return;
        }
        self.summary_loading = Some(worker_id.clone());
        self.error = None;
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let id = worker_id.clone();
            let result = cx
                .background_executor()
                .spawn(async move { client.call("session_summary", json!({"worker_id":id})) })
                .await;
            let copied = this
                .update(cx, |this, cx| {
                    this.summary_loading = None;
                    let copied = match result.and_then(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| anyhow::anyhow!("invalid summary response"))
                    }) {
                        Ok(text) => {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
                            if this.selected.as_deref() == Some(&worker_id) {
                                this.details = text;
                            }
                            this.summary_copied = Some(worker_id.clone());
                            true
                        }
                        Err(error) => {
                            this.error = Some(format!("Summary failed: {error}"));
                            false
                        }
                    };
                    cx.notify();
                    copied
                })
                .unwrap_or(false);
            if copied {
                cx.background_executor().timer(CONFIRMATION).await;
                let _ = this.update(cx, |this, cx| {
                    if this.summary_copied.as_deref() == Some(&worker_id) {
                        this.summary_copied = None;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        cx.notify();
    }
    pub(crate) fn summary_label(&self, worker_id: &str, idle: &'static str) -> &'static str {
        if self.summary_loading.as_deref() == Some(worker_id) {
            "Summarizing…"
        } else if self.summary_copied.as_deref() == Some(worker_id) {
            "Copied"
        } else {
            idle
        }
    }
}
