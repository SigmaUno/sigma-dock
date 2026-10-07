//! Review exact repository config contents before approving; script terminals share the viewer.
use crate::Workspace;
use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div, prelude::*, px, rgb};
use serde_json::{Value, json};
use sigmadock_core::workspace_scripts::Phase;

#[derive(Default)]
pub(crate) struct ScriptsPane {
    pub value: Option<Value>,
    pub loading: bool,
    pub request: u64,
    pub phase: Option<Phase>,
}
impl Workspace {
    pub(crate) fn load_scripts(&mut self, cx: &mut Context<Self>) {
        let Some(worker) = self.selected.clone() else {
            return;
        };
        if self.scripts.loading {
            return;
        }
        self.scripts.loading = true;
        self.scripts.request += 1;
        let request = self.scripts.request;
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let id = worker.clone();
            let result = cx
                .background_executor()
                .spawn(async move { client.call("workspace_scripts", json!({"worker_id":id})) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.selected.as_ref() != Some(&worker) || this.scripts.request != request {
                    return;
                }
                this.scripts.loading = false;
                match result {
                    Ok(value) => this.scripts.value = Some(value),
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(crate) fn select_script_terminal(
        &mut self,
        script: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.terminal_script = script;
        self.reconnect_script_terminal(cx);
        if let Some(terminal) = &self.terminal {
            terminal.read(cx).focus_handle().focus(window);
        }
    }
    pub(crate) fn reconnect_script_terminal(&mut self, cx: &mut Context<Self>) {
        let Some(worker) = self.selected.clone() else {
            return;
        };
        self.connection
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.connection = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        self.terminal = Some(self.connect_terminal(&worker, self.connection.clone(), cx));
    }
    pub(crate) fn refresh_scripts_phase(&mut self, cx: &mut Context<Self>) {
        if self
            .departed
            .iter()
            .any(|worker| Some(&worker.id) == self.selected.as_ref())
        {
            return;
        }
        let phase = self
            .workers
            .iter()
            .find(|worker| Some(&worker.id) == self.selected.as_ref())
            .map(|worker| worker.workspace_scripts.phase);
        if phase != self.scripts.phase {
            self.scripts.phase = phase;
            self.terminal_script = match phase {
                Some(Phase::SettingUp | Phase::SetupFailed) => Some("setup".into()),
                Some(Phase::Archiving | Phase::ArchiveFailed) => Some("archive".into()),
                _ => None,
            };
            if self.terminal.is_some() {
                self.reconnect_script_terminal(cx);
            }
            self.load_scripts(cx);
        }
    }
    pub(crate) fn berth_script_actions(
        &self,
        worker: &sigmadock_core::Worker,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut actions = div().flex().flex_wrap().gap_1().text_xs();
        for name in &worker.workspace_scripts.available_runs {
            let running = worker
                .workspace_scripts
                .runs
                .get(name)
                .is_some_and(|status| status.running);
            let id = worker.id.clone();
            let name = name.clone();
            let label = format!("{} {name}", if running { "Stop" } else { "Run" });
            actions = actions.child(
                self.header_button(SharedString::from(format!("berth-run-{id}-{name}")))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.run_action(
                            "run_script",
                            json!({"worker_id":id,"name":name,"stop":running}),
                            cx,
                        );
                    })),
            );
        }
        actions.into_any_element()
    }
    pub(crate) fn scripts_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let Some(worker) = self
            .workers
            .iter()
            .find(|worker| Some(&worker.id) == self.selected.as_ref())
        else {
            return div().into_any_element();
        };
        let phase = worker.workspace_scripts.phase;
        if phase == Phase::Ready
            && worker.workspace_scripts.error.is_none()
            && self
                .scripts
                .value
                .as_ref()
                .is_some_and(|value| value["config"].is_null())
        {
            return div().into_any_element();
        }
        let mut panel = div()
            .flex()
            .flex_col()
            .gap_2()
            .text_sm()
            .p_3()
            .rounded_md()
            .bg(rgb(theme.panel));
        let mut actions = div().flex().flex_wrap().gap_2().items_center().child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(if phase == Phase::Ready {
                    "Workspace scripts"
                } else {
                    phase.label()
                }),
        );
        let id = worker.id.clone();
        actions = actions.child(
            self.header_button("refresh-scripts")
                .child("Review scripts")
                .on_click(cx.listener(|this, _, _, cx| this.load_scripts(cx))),
        );
        actions = actions.child(
            self.header_button("agent-terminal")
                .child("Agent")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_script_terminal(None, window, cx)
                })),
        );
        for script in ["setup", "archive"] {
            let script = script.to_owned();
            let label = format!("{script} output");
            actions = actions.child(
                self.header_button(SharedString::from(format!("script-output-{script}")))
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_script_terminal(Some(script.clone()), window, cx)
                    })),
            );
        }
        if matches!(phase, Phase::SetupFailed | Phase::AwaitingApproval)
            && !worker.workspace_scripts.archive_requested
        {
            for (label, skip) in [("Retry setup", false), ("Skip setup and start agent", true)] {
                let id = id.clone();
                actions = actions.child(
                    self.header_button(SharedString::from(format!("setup-{skip}")))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.run_action("setup_worker", json!({"worker_id":id,"skip":skip}), cx)
                        })),
                );
            }
        }
        if matches!(
            phase,
            Phase::SetupFailed | Phase::AwaitingApproval | Phase::ArchiveFailed
        ) {
            let archive_id = id.clone();
            actions = actions.child(
                self.header_button("scripts-archive")
                    .child("Archive")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_action("archive_worker", json!({"worker_id":archive_id}), cx)
                    })),
            );
        }
        if phase == Phase::ArchiveFailed {
            let id = id.clone();
            actions = actions.child(
                self.header_button("scripts-force-archive")
                    .child("Archive despite hook failure")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_action("archive_worker", json!({"worker_id":id,"force":true}), cx)
                    })),
            );
        }
        if let Some(error) = &worker.workspace_scripts.error {
            panel = panel.child(div().text_color(rgb(theme.error)).child(error.clone()));
        }
        if worker.facts.session == sigmadock_core::SessionState::Lost && phase != Phase::Ready {
            let id = id.clone();
            actions = actions.child(
                self.header_button("setup-acknowledge")
                    .child("I verified the old process stopped · retry")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_action(
                            "setup_worker",
                            json!({"worker_id":id,"acknowledge_unknown":true}),
                            cx,
                        )
                    })),
            );
        }
        if let Some(value) = &self.scripts.value {
            if value["approved"] == false {
                panel = panel.child(div().text_color(rgb(theme.warning)).child("Repository scripts execute code with your account. Review the complete config below before approving. Approval applies to this project's exact contents."));
                if let Some(text) = value["text"].as_str() {
                    panel = panel.child(
                        div()
                            .id("repository-script-config")
                            .max_h(px(180.))
                            .overflow_y_scroll()
                            .font_family("Menlo")
                            .text_xs()
                            .child(text.to_owned()),
                    );
                }
                if let Some(hash) = value["hash"].as_str() {
                    let hash = hash.to_owned();
                    let id = id.clone();
                    actions = actions.child(
                        self.header_button("approve-repository-scripts")
                            .child("Approve these scripts")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.run_action(
                                    "approve_scripts",
                                    json!({"worker_id":id,"hash":hash}),
                                    cx,
                                )
                            })),
                    );
                }
            }
            if let Some(runs) = value["config"]["scripts"]["run"].as_object() {
                for name in runs.keys() {
                    let name = name.clone();
                    let id = id.clone();
                    let run_name = name.clone();
                    let run_id = id.clone();
                    let running = worker
                        .workspace_scripts
                        .runs
                        .get(&name)
                        .is_some_and(|s| s.running);
                    actions = actions.child(
                        self.header_button(SharedString::from(format!("run-{name}")))
                            .child(format!("{} {name}", if running { "Stop" } else { "Run" }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.run_action(
                                    "run_script",
                                    json!({"worker_id":run_id,"name":run_name,"stop":running}),
                                    cx,
                                )
                            })),
                    );
                    let script = format!("run:{name}");
                    actions = actions.child(
                        self.header_button(SharedString::from(format!("attach-{name}")))
                            .child(format!("{name} output"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select_script_terminal(Some(script.clone()), window, cx)
                            })),
                    );
                }
            }
        }
        if let Some(name) = &worker.workspace_scripts.pending_run {
            panel = panel.child(format!("Starting {name} after other runs stop…"));
        }
        panel
            .child(format!(
                "Terminal: {}",
                self.terminal_script
                    .as_deref()
                    .unwrap_or("agent / current lifecycle")
            ))
            .child(actions)
            .into_any_element()
    }
}
