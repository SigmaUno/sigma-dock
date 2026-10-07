use crate::{
    Workspace,
    updates::{self, Available, Channel, CheckError},
};
use gpui::{Context, div, prelude::*, px, rgb};
impl Workspace {
    fn save_update_preferences(&mut self) {
        self.settings_error = self
            .preferences
            .save(&self.preferences_path)
            .err()
            .map(|error| error.to_string());
    }
    pub(crate) fn check_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.checking_update {
            return;
        }
        self.checking_update = true;
        self.update_message = Some("Checking GitHub releases…".into());
        let checker = self.checker.clone();
        let channel = self.preferences.updates.channel;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match checker.lock() {
                        Ok(mut checker) => checker.check(channel),
                        Err(_) => Err(CheckError {
                            message: "Update checker unavailable; restart the app to retry.".into(),
                            retry_after: 3600,
                        }),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.checking_update = false;
                // Ignore an old channel's result if the preference changed in flight.
                if this.preferences.updates.channel == channel {
                    this.preferences.updates.next_check =
                        updates::next_check(&result, updates::now());
                    match result {
                        Ok(Some(update)) => {
                            this.update_message =
                                Some(format!("Version {} is available.", update.version));
                            if this
                                .preferences
                                .updates
                                .should_notify(&update.version.to_string(), manual)
                            {
                                this.available_update = Some(update);
                            }
                        }
                        Ok(None) => {
                            this.available_update = None;
                            this.update_message = Some(format!(
                                "Up to date — no newer compatible {:?} release.",
                                channel
                            ));
                        }
                        Err(error) => {
                            this.update_message = Some(error.message);
                        }
                    }
                }
                this.save_update_preferences();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(crate) fn update_controls(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut panel = div()
            .flex()
            .flex_col()
            .gap_2()
            .mt_3()
            .pt_3()
            .border_t_1()
            .border_color(rgb(0x3a4d65))
            .child(div().text_lg().child("Updates"))
            .child(div().text_sm().text_color(rgb(0xa6b4c8)).child(format!(
                "Installed {} · {} · {}\nSource {}",
                updates::VERSION,
                updates::BUILD_CHANNEL,
                std::env::consts::ARCH,
                updates::COMMIT
            )))
            .child(
                div()
                    .text_sm()
                    .child("Checks contact GitHub. Installation opens the release page."),
            )
            .child(
                div()
                    .id("check-updates")
                    .p_2()
                    .rounded_md()
                    .bg(rgb(0x275f54))
                    .cursor_pointer()
                    .child(if self.checking_update {
                        "Checking…"
                    } else {
                        "Check for updates"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.check_updates(true, cx))),
            )
            .child(
                div()
                    .id("automatic-updates")
                    .p_2()
                    .bg(rgb(0x243248))
                    .cursor_pointer()
                    .child(if self.preferences.updates.automatic {
                        "Automatic daily checks: on"
                    } else {
                        "Automatic daily checks: off"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.preferences.updates.automatic = !this.preferences.updates.automatic;
                        if this.preferences.updates.automatic {
                            this.preferences.updates.next_check = 0;
                        }
                        this.save_update_preferences();
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("update-channel")
                    .p_2()
                    .bg(rgb(0x243248))
                    .cursor_pointer()
                    .child(format!(
                        "Release channel: {:?} ↻",
                        self.preferences.updates.channel
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.preferences.updates.channel = match this.preferences.updates.channel {
                            Channel::Stable => Channel::Preview,
                            Channel::Preview => Channel::Stable,
                        };
                        this.available_update = None;
                        this.update_message = None;
                        this.preferences.updates.next_check = 0;
                        this.save_update_preferences();
                        cx.notify();
                    })),
            );
        if let Some(message) = &self.update_message {
            panel = panel.child(div().text_sm().child(message.clone()));
        }
        panel.into_any_element()
    }
    pub(crate) fn update_notice(
        &self,
        update: &Available,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let url = update.url.clone();
        let version = update.version.to_string();
        div()
            .p_3()
            .rounded_md()
            .bg(rgb(0x22473e))
            .flex()
            .flex_col()
            .gap_2()
            .child(format!(
                "SigmaDock {} is available · installed {} ({})",
                update.version,
                updates::VERSION,
                updates::BUILD_CHANNEL
            ))
            .child(
                div()
                    .id("update-release-notes")
                    .max_h(px(75.))
                    .overflow_y_scroll()
                    .text_sm()
                    .child(update.notes.clone()),
            )
            .child(
                div()
                    .text_sm()
                    .child(format!("Installer: {}", update.installer)),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        div()
                            .id("open-update-release")
                            .cursor_pointer()
                            .p_2()
                            .bg(rgb(0x275f54))
                            .child("Release notes & download")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .child(
                        div()
                            .id("dismiss-update")
                            .cursor_pointer()
                            .p_2()
                            .bg(rgb(0x243248))
                            .child("Dismiss")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.preferences.updates.dismissed = Some(version.clone());
                                this.available_update = None;
                                this.save_update_preferences();
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }
}
