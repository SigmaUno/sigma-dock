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
        if let Some(terminal) = &self.terminal {
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
    /// Terminal appearance controls for the Settings page.
    pub(crate) fn appearance_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let effective = self.theme.terminal(&self.preferences.appearance);
        let appearance = &effective;
        let mut panel = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(self.theme.muted))
                    .child("Changes apply live. Click a value; Cmd/Ctrl+A replaces it."),
            )
            .child(self.appearance_field(0, "Font family".into(), appearance.font.clone(), cx));
        let mut fonts = div().flex().gap_2();
        for font in ["monospace", "Menlo", "JetBrains Mono"] {
            fonts = fonts.child(
                div()
                    .id(SharedString::from(format!("font-{font}")))
                    .p_1()
                    .bg(rgb(self.theme.button))
                    .hover(|style| style.bg(rgb(self.theme.selection)))
                    .cursor_pointer()
                    .text_sm()
                    .child(font)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_editor = None;
                        this.preferences.appearance.font = font.into();
                        this.apply_appearance(cx);
                    })),
            );
        }
        panel = panel.child(fonts);
        for (index, label, value) in [
            (0, "Font size", format!("{} px", appearance.size)),
            (1, "Line spacing", format!("{:.1}×", appearance.line_height)),
            (2, "Padding", format!("{} px", appearance.padding)),
        ] {
            let mut row = div()
                .flex()
                .gap_2()
                .items_center()
                .child(div().w(px(130.)).child(label))
                .child(div().flex_1().child(value));
            for (direction, label) in [(-1., "−"), (1., "+")] {
                row = row.child(
                    div()
                        .id(SharedString::from(format!("step-{index}-{direction}")))
                        .p_2()
                        .bg(rgb(self.theme.button))
                        .hover(|style| style.bg(rgb(self.theme.selection)))
                        .cursor_pointer()
                        .child(label)
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
                        })),
                );
            }
            panel = panel.child(row);
        }
        let mut themes = div().flex().gap_2().child(div().w(px(130.)).child("Theme"));
        for (light, label) in [(false, "Dark"), (true, "Light")] {
            themes = themes.child(
                div()
                    .id(SharedString::from(format!("theme-{label}")))
                    .p_2()
                    .bg(rgb(self.theme.button))
                    .hover(|style| style.bg(rgb(self.theme.selection)))
                    .cursor_pointer()
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let preset = if light {
                            Appearance::light()
                        } else {
                            Appearance::default()
                        };
                        let a = &mut this.preferences.appearance;
                        a.follow_system = false;
                        a.background = preset.background;
                        a.foreground = preset.foreground;
                        a.ansi = preset.ansi;
                        this.settings_editor = None;
                        this.apply_appearance(cx);
                    })),
            );
        }
        panel = panel
            .child(
                div()
                    .id("terminal-system-theme")
                    .p_2()
                    .bg(rgb(self.theme.button))
                    .cursor_pointer()
                    .child(if appearance.follow_system {
                        "Terminal colors: follow system"
                    } else {
                        "Terminal colors: custom"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        let effective = this.theme.terminal(&this.preferences.appearance);
                        let a = &mut this.preferences.appearance;
                        if a.follow_system {
                            a.background = effective.background;
                            a.foreground = effective.foreground;
                            a.ansi = effective.ansi;
                        }
                        a.follow_system = !a.follow_system;
                        this.settings_editor = None;
                        this.apply_appearance(cx);
                    })),
            )
            .child(themes)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .id("cursor-shape")
                            .flex_1()
                            .p_2()
                            .bg(rgb(self.theme.button))
                            .hover(|style| style.bg(rgb(self.theme.selection)))
                            .cursor_pointer()
                            .child(format!("Cursor: {:?} ↻", appearance.cursor))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.preferences.appearance.cursor =
                                    match this.preferences.appearance.cursor {
                                        Cursor::Block => Cursor::Underline,
                                        Cursor::Underline => Cursor::Beam,
                                        Cursor::Beam => Cursor::Block,
                                    };
                                this.apply_appearance(cx);
                            })),
                    )
                    .child(
                        div()
                            .id("cursor-blink")
                            .p_2()
                            .bg(rgb(self.theme.button))
                            .hover(|style| style.bg(rgb(self.theme.selection)))
                            .cursor_pointer()
                            .child(if appearance.blink {
                                "Blink: on"
                            } else {
                                "Blink: off"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.preferences.appearance.blink =
                                    !this.preferences.appearance.blink;
                                this.apply_appearance(cx);
                            })),
                    ),
            );
        for index in 0..18 {
            let label = match index {
                0 => "Background".into(),
                1 => "Foreground".into(),
                _ => format!("ANSI {}", index - 2),
            };
            panel = panel.child(self.appearance_field(
                index + 1,
                label,
                format!("#{:06x}", appearance.color(index)),
                cx,
            ));
        }
        panel
            .child(
                div()
                    .id("restore-appearance")
                    .p_2()
                    .bg(rgb(self.theme.accent))
                    .text_color(rgb(self.theme.base))
                    .cursor_pointer()
                    .child("Restore appearance defaults")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_editor = None;
                        this.preferences.appearance = Appearance::default();
                        this.apply_appearance(cx);
                    })),
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
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
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
