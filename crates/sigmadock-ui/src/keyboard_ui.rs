//! Keyboard focus and shortcuts for the berths workspace.
use crate::{Workspace, berths_ui};
use gpui::{Context, KeyDownEvent, Window, div, prelude::*, rgb};

pub(crate) struct ShortcutTooltip(pub String);
impl gpui::Render for ShortcutTooltip {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::theme::Theme::for_appearance(window.appearance());
        div()
            .p_2()
            .rounded_md()
            .bg(rgb(theme.button))
            .text_color(rgb(theme.text))
            .child(self.0.clone())
    }
}

pub(crate) fn tooltip(text: String, cx: &mut gpui::App) -> gpui::AnyView {
    cx.new(|_| ShortcutTooltip(text)).into()
}

// Three visual slots per row. Horizontal movement never crosses a row boundary.
fn adjacent(index: usize, count: usize, key: &str) -> Option<usize> {
    if index >= count {
        return None;
    }
    match key {
        "left" if !index.is_multiple_of(3) => Some(index - 1),
        "right" if index % 3 != 2 && index + 1 < count => Some(index + 1),
        "up" if index >= 3 => Some(index - 3),
        "down" if index + 3 < count => Some(index + 3),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Shortcut {
    Checks,
    Settings,
    NewTask,
    Berths,
    Inbox,
    OpenEditor,
    Project(usize),
    Next,
    Previous,
}

fn workspace_shortcut(
    key: &gpui::Keystroke,
    terminal: bool,
    terminal_focused: bool,
    form: bool,
    settings: bool,
) -> Option<Shortcut> {
    if key.key == "," && (key.modifiers.platform || key.modifiers.control) {
        return Some(Shortcut::Settings);
    }
    if key.key == "n" && key.modifiers.platform && !settings {
        return Some(Shortcut::NewTask);
    }
    if form || settings {
        return None;
    }
    if key.key == "k" && key.modifiers.platform && key.modifiers.shift {
        return Some(Shortcut::Checks);
    }
    if key.key == "[" && key.modifiers.platform && terminal {
        return Some(Shortcut::Berths);
    }
    if key.key == "o" && key.modifiers.platform && key.modifiers.shift && terminal {
        return Some(Shortcut::OpenEditor);
    }
    if key.key == "i" && key.modifiers.platform && !key.modifiers.shift {
        return Some(Shortcut::Inbox);
    }
    if key.modifiers.platform && key.key.len() == 1 && ("1"..="9").contains(&key.key.as_str()) {
        return Some(Shortcut::Project(key.key.parse::<usize>().unwrap() - 1));
    }
    if key.key == "tab"
        && !terminal_focused
        && !key.modifiers.platform
        && !key.modifiers.control
        && !key.modifiers.alt
    {
        return Some(if key.modifiers.shift {
            Shortcut::Previous
        } else {
            Shortcut::Next
        });
    }
    None
}

impl Workspace {
    pub(crate) fn prepare_berth_focus(&mut self, cx: &mut Context<Self>) {
        // One handle per grid slot, keyed exactly as the grid renders it.
        let keys: Vec<_> = self
            .slots(self.selected_project.as_deref())
            .into_iter()
            .map(|(number, worker)| {
                worker.map_or_else(
                    || format!("empty-berth-{number}"),
                    |worker| format!("berth-{}", worker.id),
                )
            })
            .collect();
        self.berth_focus.retain(|key, _| keys.contains(key));
        for key in keys {
            self.berth_focus
                .entry(key)
                .or_insert_with(|| cx.focus_handle().tab_index(0).tab_stop(true));
        }
    }

    fn focus_slot(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let id = self
            .slots(self.selected_project.as_deref())
            .get(index)
            .and_then(|(_, worker)| worker.map(|worker| worker.id.clone()));
        let key = id.as_ref().map_or_else(
            || format!("empty-berth-{}", index + 1),
            |id| format!("berth-{id}"),
        );
        self.focused_berth = id;
        if let Some(handle) = self.berth_focus.get(&key) {
            handle.focus(window);
        }
        cx.notify();
    }

    pub(crate) fn focus_worker_berth(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focused_berth = Some(id.to_owned());
        if let Some(handle) = self.berth_focus.get(&format!("berth-{id}")) {
            handle.focus(window);
        }
        cx.notify();
    }

    pub(crate) fn select_project(
        &mut self,
        project: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_open = false;
        if self.terminal.is_some() {
            self.close_terminal(window, cx);
        }
        self.selected_project = project;
        self.focused_berth = None;
        cx.notify();
    }

    pub(crate) fn berth_key(
        &mut self,
        index: usize,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.form_open || self.settings_open {
            return;
        }
        let key = &event.keystroke;
        if key.key == "enter" {
            let worker = self
                .slots(self.selected_project.as_deref())
                .get(index)
                .and_then(|(_, worker)| worker.cloned());
            if let Some(worker) = worker {
                if key.modifiers.platform {
                    if let Some(action) = berths_ui::status(&worker).action {
                        self.run_berth_action(&action, worker.id, window, cx);
                    }
                } else {
                    self.open_worker(worker.id, window, cx);
                }
            } else if !self.global_full() && !self.form_open {
                self.open_new_task(window, cx);
            }
            cx.stop_propagation();
        } else if !key.modifiers.platform
            && !key.modifiers.control
            && !key.modifiers.alt
            && matches!(key.key.as_str(), "left" | "right" | "up" | "down")
        {
            let count = self.slots(self.selected_project.as_deref()).len();
            if let Some(next) = adjacent(index, count, &key.key) {
                self.focus_slot(next, window, cx);
            }
            cx.stop_propagation();
        }
    }

    pub(crate) fn workspace_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match workspace_shortcut(
            &event.keystroke,
            self.terminal.is_some(),
            self.terminal.as_ref().is_some_and(|terminal| {
                terminal
                    .read(cx)
                    .focus_handle()
                    .contains_focused(window, cx)
            }),
            self.form_open,
            self.settings_open,
        ) {
            Some(Shortcut::Checks) => {
                let focused = self.berth_focus.iter().find_map(|(key, handle)| {
                    if handle.contains_focused(window, cx) {
                        key.strip_prefix("berth-").map(str::to_owned)
                    } else {
                        None
                    }
                });
                if let Some(id) = self
                    .selected
                    .clone()
                    .or(focused)
                    .or(self.focused_berth.clone())
                {
                    self.open_checks(id, window, cx);
                }
            }
            Some(Shortcut::Settings) => self.toggle_settings(window, cx),
            Some(Shortcut::NewTask) => {
                if self.terminal.is_some() {
                    self.close_terminal(window, cx);
                }
                self.view = crate::View::Berths;
                if !self.form_open {
                    self.open_new_task(window, cx);
                }
            }
            Some(Shortcut::Berths) => self.close_terminal(window, cx),
            Some(Shortcut::Inbox) => self.open_inbox(window, cx),
            Some(Shortcut::OpenEditor) => self.open_in_editor(None, cx),
            Some(Shortcut::Project(0)) => {
                self.view = crate::View::Berths;
                self.select_project(None, window, cx)
            }
            Some(Shortcut::Project(index)) => {
                self.view = crate::View::Berths;
                if let Some(project) = self.projects.get(index - 1) {
                    self.select_project(Some(project.id.clone()), window, cx);
                } else {
                    return;
                }
            }
            Some(Shortcut::Next) => window.focus_next(),
            Some(Shortcut::Previous) => window.focus_prev(),
            None => return,
        }
        cx.stop_propagation();
    }
}

#[cfg(test)]
mod tests {
    use super::{Shortcut, adjacent, workspace_shortcut};
    use gpui::Keystroke;
    #[test]
    fn terminal_and_form_input_stays_with_the_input_view() {
        for key in ["escape", "tab", "left", "right", "up", "down", "enter", "a"] {
            assert_eq!(
                workspace_shortcut(&Keystroke::parse(key).unwrap(), true, true, false, false),
                None,
                "{key}"
            );
        }
        for key in ["tab", "shift-tab", "cmd-1", "cmd-["] {
            assert_eq!(
                workspace_shortcut(&Keystroke::parse(key).unwrap(), false, false, true, false),
                None,
                "{key}"
            );
            assert_eq!(
                workspace_shortcut(&Keystroke::parse(key).unwrap(), false, false, false, true),
                None,
                "{key}"
            );
        }
        assert_eq!(
            workspace_shortcut(
                &Keystroke::parse("cmd-[").unwrap(),
                true,
                true,
                false,
                false
            ),
            Some(Shortcut::Berths)
        );
        assert_eq!(
            workspace_shortcut(
                &Keystroke::parse("cmd-1").unwrap(),
                false,
                false,
                false,
                false
            ),
            Some(Shortcut::Project(0))
        );
        assert_eq!(
            workspace_shortcut(
                &Keystroke::parse("cmd-9").unwrap(),
                false,
                false,
                false,
                false
            ),
            Some(Shortcut::Project(8))
        );
        assert_eq!(
            workspace_shortcut(
                &Keystroke::parse("tab").unwrap(),
                false,
                false,
                false,
                false
            ),
            Some(Shortcut::Next)
        );
        assert_eq!(
            workspace_shortcut(
                &Keystroke::parse("shift-tab").unwrap(),
                false,
                false,
                false,
                false
            ),
            Some(Shortcut::Previous)
        );
        assert_eq!(
            workspace_shortcut(
                &Keystroke::parse("cmd-n").unwrap(),
                false,
                false,
                false,
                false
            ),
            Some(Shortcut::NewTask)
        );
        assert_eq!(
            workspace_shortcut(
                &Keystroke::parse("cmd-i").unwrap(),
                true,
                true,
                false,
                false
            ),
            Some(Shortcut::Inbox)
        );
        assert_eq!(
            workspace_shortcut(
                &Keystroke::parse("cmd-shift-o").unwrap(),
                true,
                true,
                false,
                false
            ),
            Some(Shortcut::OpenEditor)
        );
        assert_eq!(
            workspace_shortcut(
                &Keystroke::parse("cmd-shift-o").unwrap(),
                false,
                false,
                false,
                false
            ),
            None
        );
        assert_eq!(
            workspace_shortcut(
                &Keystroke::parse("cmd-,").unwrap(),
                false,
                false,
                false,
                true
            ),
            Some(Shortcut::Settings)
        );
    }
    #[test]
    fn checks_shortcut_works_for_berths_and_terminal_but_not_forms() {
        let key = Keystroke::parse("cmd-shift-k").unwrap();
        assert_eq!(
            workspace_shortcut(&key, false, false, false, false),
            Some(Shortcut::Checks)
        );
        assert_eq!(
            workspace_shortcut(&key, true, true, false, false),
            Some(Shortcut::Checks)
        );
        assert_eq!(workspace_shortcut(&key, false, false, true, false), None);
        assert_eq!(workspace_shortcut(&key, false, false, false, true), None);
    }
    #[test]
    fn grid_navigation_respects_rows_and_partial_last_row() {
        assert_eq!(adjacent(0, 5, "left"), None);
        assert_eq!(adjacent(2, 5, "right"), None);
        assert_eq!(adjacent(3, 5, "left"), None);
        assert_eq!(adjacent(0, 5, "right"), Some(1));
        assert_eq!(adjacent(1, 5, "down"), Some(4));
        assert_eq!(adjacent(4, 5, "up"), Some(1));
        assert_eq!(adjacent(2, 5, "down"), None);
        assert_eq!(adjacent(4, 5, "right"), None);
        assert_eq!(adjacent(0, 0, "down"), None);
        assert_eq!(adjacent(5, 5, "up"), None);
    }
}
