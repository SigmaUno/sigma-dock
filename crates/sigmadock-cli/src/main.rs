use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use sigmadock_core::{Client, Output, socket_path, status};
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
#[derive(Parser)]
#[command(name = "sdk", about = "SigmaDock workspace CLI")]
struct Args {
    #[arg(long, env = "SIGMA_DOCK_SOCKET", default_value_os_t = socket_path())]
    socket: PathBuf,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Internal, read-only observation of Claude hook events.
    #[command(hide = true)]
    AttentionReport,
    Capacity,
    MaxWorkers {
        value: usize,
    },
    RemoveProject {
        project_id: String,
    },
    Queue {
        #[command(subcommand)]
        command: Option<QueueCommand>,
    },
    Ping,
    Orchestrator {
        project_id: String,
        #[arg(long, default_value = "claude")]
        agent: String,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        allow_spawn: bool,
    },
    Notes {
        project_id: String,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        expected_revision: Option<u64>,
    },
    Ci {
        worker_id: String,
        #[arg(long)]
        send: bool,
    },
    AutoCi {
        worker_id: String,
        #[arg(long)]
        enable: bool,
    },
    Conflict {
        worker_id: String,
        #[arg(long)]
        send: bool,
    },
    Project {
        path: PathBuf,
    },
    /// Set the origin branch used by new workers.
    ProjectBase {
        project_id: String,
        branch: String,
    },
    Projects,
    Spawn {
        project_id: String,
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "claude")]
        agent: String,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        queue: bool,
    },
    Ls {
        #[arg(long)]
        archived: bool,
        #[arg(long)]
        json: bool,
    },
    Status {
        worker_id: String,
    },
    Attach {
        worker_id: String,
        /// setup, archive, or run:NAME; omit for the current lifecycle/agent terminal.
        #[arg(long)]
        script: Option<String>,
    },
    /// Review repository commands, or approve the exact hash returned by this command.
    Scripts {
        worker_id: String,
        #[arg(long)]
        approve: Option<String>,
    },
    /// Retry setup, or skip it and start the agent. Skipping executes no repository hook.
    Setup {
        worker_id: String,
        #[arg(long)]
        skip: bool,
        #[arg(long)]
        acknowledge_unknown: bool,
    },
    /// Start a named/default run script or stop one/all runs in this worker.
    Run {
        worker_id: String,
        name: Option<String>,
        #[arg(long)]
        stop: bool,
    },
    Message {
        worker_id: String,
        message: String,
    },
    Usage {
        worker_id: String,
    },
    UsageReporting {
        worker_id: String,
        #[arg(long)]
        enable: bool,
    },
    /// Receive allowlisted Claude status-line metrics; never stores raw input.
    UsageReport {
        #[arg(long, env = "SIGMA_DOCK_WORKER_ID")]
        worker_id: String,
    },
    CiPreview {
        worker_id: String,
    },
    Unfinished,
    Context {
        worker_id: String,
        #[arg(long)]
        clear: bool,
    },
    Resume {
        worker_id: String,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long = "continue")]
        continue_session: bool,
        /// Confirm you verified an interrupted session's old process has stopped.
        #[arg(long)]
        acknowledge_unknown: bool,
    },
    Stop {
        worker_id: String,
    },
    Archive {
        worker_id: String,
        #[arg(long)]
        cleanup: bool,
        /// Continue after a failing archive hook; never bypasses approval or dirty-worktree protection.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        acknowledge_unknown: bool,
    },
    Diff {
        worker_id: String,
        /// Show file counts instead of unified hunks.
        #[arg(long)]
        stat: bool,
    },
    /// Print a Markdown session summary from recorded facts and git history.
    Summary {
        worker_id: String,
    },
    Prune {
        project_id: String,
    },
    Forge {
        worker_id: String,
        #[arg(long, default_value = "github")]
        kind: String,
        #[arg(long, default_value = "https://api.github.com")]
        api_url: String,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "GITHUB_TOKEN")]
        token_env: String,
        /// Read the token from `gh auth token` instead of `--token-env` (GitHub only).
        #[arg(long)]
        github_cli: bool,
        #[arg(long)]
        actions: bool,
    },
    Refresh {
        worker_id: String,
    },
    Review {
        worker_id: String,
        #[arg(long)]
        send: bool,
    },
}
#[derive(Subcommand)]
enum QueueCommand {
    Cancel {
        id: String,
    },
    Retry {
        id: String,
        #[arg(long)]
        acknowledge_unknown: bool,
    },
}
fn main() -> Result<()> {
    let args = Args::parse();
    let client = Client {
        socket: args.socket,
    };
    if matches!(&args.command, Commands::AttentionReport) {
        // Hooks never return a permission decision or fail/block the agent.
        let _ = attention_report(&client);
        return Ok(());
    }
    client.check_version()?;
    let (method, params) = match args.command {
        Commands::AttentionReport => unreachable!(),
        Commands::Capacity => ("capacity", json!({})),
        Commands::MaxWorkers { value } => ("set_max_workers", json!({"max_workers":value})),
        Commands::RemoveProject { project_id } => {
            ("remove_project", json!({"project_id":project_id}))
        }
        Commands::Queue { command } => match command {
            None => {
                let mut tasks = Vec::<Value>::new();
                loop {
                    let page: Vec<Value> = serde_json::from_value(
                        client.call("list_queue", json!({"offset":tasks.len(),"limit":100}))?,
                    )?;
                    let finished = page.len() < 100;
                    tasks.extend(page);
                    if finished {
                        break;
                    }
                }
                println!("{}", serde_json::to_string_pretty(&tasks)?);
                return Ok(());
            }
            Some(QueueCommand::Cancel { id }) => ("cancel_queued", json!({"id":id})),
            Some(QueueCommand::Retry {
                id,
                acknowledge_unknown,
            }) => (
                "retry_queued",
                json!({"id":id,"acknowledge_unknown":acknowledge_unknown}),
            ),
        },
        Commands::Usage { worker_id } => ("agent_usage", json!({"worker_id":worker_id})),
        Commands::UsageReporting { worker_id, enable } => (
            "configure_usage",
            json!({"worker_id":worker_id,"enabled":enable}),
        ),
        Commands::UsageReport { worker_id } => {
            let mut bytes = Vec::new();
            std::io::stdin().take(65537).read_to_end(&mut bytes)?;
            anyhow::ensure!(bytes.len() <= 65536, "Status line payload exceeds 64 KiB");
            let raw: Value = serde_json::from_slice(&bytes)?;
            let usage = sigmadock_agents::usage::claude_status(&raw);
            client.call(
                "report_usage",
                json!({"worker_id":worker_id,"report":usage}),
            )?;
            println!(
                "{}",
                usage
                    .windows
                    .iter()
                    .map(|window| format!("{}: {:.1}% used", window.name, window.used_percent))
                    .collect::<Vec<_>>()
                    .join(" · ")
            );
            return Ok(());
        }
        Commands::Ping => ("ping", json!({})),
        Commands::Orchestrator {
            project_id,
            agent,
            prompt,
            allow_spawn,
        } => (
            "start_orchestrator",
            json!({"project_id":project_id,"agent":agent,"prompt":prompt,"allow_spawn":allow_spawn}),
        ),
        Commands::Notes {
            project_id,
            text,
            expected_revision,
        } => {
            if let Some(text) = text {
                (
                    "write_planning_notes",
                    json!({"project_id":project_id,"text":text,"expected_revision":expected_revision.context("--expected-revision is required when writing notes")?}),
                )
            } else {
                ("read_planning_notes", json!({"project_id":project_id}))
            }
        }
        Commands::Ci { worker_id, send } => (
            if send {
                "send_ci_feedback"
            } else {
                "ci_feedback"
            },
            json!({"worker_id":worker_id}),
        ),
        Commands::AutoCi { worker_id, enable } => (
            "configure_feedback",
            json!({"worker_id":worker_id,"auto_ci":enable}),
        ),
        Commands::Conflict { worker_id, send } => {
            let instruction =
                client.call("conflict_instruction", json!({"worker_id":worker_id}))?;
            println!(
                "{}",
                instruction
                    .as_str()
                    .context("invalid conflict instruction")?
            );
            if send {
                client.call(
                    "message_worker",
                    json!({"worker_id":worker_id,"message":instruction}),
                )?;
            }
            return Ok(());
        }
        Commands::Project { path } => ("add_project", json!({"path":path.canonicalize()?})),
        Commands::ProjectBase { project_id, branch } => (
            "configure_project",
            json!({"project_id":project_id,"base_branch":branch}),
        ),
        Commands::Projects => ("list_projects", json!({})),
        Commands::Spawn {
            project_id,
            title,
            agent,
            prompt,
            base,
            queue,
        } => (
            "spawn_worker",
            json!({"project_id":project_id,"title":title,"agent":agent,"prompt":prompt,"base":base,"queue":queue}),
        ),
        Commands::Ls {
            archived,
            json: as_json,
        } => {
            let result = client.call("list_workers", json!({"include_archived":archived}))?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                let workers: Vec<sigmadock_core::Worker> = serde_json::from_value(result)?;
                println!("{:<36}  {:<15}  {:<10}  TASK", "ID", "STATUS", "AGENT");
                for worker in workers {
                    println!(
                        "{}  {:<15}  {:<10}  {}",
                        worker.id,
                        status(&worker.facts).label(),
                        worker.agent,
                        worker.title
                    );
                }
            }
            return Ok(());
        }
        Commands::Status { worker_id } => ("get_worker_status", json!({"worker_id":worker_id})),
        Commands::Attach { worker_id, script } => return attach(client, worker_id, script),
        Commands::Scripts { worker_id, approve } => {
            if let Some(hash) = approve {
                (
                    "approve_scripts",
                    json!({"worker_id":worker_id,"hash":hash}),
                )
            } else {
                ("workspace_scripts", json!({"worker_id":worker_id}))
            }
        }
        Commands::Setup {
            worker_id,
            skip,
            acknowledge_unknown,
        } => (
            "setup_worker",
            json!({"worker_id":worker_id,"skip":skip,"acknowledge_unknown":acknowledge_unknown}),
        ),
        Commands::Run {
            worker_id,
            name,
            stop,
        } => (
            "run_script",
            json!({"worker_id":worker_id,"name":name,"stop":stop}),
        ),
        Commands::Message { worker_id, message } => (
            "message_worker",
            json!({"worker_id":worker_id,"message":message}),
        ),
        Commands::CiPreview { worker_id } => ("ci_preview", json!({"worker_id":worker_id})),
        Commands::Unfinished => ("list_unfinished", json!({})),
        Commands::Context { worker_id, clear } => (
            if clear {
                "clear_session_context"
            } else {
                "session_context"
            },
            json!({"worker_id":worker_id}),
        ),
        Commands::Resume {
            worker_id,
            prompt,
            continue_session,
            acknowledge_unknown,
        } => (
            "resume_worker",
            json!({"worker_id":worker_id,"prompt":prompt,"continue":continue_session,"acknowledge_unknown":acknowledge_unknown}),
        ),
        Commands::Stop { worker_id } => ("stop_worker", json!({"worker_id":worker_id})),
        Commands::Archive {
            worker_id,
            cleanup,
            force,
            acknowledge_unknown,
        } => return archive(client, worker_id, cleanup, force, acknowledge_unknown),
        Commands::Diff { worker_id, stat } => ("diff", json!({"worker_id":worker_id,"stat":stat})),
        Commands::Summary { worker_id } => ("session_summary", json!({"worker_id":worker_id})),
        Commands::Prune { project_id } => ("prune", json!({"project_id":project_id})),
        Commands::Forge {
            worker_id,
            kind,
            api_url,
            owner,
            repo,
            token_env,
            github_cli,
            actions,
        } => (
            "configure_forge",
            json!({"worker_id":worker_id,"forge":{"kind":kind,"api_url":api_url,"owner":owner,"repo":repo,"token_env":token_env,"actions":actions,"token":if github_cli { "github_cli" } else { "env" }}}),
        ),
        Commands::Refresh { worker_id } => ("refresh_facts", json!({"worker_id":worker_id})),
        Commands::Review { worker_id, send } => {
            let text = client.call("review_feedback", json!({"worker_id":worker_id}))?;
            let text = text.as_str().context("invalid feedback")?;
            println!("{text}");
            if send {
                client.call(
                    "message_worker",
                    json!({"worker_id":worker_id,"message":text}),
                )?;
            }
            return Ok(());
        }
    };
    let result = client.call(method, params)?;
    if let Value::String(text) = result {
        println!("{text}");
    } else {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }
    Ok(())
}
struct RawGuard;
impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = std::io::stdout().write_all(b"\x1b[?25h\x1b[?1049l");
    }
}
fn attach(client: Client, worker_id: String, script: Option<String>) -> Result<()> {
    client.call("get_worker_status", json!({"worker_id":worker_id}))?;
    eprintln!("Attached. Press Ctrl-] to detach; the agent keeps running.");
    crossterm::terminal::enable_raw_mode()?;
    let _guard = RawGuard;
    let running = Arc::new(AtomicBool::new(true));
    let input_client = client.clone();
    let input_worker = worker_id.clone();
    let input_running = running.clone();
    let input_script = script.clone();
    thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut bytes = [0; 4096];
        while input_running.load(Ordering::Relaxed) {
            let size = match stdin.read(&mut bytes) {
                Ok(0) | Err(_) => break,
                Ok(size) => size,
            };
            if bytes[..size].contains(&0x1d) {
                break;
            }
            if input_client
                .call(
                    "input",
                    json!({"worker_id":input_worker,"script":input_script,"bytes":&bytes[..size]}),
                )
                .is_err()
            {
                break;
            }
        }
        input_running.store(false, Ordering::Relaxed);
    });
    let mut cursor = 0;
    let mut generation = None;
    let mut size = (0, 0);
    let mut stdout = std::io::stdout();
    let result = (|| -> Result<()> {
        while running.load(Ordering::Relaxed) {
            if let Ok(new_size) = crossterm::terminal::size()
                && new_size != size
                && client.call("resize", json!({"worker_id":worker_id,"script":script,"cols":new_size.0,"rows":new_size.1})).is_ok()
            { size = new_size; }
            let value = client.call(
                "output",
                json!({"worker_id":worker_id,"script":script,"cursor":cursor}),
            )?;
            let current = value["generation"].as_u64();
            if generation.is_some() && current != generation {
                generation = current;
                cursor = 0;
                stdout.write_all(b"\x1bc")?;
                continue;
            }
            generation = current;
            let output: Output = serde_json::from_value(value)?;
            if output.truncated {
                stdout.write_all(b"\x1bc")?;
            }
            stdout.write_all(&output.bytes)?;
            stdout.flush()?;
            cursor = output.cursor;
            if output.exited {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        Ok(())
    })();
    running.store(false, Ordering::Relaxed);
    result
}

fn archive(
    client: Client,
    worker: String,
    cleanup: bool,
    force: bool,
    acknowledge_unknown: bool,
) -> Result<()> {
    client.call("archive_worker", json!({"worker_id":worker,"cleanup":cleanup,"force":force,"acknowledge_unknown":acknowledge_unknown}))?;
    let mut cursor = 0;
    loop {
        let status = client.call("get_worker_status", json!({"worker_id":worker}))?;
        if let Ok(output) = client.call(
            "output",
            json!({"worker_id":worker,"script":"archive","cursor":cursor}),
        ) {
            let output: Output = serde_json::from_value(output)?;
            std::io::stdout().write_all(&output.bytes)?;
            std::io::stdout().flush()?;
            cursor = output.cursor;
        }
        if status["worker"]["archived"] == true {
            return Ok(());
        }
        match status["worker"]["workspace_scripts"]["phase"].as_str() {
            Some("awaiting_approval") => anyhow::bail!(
                "Archive is waiting for approval. Review `sdk scripts {worker}`, then approve its exact hash."
            ),
            Some("archive_failed") => anyhow::bail!(
                "{}; inspect archive output and retry, or use --force for hook failure",
                status["worker"]["workspace_scripts"]["error"]
            ),
            _ => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn attention_report(client: &Client) -> Result<()> {
    let mut bytes = Vec::new();
    std::io::stdin().take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Ok(());
    }
    let hook: Value = serde_json::from_slice(&bytes)?;
    // Subagent activity must not clear or set its parent's interactive prompt.
    if hook.get("agent_id").is_some_and(|id| !id.is_null()) {
        return Ok(());
    }
    let event = hook["hook_event_name"].as_str().unwrap_or("");
    let tool = hook["tool_name"].as_str().unwrap_or("");
    let waiting = match event {
        "PermissionRequest" => true,
        "PreToolUse" if matches!(tool, "AskUserQuestion" | "ExitPlanMode") => true,
        "PostToolUse" | "PostToolUseFailure" | "UserPromptSubmit" => false,
        _ => return Ok(()),
    };
    let key = hook["tool_use_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(|id| format!("id:{id}"))
        .unwrap_or_else(|| format!("tool:{tool}"));
    client.call(
        "session_attention",
        json!({
            "worker_id": std::env::var("SIGMA_DOCK_WORKER_ID")?,
            "token": std::env::var("SIGMA_DOCK_SESSION_TOKEN")?,
            "key": key, "tool": tool, "waiting": waiting, "clear_all": event == "UserPromptSubmit"
        }),
    )?;
    Ok(())
}
