use crate::ellipsis::Ellipsis;
use crate::{
    Workspace,
    preferences::{Appearance, Cursor, parse_color},
};
use gpui::{Context, SharedString, Window, div, prelude::*, px, rgb};

/// `settings_editor` index of the custom editor command field.
const EDITOR_COMMAND: usize = 1000;

pub struct SettingsTooltip;
impl gpui::Render for SettingsTooltip {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::theme::Theme::for_appearance(window.appearance());
        div()
            .p_2()
            .rounded_md()
            .bg(rgb(theme.button))
            .text_color(rgb(theme.text))
            .child("Settings · Cmd/Ctrl+,")
    }
}
impl Workspace {
    pub(crate) fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = !self.settings_open;
        self.settings_editor = None;
        if self.settings_open {
            self.settings_focus.focus(window);
        } else if let Some(terminal) = &self.terminal {
            terminal.read(cx).focus_handle().focus(window);
        }
        cx.notify();
    }
    pub(crate) fn refresh_terminal_appearance(&mut self, cx: &mut Context<Self>) {
        for terminal in self
            .terminal
            .iter()
            .chain(self.editor_pane.iter().map(|pane| &pane.terminal))
        {
            terminal.update(cx, |terminal, cx| {
                terminal.update_config(
                    self.theme
                        .terminal(&self.preferences.appearance)
                        .apply(terminal.config().clone()),
                    cx,
                );
            });
        }
    }
    fn apply_appearance(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.preferences.appearance.validate() {
            self.settings_error = Some(error.to_string());
            return;
        }
        self.refresh_terminal_appearance(cx);
        self.settings_error = self
            .preferences
            .save(&self.preferences_path)
            .err()
            .map(|error| error.to_string());
        cx.notify();
    }
    pub(crate) fn edit_appearance(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" {
            self.toggle_settings(window, cx);
            cx.stop_propagation();
            return;
        }
        let Some((index, text)) = &mut self.settings_editor else {
            return;
        };
        let modifiers = event.keystroke.modifiers;
        if event.keystroke.key == "backspace" {
            text.pop();
        } else if event.keystroke.key == "a" && (modifiers.platform || modifiers.control) {
            text.clear();
        } else if event.keystroke.key == "v" && (modifiers.platform || modifiers.control) {
            if let Some(value) = cx.read_from_clipboard().and_then(|item| item.text()) {
                text.push_str(&value.replace(['\r', '\n'], ""));
            }
        } else if let Some(value) = &event.keystroke.key_char
            && !modifiers.control
            && !modifiers.platform
        {
            text.push_str(value);
        }
        let index = *index;
        let text = text.clone();
        if let Some(field) = index.checked_sub(crate::settings_ui::FORGE_FIELD) {
            self.edit_forge_field(field, text);
        } else if index == EDITOR_COMMAND {
            self.preferences.editor.custom_command = text;
            self.settings_error = self
                .preferences
                .save(&self.preferences_path)
                .err()
                .map(|error| error.to_string());
        } else if index == 0 && !text.trim().is_empty() && text.len() <= 128 {
            self.preferences.appearance.font = text;
            self.apply_appearance(cx);
        } else if index != 0 {
            if let Some(color) = parse_color(&text) {
                self.preferences.appearance.follow_system = false;
                self.preferences.appearance.set_color(index - 1, color);
                self.apply_appearance(cx);
            } else {
                self.settings_error = Some(
                    "Use six hex digits, for example #1e1e1e. The last valid color remains active."
                        .into(),
                );
            }
        }
        cx.stop_propagation();
        cx.notify();
    }
    pub(crate) fn appearance_field(
        &self,
        index: usize,
        label: String,
        value: String,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let editing = self.settings_editor.as_ref().filter(|(i, _)| *i == index);
        let displayed = editing
            .map(|(_, text)| text.clone())
            .unwrap_or(value.clone());
        div()
            .flex()
            .gap_2()
            .items_center()
            .child(div().w(px(130.)).text_sm().child(label))
            .child(
                div()
                    .id(SharedString::from(format!("appearance-{index}")))
                    .flex_1()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(if editing.is_some() {
                        self.theme.selection
                    } else {
                        self.theme.card
                    }))
                    .cursor_text()
                    .child(displayed)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.settings_editor = Some((index, value.clone()));
                        this.settings_focus.focus(window);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
    /// A small bordered button; `active` marks the selected option of a group.
    fn option_button(
        &self,
        id: SharedString,
        label: String,
        active: bool,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        div()
            .id(id)
            .flex_none()
            .px_2p5()
            .py_1()
            .rounded_md()
            .border_1()
            .cursor_pointer()
            .text_sm()
            .map(|button| {
                if active {
                    button
                        .border_color(rgb(theme.accent))
                        .bg(rgb(theme.selection))
                        .text_color(rgb(theme.text))
                } else {
                    button
                        .border_color(rgb(theme.border))
                        .bg(rgb(theme.surface))
                        .hover(|style| style.bg(rgb(theme.panel)))
                }
            })
            .child(label)
    }
    fn setting_row(&self, label: &'static str, control: impl IntoElement) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap_3()
            .min_h(px(36.))
            .child(
                div()
                    .w(px(130.))
                    .flex_none()
                    .text_sm()
                    .text_color(rgb(self.theme.muted))
                    .child(label),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(control),
            )
    }
    /// Terminal appearance controls for the Settings page.
    pub(crate) fn appearance_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let effective = theme.terminal(&self.preferences.appearance);
        let appearance = &effective;
        let editing_font = self.settings_editor.as_ref().filter(|(i, _)| *i == 0);
        let font_value = appearance.font.clone();
        let mut font = div().flex().items_center().gap_2().child(
            div()
                .id("appearance-0")
                .w(px(200.))
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(if editing_font.is_some() {
                    theme.accent
                } else {
                    theme.border
                }))
                .bg(rgb(theme.surface))
                .cursor_text()
                .text_sm()
                .ellipsis()
                .child(editing_font.map_or(font_value.clone(), |(_, text)| text.clone()))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.settings_editor = Some((0, font_value.clone()));
                    this.settings_focus.focus(window);
                    cx.notify();
                })),
        );
        for name in ["Menlo", "JetBrains Mono", "monospace"] {
            font = font.child(
                self.option_button(
                    SharedString::from(format!("font-{name}")),
                    name.into(),
                    appearance.font == name,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.settings_editor = None;
                    this.preferences.appearance.font = name.into();
                    this.apply_appearance(cx);
                })),
            );
        }
        let mut panel = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(self.setting_row("Font", font));
        for (index, label, value) in [
            (0, "Font size", format!("{} px", appearance.size)),
            (1, "Line spacing", format!("{:.1}×", appearance.line_height)),
            (2, "Padding", format!("{} px", appearance.padding)),
        ] {
            let mut stepper = div().flex().items_center().gap_1();
            for (direction, symbol) in [(-1., "−"), (1., "+")] {
                let button = self
                    .option_button(
                        SharedString::from(format!("step-{index}-{direction}")),
                        symbol.into(),
                        false,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let a = &mut this.preferences.appearance;
                        match index {
                            0 => a.size = (a.size + direction).clamp(8., 40.),
                            1 => {
                                a.line_height = ((a.line_height * 10.).round() + direction)
                                    .clamp(10., 20.)
                                    / 10.
                            }
                            _ => a.padding = (a.padding + direction * 2.).clamp(0., 32.),
                        }
                        this.apply_appearance(cx);
                    }));
                if direction < 0. {
                    stepper = stepper.child(button).child(
                        div()
                            .w(px(56.))
                            .text_center()
                            .text_sm()
                            .child(value.clone()),
                    );
                } else {
                    stepper = stepper.child(button);
                }
            }
            panel = panel.child(self.setting_row(label, stepper));
        }
        // System follows the app theme; Dark and Light pin a preset palette.
        let mut colors = div().flex().gap_1();
        for (choice, label) in [
            (None, "System"),
            (Some(false), "Dark"),
            (Some(true), "Light"),
        ] {
            let active = match choice {
                None => appearance.follow_system,
                Some(light) => {
                    let preset = if light {
                        Appearance::light()
                    } else {
                        Appearance::default()
                    };
                    !appearance.follow_system && appearance.background == preset.background
                }
            };
            colors = colors.child(
                self.option_button(
                    SharedString::from(format!("theme-{label}")),
                    label.into(),
                    active,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let a = &mut this.preferences.appearance;
                    match choice {
                        None => a.follow_system = true,
                        Some(light) => {
                            let preset = if light {
                                Appearance::light()
                            } else {
                                Appearance::default()
                            };
                            a.follow_system = false;
                            a.background = preset.background;
                            a.foreground = preset.foreground;
                            a.ansi = preset.ansi;
                        }
                    }
                    this.settings_editor = None;
                    this.apply_appearance(cx);
                })),
            );
        }
        panel = panel.child(self.setting_row("Colors", colors));
        let mut cursor = div().flex().gap_1();
        for (shape, label) in [
            (Cursor::Block, "Block"),
            (Cursor::Underline, "Underline"),
            (Cursor::Beam, "Beam"),
        ] {
            cursor = cursor.child(
                self.option_button(
                    SharedString::from(format!("cursor-{label}")),
                    label.into(),
                    appearance.cursor == shape,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.preferences.appearance.cursor = shape;
                    this.apply_appearance(cx);
                })),
            );
        }
        cursor = cursor.child(div().w(px(8.))).child(
            self.option_button("cursor-blink".into(), "Blink".into(), appearance.blink)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.preferences.appearance.blink = !this.preferences.appearance.blink;
                    this.apply_appearance(cx);
                })),
        );
        panel = panel.child(self.setting_row("Cursor", cursor));
        // Background, foreground, then the 16 ANSI colors in two rows of eight.
        let swatch = |index: usize, label: String, cx: &mut Context<Self>| {
            let color = appearance.color(index);
            let value = format!("#{color:06x}");
            let editing = self
                .settings_editor
                .as_ref()
                .is_some_and(|(i, _)| *i == index + 1);
            div()
                .id(SharedString::from(format!("appearance-{}", index + 1)))
                .flex()
                .flex_col()
                .items_center()
                .gap_0p5()
                .w(px(52.))
                .cursor_pointer()
                .child(
                    div()
                        .size(px(28.))
                        .rounded_md()
                        .border_2()
                        .border_color(rgb(if editing { theme.accent } else { theme.border }))
                        .bg(rgb(color)),
                )
                .child(div().text_xs().text_color(rgb(theme.muted)).child(label))
                .tooltip({
                    let value = value.clone();
                    move |_, cx| crate::keyboard_ui::tooltip(value.clone(), cx)
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.settings_editor = Some((index + 1, value.clone()));
                    this.settings_focus.focus(window);
                    cx.notify();
                }))
        };
        let base = div()
            .flex()
            .gap_1()
            .child(swatch(0, "Bg".into(), cx))
            .child(swatch(1, "Fg".into(), cx));
        let mut normal = div().flex().gap_1();
        let mut bright = div().flex().gap_1();
        for ansi in 0..8 {
            normal = normal.child(swatch(ansi + 2, ansi.to_string(), cx));
            bright = bright.child(swatch(ansi + 10, (ansi + 8).to_string(), cx));
        }
        panel = panel.child(
            div()
                .flex()
                .items_start()
                .gap_3()
                .pt_2()
                .child(
                    div()
                        .w(px(130.))
                        .flex_none()
                        .pt_1()
                        .text_sm()
                        .text_color(rgb(theme.muted))
                        .child("Palette"),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(base)
                        .child(normal)
                        .child(bright),
                ),
        );
        if let Some((index, text)) = self
            .settings_editor
            .as_ref()
            .filter(|(i, _)| (1..=18).contains(i))
        {
            let name = match index - 1 {
                0 => "Background".to_owned(),
                1 => "Foreground".to_owned(),
                n => format!("ANSI {}", n - 2),
            };
            panel = panel.child(
                self.setting_row(
                    "Editing",
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_sm().child(name))
                        .child(
                            div()
                                .w(px(120.))
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(theme.accent))
                                .bg(rgb(theme.surface))
                                .font_family(sigmadock_terminal::DEFAULT_MONOSPACE_FONT)
                                .text_sm()
                                .child(text.clone()),
                        ),
                ),
            );
        }
        panel
            .child(
                div().flex().pt_3().child(
                    self.option_button(
                        "restore-appearance".into(),
                        "Restore defaults".into(),
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_editor = None;
                        this.preferences.appearance = Appearance::default();
                        this.apply_appearance(cx);
                    })),
                ),
            )
            .into_any_element()
    }
}
impl Workspace {
    pub(crate) fn editor_settings(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use crate::editor::{Editor, detected};
        let theme = self.theme;
        let current = self.preferences.editor.editor;
        let mut choices = div().flex().flex_wrap().gap_2();
        let mut options: Vec<(Option<Editor>, String)> = vec![(
            None,
            format!(
                "Automatic ({})",
                crate::editor::EditorPreferences::default()
                    .resolved()
                    .label()
            ),
        )];
        options.extend(
            detected()
                .into_iter()
                .map(|e| (Some(e), e.label().to_owned())),
        );
        if !options
            .iter()
            .any(|(choice, _)| *choice == Some(Editor::Environment))
        {
            options.push((
                Some(Editor::Environment),
                Editor::Environment.label().into(),
            ));
        }
        options.push((Some(Editor::System), Editor::System.label().into()));
        options.push((Some(Editor::Custom), Editor::Custom.label().into()));
        for (choice, label) in options {
            choices = choices.child(
                div()
                    .id(SharedString::from(format!("editor-choice-{choice:?}")))
                    .p_2()
                    .rounded_md()
                    .cursor_pointer()
                    .bg(rgb(if current == choice {
                        theme.accent
                    } else {
                        theme.button
                    }))
                    .when(current == choice, |button| {
                        button.text_color(rgb(theme.base))
                    })
                    .text_sm()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when_some(choice, |button, editor| {
                        button.child(crate::editor_icons::editor_icon(
                            editor,
                            px(16.),
                            if current == choice {
                                theme.base
                            } else {
                                theme.text
                            },
                        ))
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.editor_override = None;
                        this.preferences.editor.editor = choice;
                        this.settings_error = this
                            .preferences
                            .save(&this.preferences_path)
                            .err()
                            .map(|error| error.to_string());
                        cx.notify();
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(theme.muted))
                    .child("Open in editor actions use this. SigmaDock has no built-in editor."),
            )
            .child(choices)
            .when(current == Some(Editor::Custom), |section| {
                section
                    .child(self.appearance_field(
                        EDITOR_COMMAND,
                        "Command".into(),
                        self.preferences.editor.custom_command.clone(),
                        cx,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.muted))
                            .child("Placeholders: {path} {line} {column} {worktree}. Run directly, not through a shell."),
                    )
            })
            .into_any_element()
    }
}
