//! UI-owned editor PTY, independent of daemon workers, scripts and berth capacity.
use crate::{
    Workspace,
    editor::{Editor, Target},
};
use anyhow::{Context as _, Result};
use gpui::{Context, Entity, Window, div, prelude::*, px, rgb};
use sigmadock_pty::Session;
use sigmadock_terminal::{TerminalConfig, TerminalView};
use std::{
    io::{self, Read, Write},
    sync::Arc,
    time::Duration,
};

pub(crate) struct EditorPane {
    pub terminal: Entity<TerminalView>,
    pub(crate) session: Arc<Session>,
    title: String,
}
impl Drop for EditorPane {
    fn drop(&mut self) {
        let session = self.session.clone();
        // Process-group shutdown can take seconds; never block the GPUI thread.
        std::thread::spawn(move || {
            let _ = session.stop();
        });
    }
}
struct Reader {
    session: Arc<Session>,
    cursor: u64,
    pending: Vec<u8>,
    offset: usize,
    ended: bool,
}
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            if self.offset < self.pending.len() {
                let n = bytes.len().min(self.pending.len() - self.offset);
                bytes[..n].copy_from_slice(&self.pending[self.offset..self.offset + n]);
                self.offset += n;
                return Ok(n);
            }
            if self.ended {
                return Ok(0);
            }
            let output = self.session.output(self.cursor);
            self.cursor = output.cursor;
            self.pending = output.bytes;
            self.offset = 0;
            self.ended = output.exited;
            if self.pending.is_empty() && !self.ended {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}
struct Writer(Arc<Session>);
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes).map_err(io::Error::other)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Workspace {
    pub(crate) fn active_editor(&self) -> Editor {
        self.editor_override
            .unwrap_or_else(|| self.preferences.editor.resolved())
    }
    pub(crate) fn open_editor_terminal(
        &mut self,
        target: &Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        // Keep an existing editor (and any unsaved buffers) until explicitly closed.
        if self
            .editor_pane
            .as_ref()
            .is_some_and(|pane| !pane.session.output_signal().exited)
        {
            anyhow::bail!(
                "An editor is already open. Save your buffers and close that editor pane before opening another target."
            );
        }
        let (program, args) = self
            .preferences
            .editor
            .command(Editor::Environment, target)?;
        let session = Arc::new(Session::spawn(
            program
                .to_str()
                .context("Editor executable path is not UTF-8")?,
            &args,
            &[
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
            ],
            &target.worktree,
        )?);
        let reader = Reader {
            session: session.clone(),
            cursor: 0,
            pending: Vec::new(),
            offset: 0,
            ended: false,
        };
        let writer = Writer(session.clone());
        let resize = session.clone();
        let config = self
            .theme
            .terminal(&self.preferences.appearance)
            .apply(TerminalConfig::default());
        let terminal = cx.new(|cx| {
            TerminalView::new(writer, reader, config, cx).with_resize_callback(move |cols, rows| {
                let _ = resize.resize(
                    rows.clamp(1, u16::MAX as usize) as u16,
                    cols.clamp(1, u16::MAX as usize) as u16,
                );
            })
        });
        terminal.read(cx).focus_handle().focus(window);
        self.editor_pane = Some(EditorPane {
            terminal,
            session,
            title: target.file.as_ref().map_or_else(
                || target.worktree.display().to_string(),
                |(path, _)| path.display().to_string(),
            ),
        });
        Ok(())
    }
    pub(crate) fn editor_terminal_panel(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let pane = self.editor_pane.as_ref()?;
        Some(
            div()
                .h(px(300.))
                .flex_none()
                .min_h(px(200.))
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(rgb(self.theme.border))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .p_2()
                        .text_sm()
                        .child(format!("Terminal editor · {}", pane.title))
                        .child(
                            div()
                                .id("close-editor-pane")
                                .cursor_pointer()
                                .child("Close editor (save first)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.editor_pane = None;
                                    if let Some(terminal) = &this.terminal {
                                        terminal.read(cx).focus_handle().focus(window);
                                    } else {
                                        this.workspace_focus.focus(window);
                                    }
                                    cx.notify();
                                })),
                        ),
                )
                .child(div().flex_1().p_2().child(pane.terminal.clone()))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_pty_roundtrips_input_resize_and_output_without_a_worker() {
        let session = Arc::new(
            Session::spawn(
                "/bin/sh",
                &[
                    "-c".into(),
                    "printf '%s\\n' \"$@\"; IFS= read -r line; printf 'input:%s\\n' \"$line\""
                        .into(),
                    "editor".into(),
                    "+42".into(),
                    "/work/tree with space/a;$(literal).rs".into(),
                ],
                &[("TERM".into(), "xterm-256color".into())],
                std::path::Path::new("/tmp"),
            )
            .unwrap(),
        );
        session.resize(24, 80).unwrap();
        assert_eq!(session.output(0).rows, Some(24));
        assert_eq!(session.output(0).cols, Some(80));
        Writer(session.clone()).write_all(b"saved\n").unwrap();
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while !session.output_signal().exited {
            assert!(std::time::Instant::now() < end, "editor did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut reader = Reader {
            session,
            cursor: 0,
            pending: Vec::new(),
            offset: 0,
            ended: false,
        };
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        assert!(output.contains("+42"));
        assert!(output.contains("/work/tree with space/a;$(literal).rs"));
        assert!(output.contains("input:saved"));
        assert_eq!(reader.read(&mut [0; 8]).unwrap(), 0);
    }
}
