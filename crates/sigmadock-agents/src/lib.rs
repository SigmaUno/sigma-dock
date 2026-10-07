//! Thin argument adapters. No shell interpolation and no permission bypass flags.
pub mod usage;
use anyhow::{Context, Result, bail};

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
            binary: Path::new("/app path/sigmadock-mcp"),
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
            "/app path/sigmadock-mcp"
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

/// Session-local opt-in; preserve global/user settings files and quote executable paths for the shell.
pub fn claude_usage_settings(command: &mut Command, sdk: &std::path::Path) -> Result<()> {
    let path = sdk
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("usage helper path must be UTF-8"))?;
    if !sdk.is_file() {
        bail!("install sdk beside the daemon to enable Claude usage reporting");
    }
    let invocation = format!("'{}' usage-report", path.replace("'", "'\\''"));
    let settings = serde_json::to_string(
        &serde_json::json!({"statusLine":{"type":"command","command":invocation,"padding":0}}),
    )?;
    let at = command
        .args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(command.args.len());
    command.args.splice(at..at, ["--settings".into(), settings]);
    Ok(())
}

#[cfg(all(test, unix))]
mod usage_launch_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn status_line_quotes_shell_metacharacters_and_preserves_prompt_and_resume_flags() {
        let root = std::env::temp_dir().join(format!("sigma-usage-quote-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let sdk = root.join("sdk ' $HOME `uname` $(uname)");
        std::fs::write(&sdk, "#!/bin/sh\nprintf '%s' \"$1\"\n").unwrap();
        std::fs::set_permissions(&sdk, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = Harness("claude".into())
            .command(Some("prompt"), true)
            .unwrap();
        claude_usage_settings(&mut command, &sdk).unwrap();
        assert!(command.args.contains(&"--continue".to_string()));
        assert_eq!(command.args.last().unwrap(), "prompt");
        let index = command
            .args
            .iter()
            .position(|arg| arg == "--settings")
            .unwrap();
        let settings: serde_json::Value = serde_json::from_str(&command.args[index + 1]).unwrap();
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(settings["statusLine"]["command"].as_str().unwrap())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"usage-report");
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Install observation-only hooks in the command's session settings, never on disk globally.
pub fn claude_attention_settings(command: &mut Command, sdk: &std::path::Path) -> Result<()> {
    if !sdk.is_file() {
        bail!("install sdk beside the daemon to enable Claude attention reporting");
    }
    let path = sdk
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("attention helper path must be UTF-8"))?;
    let invocation = format!("'{}' attention-report", path.replace("'", "'\\''"));
    let hook = serde_json::json!({"type":"command", "command":invocation, "timeout":2});
    let hooks = serde_json::json!({
        "PreToolUse":[{"matcher":"^(AskUserQuestion|ExitPlanMode)$", "hooks":[hook.clone()]}],
        "PermissionRequest":[{"hooks":[hook.clone()]}],
        "PostToolUse":[{"hooks":[hook.clone()]}],
        "PostToolUseFailure":[{"hooks":[hook.clone()]}],
        "UserPromptSubmit":[{"hooks":[hook]}]
    });
    // Usage reporting already supplied a statusLine: merge rather than pass a second --settings.
    if let Some(at) = command.args.iter().position(|arg| arg == "--settings") {
        let mut settings: serde_json::Value =
            serde_json::from_str(command.args.get(at + 1).context("missing settings value")?)?;
        settings
            .as_object_mut()
            .context("settings must be an object")?
            .insert("hooks".into(), hooks);
        command.args[at + 1] = serde_json::to_string(&settings)?;
    } else {
        let at = command
            .args
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(command.args.len());
        command.args.splice(
            at..at,
            [
                "--settings".into(),
                serde_json::json!({"hooks":hooks}).to_string(),
            ],
        );
    }
    Ok(())
}

#[cfg(test)]
mod attention_tests {
    use super::*;
    #[test]
    fn hooks_merge_with_usage_settings_and_never_change_permission_policy() {
        let root = std::env::temp_dir().join(format!("sigmadock-hook-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let sdk = root.join("sdk ' $(touch nope)");
        std::fs::write(&sdk, "fixture").unwrap();
        let mut command = Harness("claude".into())
            .command(Some("task"), true)
            .unwrap();
        claude_usage_settings(&mut command, &sdk).unwrap();
        claude_attention_settings(&mut command, &sdk).unwrap();
        assert_eq!(
            command.args.iter().filter(|s| *s == "--settings").count(),
            1
        );
        assert_eq!(&command.args[command.args.len() - 2..], &["--", "task"]);
        assert!(command.args.contains(&"--continue".into()));
        let at = command.args.iter().position(|s| s == "--settings").unwrap();
        let settings: serde_json::Value = serde_json::from_str(&command.args[at + 1]).unwrap();
        assert!(settings.get("statusLine").is_some());
        assert!(settings.get("permissions").is_none());
        let hook = &settings["hooks"]["PermissionRequest"][0]["hooks"][0];
        assert_eq!(hook["timeout"], 2);
        assert!(hook["command"].as_str().unwrap().contains("'\\''"));
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["matcher"],
            "^(AskUserQuestion|ExitPlanMode)$"
        );
        assert_eq!(settings["hooks"].as_object().unwrap().len(), 5);
        std::fs::remove_dir_all(root).unwrap();
    }
}
