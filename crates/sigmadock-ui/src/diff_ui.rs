//! Changes pane: the worker's patch since it forked, with open-in-editor actions.
use crate::ellipsis::Ellipsis;
use crate::{
    Workspace,
    editor::Target,
    icons::{Icon, icon},
};
use gpui::{
    Context, FontWeight, ScrollStrategy, SharedString, UniformListScrollHandle, div, prelude::*,
    px, rgb, rgba, uniform_list,
};
use sigmadock_core::diff::{DiffReport, DiffSectionKind};
use std::path::PathBuf;

const MONO: &str = "Menlo";
const ROW_HEIGHT: f32 = 22.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Line {
    pub kind: LineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hunk {
    pub header: String,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileDiff {
    pub section: DiffSectionKind,
    pub blob_id: String,
    pub viewed: bool,
    pub truncated: bool,
    pub path: String,
    pub change: Change,
    pub added: usize,
    pub removed: usize,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// Line to open the file at: the first added line, else where the first removal was.
    pub(crate) fn first_change(&self) -> u32 {
        self.hunks
            .iter()
            .flat_map(|hunk| {
                let mut new = None;
                hunk.lines.iter().find_map(move |line| {
                    if line.new.is_some() {
                        new = line.new;
                    }
                    match line.kind {
                        LineKind::Added => line.new,
                        LineKind::Removed => Some(new.map_or(1, |n| n + 1)),
                        LineKind::Context => None,
                    }
                })
            })
            .next()
            .unwrap_or(1)
    }
}

fn unquote(path: &str) -> String {
    path.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(path)
        .to_owned()
}

fn hunk_starts(header: &str) -> (u32, u32) {
    let mut parts = header.split_whitespace().skip(1);
    let mut start = |prefix: char| {
        parts
            .next()
            .and_then(|part| part.strip_prefix(prefix))
            .and_then(|part| part.split(',').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(1)
    };
    let old = start('-');
    (old, start('+'))
}

/// Parses `git diff` output. Unknown lines are ignored rather than failing the pane.
pub(crate) fn parse(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let (mut old, mut new) = (0, 0);
    for raw in text.lines() {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            let path = rest
                .rsplit_once(" b/")
                .map_or(rest.to_owned(), |(_, b)| unquote(b));
            files.push(FileDiff {
                section: DiffSectionKind::Committed,
                blob_id: String::new(),
                viewed: false,
                truncated: false,
                path,
                change: Change::Modified,
                added: 0,
                removed: 0,
                binary: false,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if file.hunks.is_empty() {
            if raw.starts_with("new file mode") {
                file.change = Change::Added;
            } else if raw.starts_with("deleted file mode") {
                file.change = Change::Deleted;
            } else if let Some(to) = raw.strip_prefix("rename to ") {
                file.change = Change::Renamed;
                file.path = unquote(to);
            } else if let Some(path) = raw.strip_prefix("+++ ") {
                if let Some(path) = unquote(path).strip_prefix("b/") {
                    file.path = path.to_owned();
                }
                continue;
            } else if let Some(path) = raw.strip_prefix("--- ") {
                if file.change == Change::Deleted
                    && let Some(path) = unquote(path).strip_prefix("a/")
                {
                    file.path = path.to_owned();
                }
                continue;
            } else if raw.starts_with("Binary files") {
                file.binary = true;
            }
        }
        if raw.starts_with("@@") {
            (old, new) = hunk_starts(raw);
            file.hunks.push(Hunk {
                header: raw.to_owned(),
                lines: Vec::new(),
            });
            continue;
        }
        let Some(hunk) = file.hunks.last_mut() else {
            continue;
        };
        let (kind, text) = match raw.as_bytes().first() {
            Some(b'+') => (LineKind::Added, &raw[1..]),
            Some(b'-') => (LineKind::Removed, &raw[1..]),
            Some(b' ') => (LineKind::Context, &raw[1..]),
            None => (LineKind::Context, ""),
            _ => continue,
        };
        let line = Line {
            kind,
            old: (kind != LineKind::Added).then_some(old),
            new: (kind != LineKind::Removed).then_some(new),
            text: text.replace('\t', "    "),
        };
        match kind {
            LineKind::Added => {
                new += 1;
                file.added += 1;
            }
            LineKind::Removed => {
                old += 1;
                file.removed += 1;
            }
            LineKind::Context => {
                old += 1;
                new += 1;
            }
        }
        hunk.lines.push(line);
    }
    files
}

/// One virtual-list row of the changes pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Section(DiffSectionKind),
    File(usize),
    Hunk(usize, usize),
    Line(usize, usize, usize),
    Binary(usize),
}

/// The worker's parsed patch and where each file starts in the row list.
pub(crate) struct Changes {
    pub base_ref: String,
    pub merge_base: String,
    pub warnings: Vec<String>,
    pub files: Vec<FileDiff>,
    pub truncated: bool,
    rows: Vec<Row>,
    file_rows: Vec<usize>,
}

impl Changes {
    #[cfg(test)]
    pub(crate) fn new(text: &str, truncated: bool) -> Self {
        Self::from_files(
            parse(text),
            truncated,
            String::new(),
            String::new(),
            Vec::new(),
        )
    }
    fn from_report(report: DiffReport, worker: &str, viewed: &crate::viewed::Viewed) -> Self {
        let mut files = Vec::new();
        for section in report.sections {
            for item in section.files {
                let parsed = parse(&item.patch);
                let hunks = parsed.into_iter().flat_map(|file| file.hunks).collect();
                files.push(FileDiff {
                    section: section.kind,
                    viewed: !item.truncated
                        && viewed.contains(worker, section.kind.key(), &item.path, &item.blob_id),
                    path: item.path,
                    blob_id: item.blob_id,
                    truncated: item.truncated,
                    change: match item.status.as_str() {
                        "A" => Change::Added,
                        "D" => Change::Deleted,
                        "R" => Change::Renamed,
                        _ => Change::Modified,
                    },
                    added: item.added.unwrap_or(0) as usize,
                    removed: item.removed.unwrap_or(0) as usize,
                    binary: item.binary,
                    hunks,
                });
            }
        }
        Self::from_files(
            files,
            report.truncated,
            report.base_ref,
            report.merge_base,
            report.warnings,
        )
    }
    fn from_files(
        files: Vec<FileDiff>,
        truncated: bool,
        base_ref: String,
        merge_base: String,
        warnings: Vec<String>,
    ) -> Self {
        let mut rows = Vec::new();
        let mut file_rows = Vec::new();
        let mut section = None;
        for (f, file) in files.iter().enumerate() {
            if section != Some(file.section) {
                rows.push(Row::Section(file.section));
                section = Some(file.section);
            }
            file_rows.push(rows.len());
            rows.push(Row::File(f));
            if file.binary {
                rows.push(Row::Binary(f));
            }
            for (h, hunk) in file.hunks.iter().enumerate() {
                rows.push(Row::Hunk(f, h));
                rows.extend((0..hunk.lines.len()).map(|l| Row::Line(f, h, l)));
            }
        }
        Self {
            base_ref,
            merge_base,
            warnings,
            files,
            truncated,
            rows,
            file_rows,
        }
    }
    fn totals(&self) -> (usize, usize) {
        self.files
            .iter()
            .fold((0, 0), |(a, r), file| (a + file.added, r + file.removed))
    }
}

/// Diff pane state owned by the workspace.
#[derive(Default)]
pub(crate) struct DiffState {
    pub changes: Option<Changes>,
    pub loading: bool,
    pub error: Option<String>,
    pub scroll: UniformListScrollHandle,
    pub selected: Option<usize>,
    pub request: u64,
    pub viewed: crate::viewed::Viewed,
    /// A refresh was clicked while another load was running; it runs when that one ends.
    pub queued: bool,
    /// The running load was asked for by the refresh button, so its result is confirmed.
    pub manual: bool,
    /// Briefly true after a clicked refresh finishes.
    pub confirmed: bool,
}

/// How long the Changes header says "Updated" after a clicked refresh.
const REFRESH_CONFIRMATION: std::time::Duration = std::time::Duration::from_secs(2);

impl Workspace {
    /// The refresh button: never dropped, even while the periodic refresh is loading.
    pub(crate) fn refresh_changes(&mut self, cx: &mut Context<Self>) {
        self.diff.manual = true;
        self.diff.confirmed = false;
        if self.diff.loading {
            self.diff.queued = true;
            cx.notify();
            return;
        }
        self.load_changes(cx);
        cx.notify();
    }
    pub(crate) fn load_changes(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        if self.diff.loading {
            return;
        }
        self.diff.loading = true;
        self.diff.request += 1;
        let request = self.diff.request;
        let viewed_path = self.preferences_path.with_file_name("viewed-diffs.json");
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let worker = id.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let value =
                        client.call("diff_patch", serde_json::json!({"worker_id": worker}))?;
                    let report = serde_json::from_value::<DiffReport>(value)?;
                    let viewed = crate::viewed::Viewed::load(&viewed_path)?;
                    Ok::<_, anyhow::Error>((report, viewed))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // A newer request owns the loading state; this result is stale.
                if this.diff.request != request {
                    return;
                }
                this.diff.loading = false;
                if this.selected.as_ref() != Some(&id) {
                    this.diff.queued = false;
                    this.diff.manual = false;
                    return;
                }
                if std::mem::take(&mut this.diff.queued) {
                    // The click came during this load; show what changed after it.
                    this.load_changes(cx);
                    cx.notify();
                    return;
                }
                if std::mem::take(&mut this.diff.manual) && result.is_ok() {
                    this.diff.confirmed = true;
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(REFRESH_CONFIRMATION).await;
                        let _ = this.update(cx, |this, cx| {
                            if this.diff.request == request {
                                this.diff.confirmed = false;
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
                match result {
                    Ok((report, viewed)) => {
                        let changes = Changes::from_report(report, &id, &viewed);
                        this.diff.viewed = viewed;
                        this.diff.selected =
                            this.diff.selected.filter(|&f| f < changes.files.len());
                        this.diff.changes = Some(changes);
                        this.diff.error = None;
                    }
                    Err(error) => {
                        this.diff.error = Some(if error.to_string().contains("unknown method") {
                            "Restart the SigmaDock daemon to load full diffs.".into()
                        } else {
                            error.to_string()
                        })
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn mark_viewed(
        &mut self,
        worker: &str,
        section: DiffSectionKind,
        path: &str,
        blob: &str,
        viewed: bool,
        cx: &mut Context<Self>,
    ) {
        if self.selected.as_deref() != Some(worker)
            || self.diff.changes.as_ref().is_none_or(|changes| {
                !changes.files.iter().any(|file| {
                    file.path == path
                        && file.section == section
                        && file.blob_id == blob
                        && !file.truncated
                })
            })
        {
            return;
        }
        let mut state = self.diff.viewed.clone();
        state.set(worker, section.key(), path, blob, viewed);
        match state.save(&self.preferences_path.with_file_name("viewed-diffs.json")) {
            Ok(()) => {
                // Discard a refresh that read the markers before this click.
                self.diff.request += 1;
                self.diff.loading = false;
                self.diff.viewed = state;
                if let Some(changes) = &mut self.diff.changes {
                    for file in &mut changes.files {
                        if file.path == path && file.section == section && file.blob_id == blob {
                            file.viewed = viewed;
                        }
                    }
                }
                self.diff.error = None;
            }
            Err(error) => {
                self.diff.error = Some(format!("Viewed state could not be saved: {error}"))
            }
        }
        cx.notify();
    }

    pub(crate) fn open_in_editor(
        &mut self,
        file: Option<(String, u32)>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(worker) = self
            .selected
            .as_ref()
            .and_then(|id| self.workers.iter().find(|worker| &worker.id == id))
        else {
            return;
        };
        let target = Target {
            worktree: worker.worktree.clone(),
            file: file.map(|(path, line)| (PathBuf::from(path), line)),
        };
        let editor = self.active_editor();
        let result = if editor == crate::editor::Editor::Environment {
            self.open_editor_terminal(&target, window, cx)
        } else {
            let mut preferences = self.preferences.editor.clone();
            preferences.editor = Some(editor);
            preferences.open(&target).map(|_| ())
        };
        self.editor_error = result.err().map(|error| format!("{error:#}"));
        self.error = self.editor_error.clone();
        cx.notify();
    }

    fn change_badge(&self, change: Change) -> gpui::Div {
        let theme = self.theme;
        let (letter, color) = match change {
            Change::Added => ("A", theme.success),
            Change::Modified => ("M", theme.warning),
            Change::Deleted => ("D", theme.error),
            Change::Renamed => ("R", theme.link),
        };
        div()
            .size(px(16.))
            .flex_none()
            .rounded_sm()
            .bg(rgb(color))
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(theme.surface))
            .text_size(px(10.))
            .font_weight(FontWeight::BOLD)
            .child(letter)
    }

    fn open_button(&self, id: SharedString, label: Option<String>) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        let editor = self.active_editor().label();
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .px_1p5()
            .py_0p5()
            .rounded_md()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface))
            .cursor_pointer()
            .hover(|style| style.border_color(rgb(theme.accent)))
            .text_xs()
            .text_color(rgb(theme.text))
            .child(crate::editor_icons::editor_icon(
                self.active_editor(),
                px(12.),
                theme.accent,
            ))
            .children(label)
            .tooltip(move |_, cx| crate::keyboard_ui::tooltip(format!("Open in {editor}"), cx))
    }

    fn change_row(&self, ix: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let Some(changes) = &self.diff.changes else {
            return div().into_any_element();
        };
        let row = changes.rows[ix];
        let base = div()
            .id(ix)
            .h(px(ROW_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .font_family(MONO)
            .text_size(px(13.))
            .whitespace_nowrap()
            .overflow_hidden();
        match row {
            Row::Section(kind) => base
                .px_3()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme.link))
                .child(kind.label())
                .into_any_element(),
            Row::File(f) => {
                let file = &changes.files[f];
                let path = file.path.clone();
                let line = file.first_change();
                base.mt_2()
                    .px_3()
                    .gap_2()
                    .bg(rgb(theme.card))
                    .text_size(px(12.))
                    .text_color(rgb(theme.muted))
                    .child(div().flex_1().min_w(px(0.)).ellipsis().child(path.clone()))
                    .when(file.change != Change::Deleted, |row| {
                        row.child(
                            self.open_button(
                                SharedString::from(format!("open-file-{f}")),
                                Some(format!("Open at {line}")),
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_in_editor(Some((path.clone(), line)), window, cx)
                                },
                            )),
                        )
                    })
                    .into_any_element()
            }
            Row::Binary(_) => base
                .px_3()
                .text_color(rgb(theme.muted))
                .child("Binary file changed")
                .into_any_element(),
            Row::Hunk(f, h) => base
                .px_3()
                .bg(rgba((theme.link << 8) | 0x14))
                .text_color(rgb(theme.link))
                .child(changes.files[f].hunks[h].header.clone())
                .into_any_element(),
            Row::Line(f, h, l) => {
                let file = &changes.files[f];
                let line = &file.hunks[h].lines[l];
                let (bg, sign, sign_color) = match line.kind {
                    LineKind::Added => (Some(theme.success), "+", theme.success),
                    LineKind::Removed => (Some(theme.error), "−", theme.error),
                    LineKind::Context => (None, " ", theme.muted),
                };
                let number = line.new.or(line.old).unwrap_or(0);
                let target = line.new.map(|n| (file.path.clone(), n));
                let group = SharedString::from(format!("diff-line-{ix}"));
                base.group(group.clone())
                    .relative()
                    .when_some(bg, |row, color| row.bg(rgba((color << 8) | 0x1f)))
                    .child(
                        div()
                            .w(px(48.))
                            .flex_none()
                            .pr_2()
                            .text_right()
                            .text_color(rgb(theme.muted))
                            .child(number.to_string()),
                    )
                    .child(
                        div()
                            .w(px(16.))
                            .flex_none()
                            .text_color(rgb(sign_color))
                            .child(sign),
                    )
                    .child(div().text_color(rgb(theme.text)).child(line.text.clone()))
                    .when_some(target, |row, (path, n)| {
                        row.child(
                            div()
                                .absolute()
                                .right(px(8.))
                                .opacity(0.)
                                .group_hover(group, |style| style.opacity(1.))
                                .child(
                                    self.open_button(
                                        SharedString::from(format!("open-line-{ix}")),
                                        Some(format!("Line {n}")),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            this.open_in_editor(Some((path.clone(), n)), window, cx)
                                        },
                                    )),
                                ),
                        )
                    })
                    .into_any_element()
            }
        }
    }

    pub(crate) fn changes_pane(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        // Only clicked refreshes show progress; the periodic one stays quiet.
        let refreshing = self.diff.manual && (self.diff.loading || self.diff.queued);
        let header = div()
            .h(px(56.))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .border_b_1()
            .border_color(rgb(theme.border))
            .child(self.right_tabs(cx))
            .child(
                div()
                    .id("refresh-changes")
                    .flex()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .p_1()
                    .rounded_md()
                    .hover(|style| style.bg(rgb(theme.panel)))
                    .child(icon(
                        Icon::Refresh,
                        px(13.),
                        rgb(if refreshing {
                            theme.accent
                        } else {
                            theme.muted
                        }),
                    ))
                    .when_some(
                        if refreshing {
                            Some("Refreshing…")
                        } else if self.diff.confirmed {
                            Some("Updated")
                        } else {
                            None
                        },
                        |button, label| {
                            button.child(div().text_xs().text_color(rgb(theme.muted)).child(label))
                        },
                    )
                    .tooltip(|_, cx| crate::keyboard_ui::tooltip("Refresh changes".into(), cx))
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_changes(cx))),
            );
        let mut pane = div()
            .id("changes-pane")
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.sidebar))
            .border_l_1()
            .border_color(rgb(theme.border));
        let Some(changes) = &self.diff.changes else {
            let message = self.diff.error.clone().unwrap_or_else(|| {
                if self.diff.loading {
                    "Loading changes…".into()
                } else {
                    "No changes loaded".into()
                }
            });
            return pane
                .child(header)
                .child(
                    div()
                        .p_4()
                        .text_sm()
                        .text_color(rgb(if self.diff.error.is_some() {
                            theme.error
                        } else {
                            theme.muted
                        }))
                        .child(message),
                )
                .into_any_element();
        };
        let (added, removed) = changes.totals();
        let header = header
            .child(
                div()
                    .px_2()
                    .rounded_full()
                    .bg(rgb(theme.chip))
                    .text_xs()
                    .text_color(rgb(theme.muted))
                    .child(format!(
                        "{} file{}",
                        changes.files.len(),
                        if changes.files.len() == 1 { "" } else { "s" }
                    )),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .gap_2()
                    .font_family(MONO)
                    .text_xs()
                    .child(
                        div()
                            .text_color(rgb(theme.success))
                            .child(format!("+{added}")),
                    )
                    .child(
                        div()
                            .text_color(rgb(theme.error))
                            .child(format!("−{removed}")),
                    ),
            );
        pane = pane.child(header).child(
            div()
                .px_4()
                .py_1()
                .text_xs()
                .text_color(rgb(theme.muted))
                .child(format!(
                    "Base {} · merge base {}",
                    changes.base_ref, changes.merge_base
                )),
        );
        if let Some(error) = &self.diff.error {
            pane = pane.child(div().text_color(rgb(theme.error)).child(error.clone()));
        }
        for warning in &changes.warnings {
            pane = pane.child(
                div()
                    .px_4()
                    .text_xs()
                    .text_color(rgb(theme.warning))
                    .child(warning.clone()),
            );
        }
        if changes.files.is_empty() {
            return pane
                .child(
                    div()
                        .p_4()
                        .text_sm()
                        .text_color(rgb(theme.muted))
                        .child("No changes against the merge base"),
                )
                .into_any_element();
        }
        let mut list = div()
            .id("changed-files")
            .flex_none()
            .max_h(px(180.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_0p5()
            .p_2()
            .border_b_1()
            .border_color(rgb(theme.border));
        let mut section = None;
        for (f, file) in changes.files.iter().enumerate() {
            if section != Some(file.section) {
                list = list.child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(theme.link))
                        .child(file.section.label()),
                );
                section = Some(file.section);
            }
            let selected = self.diff.selected == Some(f);
            let path = file.path.clone();
            let line = file.first_change();
            let row_ix = changes.file_rows[f];
            list = list.child(
                div()
                    .id(SharedString::from(format!("changed-file-{f}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(rgb(theme.base)))
                    .hover(|style| style.bg(rgb(theme.base)))
                    .font_family(MONO)
                    .text_size(px(12.5))
                    .child(self.change_badge(file.change))
                    .child({
                        let blob = file.blob_id.clone();
                        let path = file.path.clone();
                        let section = file.section;
                        let worker = self.selected.clone();
                        let viewed = file.viewed;
                        let truncated = file.truncated;
                        div()
                            .id(SharedString::from(format!("viewed-{f}")))
                            .tab_index(0)
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .focus(|style| style.border_color(rgb(theme.focus)))
                            .cursor_pointer()
                            .child(if truncated {
                                "—"
                            } else if viewed {
                                "☑"
                            } else {
                                "☐"
                            })
                            .tooltip(move |_, cx| {
                                crate::keyboard_ui::tooltip(
                                    if truncated {
                                        "Patch truncated; cannot mark as viewed".into()
                                    } else {
                                        "Mark file as viewed".into()
                                    },
                                    cx,
                                )
                            })
                            .on_key_down(cx.listener({
                                let worker = worker.clone();
                                let path = path.clone();
                                let blob = blob.clone();
                                move |this, event: &gpui::KeyDownEvent, _, cx| {
                                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                        cx.stop_propagation();
                                        if !truncated && let Some(worker) = &worker {
                                            this.mark_viewed(
                                                worker, section, &path, &blob, !viewed, cx,
                                            );
                                        }
                                    }
                                }
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if !truncated && let Some(worker) = &worker {
                                    this.mark_viewed(worker, section, &path, &blob, !viewed, cx);
                                }
                            }))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .ellipsis()
                            .text_color(rgb(if selected { theme.text } else { theme.muted }))
                            .child(file.path.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .text_xs()
                            .when(file.added > 0, |row| {
                                row.child(
                                    div()
                                        .text_color(rgb(theme.success))
                                        .child(format!("+{}", file.added)),
                                )
                            })
                            .when(file.removed > 0, |row| {
                                row.child(
                                    div()
                                        .text_color(rgb(theme.error))
                                        .child(format!("−{}", file.removed)),
                                )
                            }),
                    )
                    .when(file.change != Change::Deleted, |row| {
                        let path = path.clone();
                        row.child(
                            self.open_button(SharedString::from(format!("open-{f}")), None)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.open_in_editor(Some((path.clone(), line)), window, cx)
                                })),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.diff.selected = Some(f);
                        this.diff
                            .scroll
                            .scroll_to_item_strict(row_ix, ScrollStrategy::Top);
                        cx.notify();
                    })),
            );
        }
        pane = pane.child(list);
        if changes.truncated {
            pane = pane.child(
                div()
                    .px_4()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(theme.warning))
                    .child("Diff truncated; open the worktree for the remainder"),
            );
        }
        let count = changes.rows.len();
        pane.child(
            uniform_list(
                "change-rows",
                count,
                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                    range.map(|ix| this.change_row(ix, cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.diff.scroll.clone())
            .flex_1()
            .pb_4(),
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,4 +10,5 @@ fn main() {
 keep
-old
+new
+more
 tail
diff --git a/new.txt b/new.txt
new file mode 100644
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+fresh
\\ No newline at end of file
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/img.png b/img.png
Binary files a/img.png and b/img.png differ
";

    #[test]
    fn structured_metadata_preserves_paths_sections_and_viewed_identity() {
        use sigmadock_core::diff::{DiffFile, DiffSection};
        let mut viewed = crate::viewed::Viewed::default();
        viewed.set("worker", "committed", "quoted\tfile", "blob", true);
        let make_report = |blob: &str, truncated| DiffReport {
            base_ref: "refs/heads/main".into(),
            merge_base: "base".into(),
            head: "head".into(),
            warnings: vec![],
            truncated,
            sections: vec![DiffSection {
                kind: DiffSectionKind::Committed,
                files: vec![DiffFile {
                    path: "quoted\tfile".into(),
                    status: "M".into(),
                    added: Some(2),
                    removed: Some(1),
                    binary: false,
                    blob_id: blob.into(),
                    patch: PATCH.into(),
                    truncated,
                }],
            }],
        };
        let changes = Changes::from_report(make_report("blob", false), "worker", &viewed);
        assert_eq!(changes.files[0].path, "quoted\tfile");
        assert!(changes.files[0].viewed);
        assert_eq!(changes.rows[0], Row::Section(DiffSectionKind::Committed));
        assert!(
            !Changes::from_report(make_report("new", false), "worker", &viewed).files[0].viewed
        );
        assert!(
            !Changes::from_report(make_report("blob", true), "worker", &viewed).files[0].viewed
        );
        assert!(
            !Changes::from_report(make_report("blob", false), "other", &viewed).files[0].viewed
        );
    }

    #[test]
    fn parses_files_hunks_and_line_numbers() {
        let files = parse(PATCH);
        assert_eq!(files.len(), 4);
        let lib = &files[0];
        assert_eq!(
            (lib.path.as_str(), lib.change),
            ("src/lib.rs", Change::Modified)
        );
        assert_eq!((lib.added, lib.removed), (2, 1));
        let lines = &lib.hunks[0].lines;
        assert_eq!((lines[0].old, lines[0].new), (Some(10), Some(10)));
        assert_eq!(
            (lines[1].kind, lines[1].old, lines[1].new),
            (LineKind::Removed, Some(11), None)
        );
        assert_eq!((lines[2].kind, lines[2].new), (LineKind::Added, Some(11)));
        assert_eq!(lines[4].new, Some(13));
        assert_eq!(lib.first_change(), 11);
        assert_eq!((files[1].change, files[1].added), (Change::Added, 1));
        assert_eq!(files[1].hunks[0].lines.len(), 1);
        assert_eq!(
            (files[2].path.as_str(), files[2].change),
            ("gone.rs", Change::Deleted)
        );
        assert!(files[3].binary);
    }

    #[test]
    fn rows_index_each_file_header() {
        let changes = Changes::new(PATCH, false);
        assert_eq!(changes.file_rows.len(), 4);
        for (f, &row) in changes.file_rows.iter().enumerate() {
            assert_eq!(changes.rows[row], Row::File(f));
        }
        assert_eq!(changes.totals(), (3, 2));
    }

    #[test]
    fn renames_use_the_new_path() {
        let files = parse(
            "diff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\n",
        );
        assert_eq!(
            (files[0].path.as_str(), files[0].change),
            ("new.rs", Change::Renamed)
        );
    }
}
