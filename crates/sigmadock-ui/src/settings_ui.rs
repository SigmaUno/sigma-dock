//! Settings page: appearance, editor, forges and updates in one place.
use crate::{
    Workspace,
    icons::{Icon, icon},
};
use gpui::{
    AnyElement, Context, ElementId, FontWeight, SharedString, Window, div, prelude::*, px, rgb,
};
use serde_json::json;
use sigmadock_core::{ForgeConfig, Project, TokenSource};

/// `settings_editor` indices for forge form text fields start here.
pub(crate) const FORGE_FIELD: usize = 2000;
const FIELDS: [&str; 4] = ["API URL", "Owner", "Repository", "Token variable"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Section {
    #[default]
    Forges,
    Editor,
    Appearance,
    Updates,
}

impl Section {
    const ALL: [Self; 4] = [Self::Forges, Self::Editor, Self::Appearance, Self::Updates];
    fn label(self) -> &'static str {
        match self {
            Self::Forges => "Forges",
            Self::Editor => "Editor",
            Self::Appearance => "Terminal appearance",
            Self::Updates => "Updates",
        }
    }
    fn hint(self) -> &'static str {
        match self {
            Self::Forges => {
                "Connect each project to GitHub or Forgejo for pull request status, CI, review \
                 feedback and the Inbox. Every agent in the project uses it, including new ones."
            }
            Self::Editor => "Open-in-editor actions use this. SigmaDock has no built-in editor.",
            Self::Appearance => "Changes apply live. Click a value; ⌘A replaces it.",
            Self::Updates => "Checks contact GitHub. Installation opens the release page.",
        }
    }
}

/// A project's forge being edited, with the result of the last connection test.
pub(crate) struct ForgeForm {
    pub project: String,
    pub config: ForgeConfig,
    pub status: Option<Result<String, String>>,
    pub busy: bool,
}

fn blank_forge() -> ForgeConfig {
    ForgeConfig {
        kind: "github".into(),
        api_url: "https://api.github.com".into(),
        owner: String::new(),
        repo: String::new(),
        token_env: "GITHUB_TOKEN".into(),
        actions: false,
        token: TokenSource::GithubCli,
    }
}

fn describe(config: &ForgeConfig) -> String {
    let token = match config.token {
        TokenSource::GithubCli => "GitHub CLI login".to_owned(),
        TokenSource::Env => format!("${}", config.token_env),
    };
    let kind = if config.kind == "github" {
        "GitHub"
    } else {
        "Forgejo"
    };
    format!("{kind} · {}/{} · {token}", config.owner, config.repo)
}

impl Workspace {
    pub(crate) fn open_settings(
        &mut self,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_section = section;
        if !self.settings_open {
            self.toggle_settings(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn edit_forge_field(&mut self, field: usize, text: String) {
        let Some(form) = &mut self.forge_form else {
            return;
        };
        let config = &mut form.config;
        match field {
            0 => config.api_url = text,
            1 => config.owner = text,
            2 => config.repo = text,
            3 => config.token_env = text,
            _ => return,
        }
        form.status = None;
    }

    fn edit_forge(&mut self, project: &Project, cx: &mut Context<Self>) {
        self.settings_editor = None;
        let id = project.id.clone();
        if let Some(config) = &project.forge {
            self.forge_form = Some(ForgeForm {
                project: id,
                config: config.clone(),
                status: None,
                busy: false,
            });
            cx.notify();
            return;
        }
        self.forge_form = Some(ForgeForm {
            project: id.clone(),
            config: blank_forge(),
            status: None,
            busy: true,
        });
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let project = id.clone();
            let detected = cx
                .background_executor()
                .spawn(async move {
                    let value = client.call("detect_forge", json!({"project_id": project}))?;
                    Ok::<ForgeConfig, anyhow::Error>(serde_json::from_value(value)?)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(form) = this.forge_form.as_mut().filter(|form| form.project == id) {
                    form.busy = false;
                    match detected {
                        Ok(config) => {
                            form.config = config;
                            form.status = Some(Ok("Filled in from the origin remote".into()));
                        }
                        Err(error) => form.status = Some(Err(error.to_string())),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// `check_forge` verifies the token; `configure_project_forge` saves or, with `None`, removes.
    fn forge_request(&mut self, save: Option<Option<ForgeConfig>>, cx: &mut Context<Self>) {
        let Some(form) = &mut self.forge_form else {
            return;
        };
        if form.busy {
            return;
        }
        form.busy = true;
        form.status = None;
        let id = form.project.clone();
        let config = form.config.clone();
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let project = id.clone();
            let saving = save.is_some();
            let result = cx
                .background_executor()
                .spawn(async move {
                    match save {
                        Some(forge) => {
                            let value = client.call(
                                "configure_project_forge",
                                json!({"project_id": project, "forge": forge}),
                            )?;
                            let workers = value["workers"].as_u64().unwrap_or(0);
                            Ok(if forge.is_some() {
                                format!("Saved for this project and {workers} current agent(s)")
                            } else {
                                "Forge removed".to_owned()
                            })
                        }
                        None => {
                            let value = client.call("check_forge", json!({"forge": config}))?;
                            Ok::<_, anyhow::Error>(format!(
                                "Signed in as {}",
                                value["login"].as_str().unwrap_or("unknown")
                            ))
                        }
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(form) = this.forge_form.as_mut().filter(|form| form.project == id) {
                    form.busy = false;
                    form.status = Some(result.map_err(|error| {
                        if error.to_string().contains("unknown method") {
                            "Restart the SigmaDock daemon to configure forges here.".into()
                        } else {
                            error.to_string()
                        }
                    }));
                }
                if saving {
                    this.inbox.data = None;
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn choice(&self, id: SharedString, label: &str, on: bool) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        div()
            .id(id)
            .px_3()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .bg(rgb(if on { theme.accent } else { theme.button }))
            .when(on, |chip| chip.text_color(rgb(theme.base)))
            .when(!on, |chip| {
                chip.hover(|style| style.bg(rgb(theme.selection)))
            })
            .child(label.to_owned())
    }

    fn button(
        &self,
        id: impl Into<ElementId>,
        label: &str,
        primary: bool,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        div()
            .id(id)
            .px_3()
            .py_1p5()
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .when(primary, |button| {
                button.bg(rgb(theme.accent)).text_color(rgb(theme.base))
            })
            .when(!primary, |button| {
                button
                    .bg(rgb(theme.button))
                    .hover(|style| style.bg(rgb(theme.selection)))
            })
            .child(label.to_owned())
    }

    fn forge_form_view(
        &self,
        form: &ForgeForm,
        configured: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let config = &form.config;
        let github = config.kind == "github";
        let mut kinds = div().flex().gap_2();
        for (kind, label) in [("github", "GitHub"), ("forgejo", "Forgejo")] {
            kinds = kinds.child(
                self.choice(
                    SharedString::from(format!("forge-kind-{kind}")),
                    label,
                    config.kind == kind,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(form) = &mut this.forge_form {
                        let config = &mut form.config;
                        config.kind = kind.into();
                        if kind == "github" {
                            config.api_url = "https://api.github.com".into();
                            config.actions = false;
                        } else if config.token == TokenSource::GithubCli {
                            config.token = TokenSource::Env;
                            config.token_env = "FORGEJO_TOKEN".into();
                        }
                        form.status = None;
                    }
                    cx.notify();
                })),
            );
        }
        let mut tokens = div().flex().gap_2();
        for (source, label) in [
            (TokenSource::GithubCli, "GitHub CLI login"),
            (TokenSource::Env, "Environment variable"),
        ] {
            if source == TokenSource::GithubCli && !github {
                continue;
            }
            tokens = tokens.child(
                self.choice(
                    SharedString::from(format!("forge-token-{source:?}")),
                    label,
                    config.token == source,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(form) = &mut this.forge_form {
                        form.config.token = source;
                        form.status = None;
                    }
                    cx.notify();
                })),
            );
        }
        let row = |label: &'static str, content: AnyElement| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().w(px(130.)).flex_none().text_sm().child(label))
                .child(div().flex_1().child(content))
        };
        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .pt_3()
            .mt_2()
            .border_t_1()
            .border_color(rgb(theme.border))
            .child(row("Forge", kinds.into_any_element()));
        let values = [
            &config.api_url,
            &config.owner,
            &config.repo,
            &config.token_env,
        ];
        for (field, label) in FIELDS.iter().enumerate().take(3) {
            if field == 0 && github {
                continue;
            }
            body = body.child(self.appearance_field(
                FORGE_FIELD + field,
                (*label).into(),
                values[field].clone(),
                cx,
            ));
        }
        body = body.child(row("Token", tokens.into_any_element()));
        if config.token == TokenSource::Env {
            body = body.child(self.appearance_field(
                FORGE_FIELD + 3,
                FIELDS[3].into(),
                config.token_env.clone(),
                cx,
            ));
        }
        body = body.child(
            div()
                .text_xs()
                .text_color(rgb(theme.muted))
                .child(match config.token {
                    TokenSource::GithubCli => {
                        "Uses `gh auth token`. Run `gh auth login` once if it is not signed in. \
                         SigmaDock never stores the token."
                    }
                    TokenSource::Env => {
                        "Read from the daemon's environment. Apps opened from Finder have no \
                         shell variables, so start the daemon from a shell that sets it."
                    }
                }),
        );
        if !github {
            body = body.child(row(
                "CI",
                self.choice(
                    "forge-actions".into(),
                    "Include Forgejo Actions runs",
                    config.actions,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(form) = &mut this.forge_form {
                        form.config.actions = !form.config.actions;
                    }
                    cx.notify();
                }))
                .into_any_element(),
            ));
        }
        if let Some(status) = &form.status {
            let (color, text) = match status {
                Ok(text) => (theme.success, text.clone()),
                Err(text) => (theme.error, text.clone()),
            };
            body = body.child(div().text_sm().text_color(rgb(color)).child(text));
        }
        let ready = !config.owner.trim().is_empty() && !config.repo.trim().is_empty();
        let mut actions = div().flex().gap_2().pt_1();
        if form.busy {
            actions = actions.child(
                div()
                    .text_sm()
                    .text_color(rgb(theme.muted))
                    .child("Working…"),
            );
        } else {
            actions = actions
                .child(
                    self.button("forge-save", "Save", true)
                        .when(!ready, |button| button.opacity(0.5))
                        .when(ready, |button| {
                            button.on_click(cx.listener(|this, _, _, cx| {
                                let config =
                                    this.forge_form.as_ref().map(|form| form.config.clone());
                                this.forge_request(Some(config), cx)
                            }))
                        }),
                )
                .child(
                    self.button("forge-test", "Test connection", false)
                        .on_click(cx.listener(|this, _, _, cx| this.forge_request(None, cx))),
                )
                .when(configured, |row| {
                    row.child(
                        self.button("forge-remove", "Remove", false)
                            .text_color(rgb(theme.error))
                            .on_click(
                                cx.listener(|this, _, _, cx| this.forge_request(Some(None), cx)),
                            ),
                    )
                })
                .child(div().flex_1())
                .child(
                    self.button("forge-close", "Close", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.forge_form = None;
                            this.settings_editor = None;
                            cx.notify();
                        })),
                );
        }
        body.child(actions).into_any_element()
    }

    fn forges_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let mut list = div().flex().flex_col().gap_3();
        if self.projects.is_empty() {
            list = list.child(
                div()
                    .text_sm()
                    .text_color(rgb(theme.muted))
                    .child("Add a repository from the sidebar first."),
            );
        }
        for project in &self.projects {
            let editing = self
                .forge_form
                .as_ref()
                .filter(|form| form.project == project.id);
            let configured = project.forge.is_some();
            let edit_project = project.clone();
            let mut card =
                div()
                    .flex()
                    .flex_col()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(if editing.is_some() {
                        theme.accent
                    } else {
                        theme.border
                    }))
                    .bg(rgb(theme.surface))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(icon(
                                if configured {
                                    Icon::GitPullRequest
                                } else {
                                    Icon::GitBranch
                                },
                                px(16.),
                                rgb(if configured {
                                    theme.success
                                } else {
                                    theme.muted
                                }),
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(project.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(theme.muted))
                                            .truncate()
                                            .child(project.forge.as_ref().map_or_else(
                                                || "Not connected".to_owned(),
                                                describe,
                                            )),
                                    ),
                            )
                            .when(editing.is_none(), |row| {
                                row.child(
                                    self.button(
                                        SharedString::from(format!("forge-edit-{}", project.id)),
                                        if configured { "Edit" } else { "Connect" },
                                        !configured,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| this.edit_forge(&edit_project, cx),
                                    )),
                                )
                            }),
                    );
            if let Some(form) = editing {
                card = card.child(self.forge_form_view(form, configured, cx));
            }
            list = list.child(card);
        }
        list.into_any_element()
    }

    pub(crate) fn settings_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let section = self.settings_section;
        let mut nav = div()
            .w(px(200.))
            .flex_none()
            .flex()
            .flex_col()
            .gap_0p5()
            .p_3()
            .border_r_1()
            .border_color(rgb(theme.border))
            .child(
                div()
                    .px_2()
                    .pt_2()
                    .pb_3()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Settings"),
            );
        for item in Section::ALL {
            let active = item == section;
            nav = nav.child(
                div()
                    .id(SharedString::from(format!("settings-{}", item.label())))
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .border_l_2()
                    .border_color(if active {
                        rgb(theme.accent).into()
                    } else {
                        gpui::transparent_black()
                    })
                    .when(active, |row| {
                        row.bg(rgb(theme.base)).font_weight(FontWeight::SEMIBOLD)
                    })
                    .when(!active, |row| row.hover(|style| style.bg(rgb(theme.panel))))
                    .child(item.label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_section = item;
                        this.settings_editor = None;
                        cx.notify();
                    })),
            );
        }
        nav = nav.child(div().flex_1()).child(
            div()
                .id("close-settings")
                .px_2()
                .py_1p5()
                .rounded_md()
                .cursor_pointer()
                .text_sm()
                .text_color(rgb(theme.muted))
                .hover(|style| style.bg(rgb(theme.panel)))
                .child("Done · Esc")
                .on_click(cx.listener(|this, _, window, cx| this.toggle_settings(window, cx))),
        );
        let body = match section {
            Section::Forges => self.forges_section(cx),
            Section::Editor => self.editor_settings(cx),
            Section::Appearance => self.appearance_section(cx),
            Section::Updates => self.update_controls(cx),
        };
        div()
            .id("settings-page")
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .track_focus(&self.settings_focus)
            .on_key_down(cx.listener(Self::edit_appearance))
            .child(nav)
            .child(
                div()
                    .id("settings-content")
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .overflow_y_scroll()
                    .px_8()
                    .py_6()
                    .child(
                        div()
                            .max_w(px(720.))
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(section.label()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(theme.muted))
                                    .child(section.hint()),
                            )
                            .children(self.settings_error.as_ref().map(|error| {
                                div()
                                    .text_sm()
                                    .text_color(rgb(theme.error))
                                    .child(error.clone())
                            }))
                            .child(body),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_summaries_name_kind_repository_and_token_source() {
        let mut config = blank_forge();
        config.owner = "SigmaUno".into();
        config.repo = "sigma-dock".into();
        assert_eq!(
            describe(&config),
            "GitHub · SigmaUno/sigma-dock · GitHub CLI login"
        );
        config.kind = "forgejo".into();
        config.token = TokenSource::Env;
        config.token_env = "FORGEJO_TOKEN".into();
        assert_eq!(
            describe(&config),
            "Forgejo · SigmaUno/sigma-dock · $FORGEJO_TOKEN"
        );
    }
}
