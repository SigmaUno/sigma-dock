//! Opens worker files in the user's own editor; SigmaDock has no built-in editor.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Editor {
    Zed,
    Cursor,
    VsCode,
    Sublime,
    /// JetBrains IDEs, through the launcher inside the app or a Toolbox script.
    Idea,
    RustRover,
    GoLand,
    PyCharm,
    WebStorm,
    Xcode,
    /// `$VISUAL`, then `$EDITOR`, in an embedded terminal.
    Environment,
    /// `custom_command` from the preferences.
    Custom,
    /// The macOS default app for the file, without line support.
    System,
}

/// Detection order when no editor is chosen.
const KNOWN: [Editor; 10] = [
    Editor::Zed,
    Editor::Cursor,
    Editor::VsCode,
    Editor::Sublime,
    Editor::Idea,
    Editor::RustRover,
    Editor::GoLand,
    Editor::PyCharm,
    Editor::WebStorm,
    Editor::Xcode,
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorPreferences {
    /// `None` uses the first installed editor in detection order.
    pub editor: Option<Editor>,
    /// Program and arguments with `{path}`, `{line}`, `{column}` and `{worktree}` placeholders.
    pub custom_command: String,
}

/// What to open: a whole worktree, or one file at a 1-based line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub worktree: PathBuf,
    pub file: Option<(PathBuf, u32)>,
}

impl Editor {
    pub fn label(self) -> &'static str {
        match self {
            Self::Zed => "Zed",
            Self::Cursor => "Cursor",
            Self::VsCode => "VS Code",
            Self::Sublime => "Sublime Text",
            Self::Idea => "IntelliJ IDEA",
            Self::RustRover => "RustRover",
            Self::GoLand => "GoLand",
            Self::PyCharm => "PyCharm",
            Self::WebStorm => "WebStorm",
            Self::Xcode => "Xcode",
            Self::Environment => "$VISUAL / $EDITOR",
            Self::Custom => "Custom command",
            Self::System => "Default app",
        }
    }
    /// Bundle launchers used when the preferred PATH launcher is unavailable.
    fn launchers(self) -> (&'static [&'static str], &'static str) {
        match self {
            Self::Zed => (&["/Applications/Zed.app/Contents/MacOS/cli"], "zed"),
            Self::Cursor => (
                &["/Applications/Cursor.app/Contents/Resources/app/bin/cursor"],
                "cursor",
            ),
            Self::VsCode => (
                &["/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"],
                "code",
            ),
            Self::Sublime => (
                &["/Applications/Sublime Text.app/Contents/SharedSupport/bin/subl"],
                "subl",
            ),
            // Community editions share the launcher name; Toolbox scripts are found on the search path.
            Self::Idea => (
                &[
                    "/Applications/IntelliJ IDEA.app/Contents/MacOS/idea",
                    "/Applications/IntelliJ IDEA Ultimate.app/Contents/MacOS/idea",
                    "/Applications/IntelliJ IDEA CE.app/Contents/MacOS/idea",
                ],
                "idea",
            ),
            Self::RustRover => (
                &["/Applications/RustRover.app/Contents/MacOS/rustrover"],
                "rustrover",
            ),
            Self::GoLand => (
                &["/Applications/GoLand.app/Contents/MacOS/goland"],
                "goland",
            ),
            Self::PyCharm => (
                &[
                    "/Applications/PyCharm.app/Contents/MacOS/pycharm",
                    "/Applications/PyCharm Professional Edition.app/Contents/MacOS/pycharm",
                    "/Applications/PyCharm CE.app/Contents/MacOS/pycharm",
                ],
                "pycharm",
            ),
            Self::WebStorm => (
                &["/Applications/WebStorm.app/Contents/MacOS/webstorm"],
                "webstorm",
            ),
            Self::Xcode => (
                &["/Applications/Xcode.app/Contents/Developer/usr/bin/xed"],
                "xed",
            ),
            Self::Environment | Self::Custom | Self::System => (&[], ""),
        }
    }
    fn program(self) -> Option<PathBuf> {
        let (bundled, name) = self.launchers();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        search_path(name)
            .into_iter()
            .chain(
                bundled
                    .iter()
                    .map(PathBuf::from)
                    .chain(home.iter().flat_map(|home| {
                        // Per-user installs, including JetBrains Toolbox apps, land in ~/Applications.
                        bundled
                            .iter()
                            .filter_map(|path| path.strip_prefix('/'))
                            .map(|path| home.join(path))
                    })),
            )
            .find(|path| is_executable(path))
            .or_else(|| {
                let app = crate::editor_icons::app_bundle(self)?;
                bundled
                    .iter()
                    .filter_map(|path| path.split_once(".app/").map(|(_, suffix)| app.join(suffix)))
                    .find(|path| is_executable(path))
            })
    }
    pub fn installed(self) -> bool {
        match self {
            Self::Environment => environment_command().is_ok(),
            Self::Custom | Self::System => true,
            _ => self.program().is_some() || crate::editor_icons::app_bundle(self).is_some(),
        }
    }
}

fn search_path(name: &str) -> Vec<PathBuf> {
    if name.is_empty() {
        return Vec::new();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let toolbox = std::env::var_os("HOME").map(|home| {
        PathBuf::from(home).join("Library/Application Support/JetBrains/Toolbox/scripts")
    });
    std::env::split_paths(&path)
        .chain(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from))
        .chain(toolbox)
        .map(|dir| dir.join(name))
        .collect()
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Installed editors in detection order, for the settings picker and header menu.
pub fn detected() -> Vec<Editor> {
    KNOWN[..3]
        .iter()
        .copied()
        .chain([Editor::Environment])
        .chain(KNOWN[3..].iter().copied())
        .filter(|editor| editor.installed())
        .collect()
}

fn environment_command() -> Result<(PathBuf, Vec<String>)> {
    let text = environment_value(std::env::var("VISUAL").ok(), std::env::var("EDITOR").ok())?;
    parse_environment_command(&text)
}
fn environment_value(visual: Option<String>, editor: Option<String>) -> Result<String> {
    visual
        .filter(|value| !value.trim().is_empty())
        .or_else(|| editor.filter(|value| !value.trim().is_empty()))
        .context("Set VISUAL or EDITOR to a terminal editor, or choose an editor in Settings")
}
fn parse_environment_command(text: &str) -> Result<(PathBuf, Vec<String>)> {
    if text.len() > 512 || text.contains('\0') {
        bail!("Environment editor command is invalid or exceeds 512 bytes");
    }
    let mut words = split_command(text)?.into_iter();
    let program = words
        .next()
        .filter(|word| !word.is_empty())
        .context("Environment editor command is empty")?;
    let path = PathBuf::from(&program);
    let path = if path.is_absolute() {
        is_executable(&path).then_some(path)
    } else {
        search_path(&program)
            .into_iter()
            .find(|path| is_executable(path))
    }
    .with_context(|| {
        format!("Environment editor {program} is not installed; choose another editor in Settings")
    })?;
    Ok((path, words.collect()))
}

impl EditorPreferences {
    /// The editor the open actions use: the chosen one, else the first installed one.
    pub fn resolved(&self) -> Editor {
        self.editor
            .unwrap_or_else(|| detected().first().copied().unwrap_or(Editor::System))
    }
    pub fn validate(&self) -> Result<()> {
        if self.custom_command.contains('\0') {
            bail!("Editor command cannot contain NUL");
        }
        if self.custom_command.len() > 512 {
            bail!("Custom editor command is limited to 512 characters");
        }
        if self.editor == Some(Editor::Custom)
            && split_command(&self.custom_command)?
                .first()
                .is_none_or(|program| program.is_empty())
        {
            bail!("Enter a custom editor command");
        }
        Ok(())
    }
    /// Program and arguments for `target`. Arguments are passed directly, never through a shell.
    pub fn command(&self, editor: Editor, target: &Target) -> Result<(PathBuf, Vec<String>)> {
        let worktree = target.worktree.to_string_lossy().into_owned();
        let file = target.file.as_ref().map(|(path, line)| {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                target.worktree.join(path)
            };
            (path.to_string_lossy().into_owned(), (*line).max(1))
        });
        if editor == Editor::Environment {
            let (program, mut args) = environment_command()?;
            args.extend(terminal_args(&program, worktree, file));
            return Ok((program, args));
        }
        if editor == Editor::Custom {
            let (path, line) = file.clone().unwrap_or((worktree.clone(), 1));
            let mut words = split_command(&self.custom_command)?.into_iter();
            let program = words.next().context("Enter a custom editor command")?;
            let mut placed = false;
            let mut args: Vec<String> = words
                .map(|word| {
                    placed |= word.contains("{path}") || word.contains("{worktree}");
                    substitute(
                        &word,
                        &[
                            ("{path}", &path),
                            ("{line}", &line.to_string()),
                            ("{column}", "1"),
                            ("{worktree}", &worktree),
                        ],
                    )
                })
                .collect();
            if !placed {
                args.push(path);
            }
            return Ok((PathBuf::from(program), args));
        }
        if editor == Editor::System {
            let path = file.map_or(worktree, |(path, _)| path);
            return Ok(("/usr/bin/open".into(), vec![path]));
        }
        if editor.program().is_none()
            && let Some(app) = crate::editor_icons::app_bundle(editor)
        {
            let mut args = vec![
                "-a".into(),
                app.to_string_lossy().into_owned(),
                "--args".into(),
            ];
            args.extend(editor_args(editor, worktree, file));
            return Ok(("/usr/bin/open".into(), args));
        }
        let program = editor.program().with_context(|| {
            format!(
                "{} is not installed; choose another editor in Settings",
                editor.label()
            )
        })?;
        Ok((program, editor_args(editor, worktree, file)))
    }
    /// Launch the editor without waiting for it; a reaper thread collects the launcher.
    pub fn open(&self, target: &Target) -> Result<Editor> {
        let editor = self.resolved();
        let (program, args) = self.command(editor, target)?;
        let mut child = std::process::Command::new(&program)
            .args(&args)
            .current_dir(&target.worktree)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("Could not start {}", program.display()))?;
        std::thread::spawn(move || child.wait());
        Ok(editor)
    }
}

/// Substitute only tokens in the original template, never in inserted file names.
fn substitute(mut template: &str, values: &[(&str, &str)]) -> String {
    let mut result = String::new();
    while let Some(start) = template.find('{') {
        result.push_str(&template[..start]);
        template = &template[start..];
        if let Some((token, value)) = values.iter().find(|(token, _)| template.starts_with(token)) {
            result.push_str(value);
            template = &template[token.len()..];
        } else {
            result.push('{');
            template = &template[1..];
        }
    }
    result.push_str(template);
    result
}

fn terminal_args(program: &Path, worktree: String, file: Option<(String, u32)>) -> Vec<String> {
    match file {
        None => vec![worktree],
        Some((path, line))
            if matches!(
                program.file_name().and_then(|name| name.to_str()),
                Some("hx" | "helix")
            ) =>
        {
            vec![format!("{path}:{line}:1")]
        }
        Some((path, line)) => vec![format!("+{line}"), path],
    }
}

/// Arguments for a detected editor: the worktree alone, or the file at a 1-based line.
fn editor_args(editor: Editor, worktree: String, file: Option<(String, u32)>) -> Vec<String> {
    match (editor, file) {
        (_, None) => vec![worktree],
        (Editor::Zed | Editor::Sublime, Some((path, line))) => vec![format!("{path}:{line}")],
        (Editor::Cursor | Editor::VsCode, Some((path, line))) => {
            // Open the worktree as the window's folder so the file lands in context.
            vec![worktree, "-g".into(), format!("{path}:{line}")]
        }
        (Editor::Xcode, Some((path, line))) => vec!["-l".into(), line.to_string(), path],
        // The project folder first, so the file opens inside the worktree's project.
        (
            Editor::Idea | Editor::RustRover | Editor::GoLand | Editor::PyCharm | Editor::WebStorm,
            Some((path, line)),
        ) => vec![worktree, "--line".into(), line.to_string(), path],
        (Editor::Environment | Editor::Custom | Editor::System, Some(_)) => unreachable!(),
    }
}

/// Splits on whitespace, keeping single- or double-quoted runs together.
fn split_command(text: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, c) => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        bail!("Custom editor command has an unclosed quote");
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree() -> String {
        "/work/tree with space".into()
    }

    fn target(file: Option<(&str, u32)>) -> Target {
        Target {
            worktree: "/work/tree with space".into(),
            file: file.map(|(path, line)| (PathBuf::from(path), line)),
        }
    }

    #[test]
    fn visual_precedes_editor_and_empty_values_fall_back() {
        assert_eq!(
            environment_value(Some("nvim -f".into()), Some("hx".into())).unwrap(),
            "nvim -f"
        );
        assert_eq!(
            environment_value(Some("  ".into()), Some("hx".into())).unwrap(),
            "hx"
        );
        assert!(environment_value(None, Some(" ".into())).is_err());
    }

    #[test]
    fn placeholders_in_file_names_remain_literal() {
        let prefs = EditorPreferences {
            editor: Some(Editor::Custom),
            custom_command: "edit --goto {path}:{line}:{column} --project {worktree}".into(),
        };
        let (_, args) = prefs
            .command(
                Editor::Custom,
                &target(Some(("src/{line} ${worktree}; file.rs", 12))),
            )
            .unwrap();
        assert_eq!(
            args,
            vec![
                "--goto",
                "/work/tree with space/src/{line} ${worktree}; file.rs:12:1",
                "--project",
                "/work/tree with space"
            ]
        );
    }

    #[test]
    fn terminal_line_arguments_keep_paths_as_single_arguments() {
        let path = "/work/tree with space/src/$literal;file.rs".to_owned();
        assert_eq!(
            terminal_args(
                Path::new("/usr/bin/nvim"),
                worktree(),
                Some((path.clone(), 42))
            ),
            vec!["+42".to_owned(), path.clone()]
        );
        assert_eq!(
            terminal_args(
                Path::new("/opt/homebrew/bin/hx"),
                worktree(),
                Some((path.clone(), 42))
            ),
            vec![format!("{path}:42:1")]
        );
        assert_eq!(
            terminal_args(Path::new("vim"), worktree(), None),
            vec![worktree()]
        );
    }
    #[test]
    fn environment_commands_preserve_quotes_and_do_not_evaluate_shell_syntax() {
        let (program, args) =
            parse_environment_command("'/bin/sh' -f 'a b' '$HOME;touch /tmp/nope'").unwrap();
        assert_eq!(program, PathBuf::from("/bin/sh"));
        assert_eq!(args, vec!["-f", "a b", "$HOME;touch /tmp/nope"]);
        assert!(parse_environment_command("''").is_err());
        assert!(parse_environment_command("'/bin/sh").is_err());
        assert!(parse_environment_command("/absent/editor").is_err());
        assert!(parse_environment_command("/bin/sh\0").is_err());
        assert!(parse_environment_command(&"x".repeat(513)).is_err());
    }
    #[test]
    fn terminal_editor_choice_is_backwards_compatible() {
        let old: EditorPreferences =
            serde_json::from_value(serde_json::json!({"custom_command":""})).unwrap();
        assert_eq!(old.editor, None);
        let preferences = EditorPreferences {
            editor: Some(Editor::Environment),
            custom_command: String::new(),
        };
        let saved = serde_json::to_value(&preferences).unwrap();
        assert_eq!(saved["editor"], "environment");
        assert_eq!(
            serde_json::from_value::<EditorPreferences>(saved).unwrap(),
            preferences
        );
    }

    #[test]
    fn custom_commands_substitute_placeholders_without_a_shell() {
        let preferences = EditorPreferences {
            editor: Some(Editor::Custom),
            custom_command: "'/opt/my editor/bin/ed' --goto {path}:{line}:{column}".into(),
        };
        let (program, args) = preferences
            .command(Editor::Custom, &target(Some(("src/a b.rs", 42))))
            .unwrap();
        assert_eq!(program, PathBuf::from("/opt/my editor/bin/ed"));
        assert_eq!(
            args,
            vec!["--goto", "/work/tree with space/src/a b.rs:42:1"]
        );
        let (_, args) = preferences.command(Editor::Custom, &target(None)).unwrap();
        assert_eq!(args, vec!["--goto", "/work/tree with space:1:1"]);
    }

    #[test]
    fn custom_commands_without_placeholders_get_the_path_appended() {
        let preferences = EditorPreferences {
            editor: Some(Editor::Custom),
            custom_command: "myedit -w".into(),
        };
        let (_, args) = preferences
            .command(Editor::Custom, &target(Some(("x.rs", 3))))
            .unwrap();
        assert_eq!(args, vec!["-w", "/work/tree with space/x.rs"]);
    }

    #[test]
    fn invalid_custom_commands_are_rejected() {
        assert!(split_command("edit 'unclosed").is_err());
        let empty = EditorPreferences {
            editor: Some(Editor::Custom),
            custom_command: "   ".into(),
        };
        assert!(empty.validate().is_err());
        assert!(EditorPreferences::default().validate().is_ok());
    }

    #[test]
    fn jetbrains_editors_open_the_project_then_the_file_at_a_line() {
        let worktree = || "/work/tree with space".to_owned();
        let file = Some(("/work/tree with space/src/a b.rs".to_owned(), 7));
        let args = editor_args(Editor::Idea, worktree(), file);
        assert_eq!(
            args,
            vec![
                "/work/tree with space",
                "--line",
                "7",
                "/work/tree with space/src/a b.rs"
            ]
        );
        assert_eq!(
            editor_args(Editor::PyCharm, worktree(), None),
            vec!["/work/tree with space"]
        );
        assert_eq!(
            serde_json::to_value(Editor::RustRover).unwrap(),
            "rust_rover"
        );
    }

    #[test]
    fn system_editor_opens_the_file_without_a_line() {
        let (program, args) = EditorPreferences::default()
            .command(Editor::System, &target(Some(("/abs/file.rs", 9))))
            .unwrap();
        assert_eq!(program, PathBuf::from("/usr/bin/open"));
        assert_eq!(args, vec!["/abs/file.rs"]);
    }

    #[test]
    fn preferences_round_trip_and_default_to_detection() {
        let preferences = EditorPreferences {
            editor: Some(Editor::VsCode),
            custom_command: String::new(),
        };
        let json = serde_json::to_value(&preferences).unwrap();
        assert_eq!(json["editor"], "vs_code");
        assert_eq!(
            serde_json::from_value::<EditorPreferences>(json).unwrap(),
            preferences
        );
        assert_eq!(
            serde_json::from_value::<EditorPreferences>(serde_json::json!({})).unwrap(),
            EditorPreferences::default()
        );
    }
}
