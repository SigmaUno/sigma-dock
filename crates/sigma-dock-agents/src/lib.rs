//! Thin argument adapters. No shell interpolation and no permission bypass flags.
use anyhow::{Result, bail};

#[derive(Debug, Clone)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}
pub trait Executor {
    fn command(&self, prompt: Option<&str>, resume: bool) -> Result<Command>;
}
pub struct Harness(pub String);
impl Executor for Harness {
    fn command(&self, prompt: Option<&str>, resume: bool) -> Result<Command> {
        let mut args = Vec::new();
        let mut env = vec![
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
        ];
        match self.0.as_str() {
            "claude" => {
                if resume {
                    args.push("--continue".into());
                }
                env.extend([
                    ("DISABLE_TELEMETRY".into(), "1".into()),
                    ("DISABLE_ERROR_REPORTING".into(), "1".into()),
                    (
                        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
                        "1".into(),
                    ),
                ]);
            }
            "codex" => {
                if resume {
                    if prompt.is_some() {
                        bail!(
                            "Codex continuation with a prompt is ambiguous; resume first, then use sdk message"
                        );
                    }
                    args.extend(["resume".into(), "--last".into()]);
                }
            }
            "gemini" => {
                if resume {
                    args.extend(["--resume".into(), "latest".into()]);
                }
                if let Some(prompt) = prompt {
                    args.push(format!("--prompt-interactive={prompt}"));
                }
            }
            "opencode" => {
                if resume {
                    args.push("--continue".into());
                }
                if let Some(prompt) = prompt {
                    args.push(format!("--prompt={prompt}"));
                }
            }
            "aider" => {
                env.push(("AIDER_ANALYTICS".into(), "false".into()));
                if let Some(prompt) = prompt {
                    args.push(format!("--message={prompt}"));
                }
                if resume {
                    bail!("aider resume is not supported");
                }
            }
            "shell" => {
                if prompt.is_some() || resume {
                    bail!("shell does not accept a prompt or resume flag");
                }
            }
            _ => bail!(
                "unknown harness {}; use claude, codex, gemini, opencode, aider, or shell",
                self.0
            ),
        }
        if let Some(prompt) = prompt
            && ["claude", "codex"].contains(&self.0.as_str())
        {
            args.extend(["--".into(), prompt.to_owned()]);
        }
        let program = if self.0 == "shell" {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
        } else {
            self.0.clone()
        };
        Ok(Command { program, args, env })
    }
}

/// Build per-session MCP configuration without editing a harness's global settings.
pub struct McpLaunch<'a> {
    pub binary: &'a std::path::Path,
    pub config_path: &'a std::path::Path,
    pub project_id: &'a str,
    pub socket: &'a std::path::Path,
    pub allow_spawn: bool,
}
pub fn orchestrator_command(
    harness: &str,
    prompt: Option<&str>,
    resume: bool,
    launch: &McpLaunch<'_>,
) -> Result<(Command, Option<String>)> {
    let McpLaunch {
        binary: mcp_binary,
        config_path,
        project_id,
        socket,
        allow_spawn,
    } = *launch;
    if !["claude", "codex"].contains(&harness) {
        bail!("managed orchestrators currently support claude or codex");
    }
    let mut command = Harness(harness.into()).command(prompt, resume)?;
    let mut args = vec![
        "--project-id".to_owned(),
        project_id.into(),
        "--socket".into(),
        socket.to_string_lossy().into_owned(),
    ];
    if allow_spawn {
        args.push("--allow-spawn".into());
    }
    let (extra, config) = if harness == "claude" {
        (
            vec![
                "--mcp-config".into(),
                config_path.to_string_lossy().into_owned(),
            ],
            Some(serde_json::to_string_pretty(
                &serde_json::json!({"mcpServers":{"sigma":{"type":"stdio","command":mcp_binary,"args":args}}}),
            )?),
        )
    } else {
        (
            vec![
                "-c".into(),
                format!(
                    "mcp_servers.sigma.command={}",
                    serde_json::to_string(&mcp_binary)?
                ),
                "-c".into(),
                format!("mcp_servers.sigma.args={}", serde_json::to_string(&args)?),
            ],
            None,
        )
    };
    let at = command
        .args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(command.args.len());
    command.args.splice(at..at, extra);
    Ok((command, config))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompts_cannot_become_options() {
        let prompt = "--dangerously-skip-permissions";
        for name in ["claude", "codex"] {
            assert_eq!(
                Harness(name.into())
                    .command(Some(prompt), false)
                    .unwrap()
                    .args,
                vec!["--", prompt]
            );
        }
        assert_eq!(
            Harness("gemini".into())
                .command(Some(prompt), false)
                .unwrap()
                .args,
            vec![format!("--prompt-interactive={prompt}")]
        );
    }
    #[test]
    fn resume_rejects_ambiguous_arguments() {
        assert!(Harness("codex".into()).command(Some("task"), true).is_err());
        assert_eq!(
            Harness("codex".into()).command(None, true).unwrap().args,
            vec!["resume", "--last"]
        );
        assert!(
            Harness("shell".into())
                .command(Some("task"), false)
                .is_err()
        );
    }
    #[test]
    fn orchestrators_keep_mcp_configuration_local_and_scoped() {
        use std::path::Path;
        let launch = McpLaunch {
            binary: Path::new("/app path/sigma-dock-mcp"),
            config_path: Path::new("/state/worker.json"),
            project_id: "p",
            socket: Path::new("/state/daemon.sock"),
            allow_spawn: false,
        };
        let (claude, config) =
            orchestrator_command("claude", Some("plan"), false, &launch).unwrap();
        assert!(
            claude
                .args
                .windows(2)
                .any(|pair| pair == ["--mcp-config", "/state/worker.json"])
        );
        let config: serde_json::Value = serde_json::from_str(&config.unwrap()).unwrap();
        assert_eq!(
            config["mcpServers"]["sigma"]["command"],
            "/app path/sigma-dock-mcp"
        );
        assert!(
            !config["mcpServers"]["sigma"]["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg == "--allow-spawn")
        );
        let (codex, config) = orchestrator_command("codex", Some("plan"), false, &launch).unwrap();
        assert!(config.is_none());
        assert!(
            codex
                .args
                .iter()
                .any(|arg| arg.starts_with("mcp_servers.sigma.command="))
        );
        assert_eq!(&codex.args[codex.args.len() - 2..], &["--", "plan"]);
        assert!(orchestrator_command("shell", None, false, &launch).is_err());
    }
}
