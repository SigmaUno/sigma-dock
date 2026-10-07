//! Bounded, read-only diff snapshots shared by the daemon, CLI and UI.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffSectionKind {
    Committed,
    Uncommitted,
    Untracked,
}
impl DiffSectionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Committed => "Committed",
            Self::Uncommitted => "Uncommitted (index and working tree)",
            Self::Untracked => "Untracked",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Uncommitted => "uncommitted",
            Self::Untracked => "untracked",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffFile {
    pub path: String,
    pub status: String,
    pub added: Option<u64>,
    pub removed: Option<u64>,
    pub binary: bool,
    /// Content and diff identity; includes both blob identities and mode/context changes.
    pub blob_id: String,
    pub patch: String,
    pub truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSection {
    pub kind: DiffSectionKind,
    pub files: Vec<DiffFile>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffReport {
    pub base_ref: String,
    pub merge_base: String,
    pub head: String,
    pub sections: Vec<DiffSection>,
    pub warnings: Vec<String>,
    pub truncated: bool,
}
impl DiffReport {
    pub fn text(&self, stat: bool) -> String {
        let mut text = format!(
            "Diff against merge base {} of {}\n",
            self.merge_base, self.base_ref
        );
        for section in &self.sections {
            if section.files.is_empty() {
                continue;
            }
            text.push_str(&format!("\n{} changes\n", section.kind.label()));
            for file in &section.files {
                if stat {
                    text.push_str(&format!(
                        "{} {} | {}\n",
                        file.status,
                        file.path,
                        if file.binary {
                            "binary".into()
                        } else {
                            format!(
                                "+{} -{}",
                                file.added.unwrap_or(0),
                                file.removed.unwrap_or(0)
                            )
                        }
                    ));
                } else {
                    text.push_str(&file.patch);
                }
                if file.truncated {
                    text.push_str(
                        "\n[File diff truncated; open it in your editor for the remainder.]\n",
                    );
                }
            }
        }
        for warning in &self.warnings {
            text.push_str(&format!("\nNote: {warning}\n"));
        }
        text
    }
}
