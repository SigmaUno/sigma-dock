//! Opens worker files in the user's own editor; SigmaDock has no built-in editor.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Editor {
    Zed,
    Cursor,
    VsCode,
    Sublime,
    Xcode,
    /// `custom_command` from the preferences.
    Custom,
    /// The macOS default app for the file, without line support.
    System,
}

/// Detection order when no editor is chosen.
const KNOWN: [Editor; 5] = [
    Editor::Zed,
    Editor::Cursor,
    Editor::VsCode,
    Editor::Sublime,
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
            Self::Xcode => "Xcode",
            Self::Custom => "Custom command",
            Self::System => "Default app",
        }
    }
    /// Command-line launchers inside the app bundle, then the name looked up on `PATH`.
    /// GUI apps on macOS get a minimal `PATH`, so the bundle paths come first.
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
            Self::Xcode => (
                &["/Applications/Xcode.app/Contents/Developer/usr/bin/xed"],
                "xed",
            ),
            Self::Custom | Self::System => (&[], ""),
        }
    }
    fn program(self) -> Option<PathBuf> {
        let (bundled, name) = self.launchers();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        bundled
            .iter()
            .map(PathBuf::from)
            .chain(home.iter().filter_map(|home| {
                // Per-user installs land in ~/Applications.
                bundled
                    .first()
                    .and_then(|path| path.strip_prefix('/'))
                    .map(|path| home.join(path))
            }))
            .chain(search_path(name))
            .find(|path| is_executable(path))
    }
    pub fn installed(self) -> bool {
        match self {
            Self::Custom | Self::System => true,
            _ => self.program().is_some(),
        }
    }
}

fn search_path(name: &str) -> Vec<PathBuf> {
    if name.is_empty() {
        return Vec::new();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from))
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
    KNOWN
        .into_iter()
        .filter(|editor| editor.installed())
        .collect()
}

impl EditorPreferences {
    /// The editor the open actions use: the chosen one, else the first installed one.
    pub fn resolved(&self) -> Editor {
        self.editor
            .unwrap_or_else(|| detected().first().copied().unwrap_or(Editor::System))
    }
    pub fn validate(&self) -> Result<()> {
        if self.custom_command.len() > 512 {
            bail!("Custom editor command is limited to 512 characters");
        }
        if self.editor == Some(Editor::Custom) && split_command(&self.custom_command)?.is_empty() {
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
        if editor == Editor::Custom {
            let (path, line) = file.clone().unwrap_or((worktree.clone(), 1));
            let mut words = split_command(&self.custom_command)?.into_iter();
            let program = words.next().context("Enter a custom editor command")?;
            let mut placed = false;
            let mut args: Vec<String> = words
                .map(|word| {
                    placed |= word.contains("{path}") || word.contains("{worktree}");
                    word.replace("{path}", &path)
                        .replace("{line}", &line.to_string())
                        .replace("{column}", "1")
                        .replace("{worktree}", &worktree)
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
        let program = editor.program().with_context(|| {
            format!(
                "{} is not installed; choose another editor in Settings",
                editor.label()
            )
        })?;
        let args = match (editor, file) {
            (_, None) => vec![worktree],
            (Editor::Zed | Editor::Sublime, Some((path, line))) => vec![format!("{path}:{line}")],
            (Editor::Cursor | Editor::VsCode, Some((path, line))) => {
                // Open the worktree as the window's folder so the file lands in context.
                vec![worktree, "-g".into(), format!("{path}:{line}")]
            }
            (Editor::Xcode, Some((path, line))) => vec!["-l".into(), line.to_string(), path],
            (Editor::Custom | Editor::System, Some(_)) => unreachable!(),
        };
        Ok((program, args))
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

    fn target(file: Option<(&str, u32)>) -> Target {
        Target {
            worktree: "/work/tree with space".into(),
            file: file.map(|(path, line)| (PathBuf::from(path), line)),
        }
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
