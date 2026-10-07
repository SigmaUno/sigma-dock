//! Repository-owned commands and persisted lifecycle state. Parsing never executes code.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Read, path::Path};

pub const CONFIG_LIMIT: usize = 64 * 1024;
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub scripts: Scripts,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scripts {
    pub setup: Option<String>,
    pub archive: Option<String>,
    pub run_mode: RunMode,
    pub run: BTreeMap<String, RunScript>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunScript {
    pub command: String,
    #[serde(default)]
    pub default: bool,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    #[default]
    Concurrent,
    Nonconcurrent,
}
#[derive(Debug, Clone)]
pub struct Document {
    pub text: String,
    pub hash: String,
    pub config: Config,
}
impl Document {
    pub fn parse(text: String) -> Result<Self> {
        if text.len() > CONFIG_LIMIT {
            bail!(".sigmadock.toml exceeds 64 KiB");
        }
        let config: Config = toml::from_str(&text)
            .map_err(|error| anyhow::anyhow!("invalid .sigmadock.toml: {error}"))?;
        let command = |value: &str| -> Result<()> {
            if value.trim().is_empty() || value.contains('\0') {
                bail!("script commands must be nonempty and contain no NUL bytes");
            }
            Ok(())
        };
        for value in [&config.scripts.setup, &config.scripts.archive]
            .into_iter()
            .flatten()
        {
            command(value)?;
        }
        if config.scripts.run.len() > 16 {
            bail!("at most 16 run scripts are supported");
        }
        for (name, run) in &config.scripts.run {
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            {
                bail!("run script names must be 1..64 ASCII letters, digits, '-' or '_'");
            }
            command(&run.command).with_context(|| format!("run script {name}"))?;
        }
        if config
            .scripts
            .run
            .values()
            .filter(|run| run.default)
            .count()
            > 1
        {
            bail!("only one run script may be default");
        }
        let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
        Ok(Self { text, hash, config })
    }
    pub fn read(worktree: &Path) -> Result<Option<Self>> {
        let path = worktree.join(".sigmadock.toml");
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if !meta.is_file() || meta.file_type().is_symlink() {
            bail!(".sigmadock.toml must be a regular file");
        }
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(CONFIG_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        let text = String::from_utf8(bytes).context(".sigmadock.toml must be UTF-8")?;
        Self::parse(text).map(Some)
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Ready,
    AwaitingApproval,
    SettingUp,
    SetupFailed,
    Archiving,
    ArchiveFailed,
}
impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::AwaitingApproval => "Approve repository scripts",
            Self::SettingUp => "Setting up",
            Self::SetupFailed => "Setup failed",
            Self::Archiving => "Archiving",
            Self::ArchiveFailed => "Archive hook failed",
        }
    }
    pub fn reserves_berth(self) -> bool {
        self != Self::Ready
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceScripts {
    pub phase: Phase,
    pub hash: Option<String>,
    pub error: Option<String>,
    pub archive_requested: bool,
    pub cleanup: bool,
    pub force: bool,
    pub available_runs: Vec<String>,
    pub pending_run: Option<String>,
    pub runs: BTreeMap<String, ScriptStatus>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScriptStatus {
    pub running: bool,
    pub exit_code: Option<u32>,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_validation_and_content_hash() {
        let text = "[scripts]\nsetup='echo setup'\nrun_mode='nonconcurrent'\n[scripts.run.web]\ncommand='echo $PORT'\ndefault=true\n";
        let parsed = Document::parse(text.into()).unwrap();
        assert_eq!(parsed.config.scripts.run_mode, RunMode::Nonconcurrent);
        assert_ne!(
            parsed.hash,
            Document::parse(format!("{text}# change\n")).unwrap().hash
        );
        for invalid in [
            "[scripts]\nsetpu='typo'",
            "[scripts]\nsetup=''",
            "[scripts]\nrun_mode='invalid'",
            "[scripts.run.'../bad']\ncommand='x'",
            "[scripts.run.a]\ncommand='a'\ndefault=true\n[scripts.run.b]\ncommand='b'\ndefault=true",
        ] {
            assert!(Document::parse(invalid.into()).is_err(), "{invalid}");
        }
        assert!(Document::parse("x".repeat(CONFIG_LIMIT + 1)).is_err());
    }
}
