//! Finder launches receive a minimal environment; bundled helpers need no shell wrapper.
use anyhow::{Context, Result, bail};
use serde_json::json;
use sigmadock_core::{API_VERSION, Client, state_dir};
use std::{
    fs::{DirBuilder, OpenOptions},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub fn ensure_bundled_daemon(client: &Client, executable: &Path) -> Result<()> {
    let Some(bin) = executable.parent() else {
        return Ok(());
    };
    let Some(contents) = bin.parent() else {
        return Ok(());
    };
    if bin.file_name().is_none_or(|name| name != "MacOS") || !contents.join("Info.plist").is_file()
    {
        return Ok(());
    }
    if let Ok(reply) = client.call("ping", json!({})) {
        if reply["version"].as_u64() != Some(u64::from(API_VERSION)) {
            bail!(
                "The running daemon uses a different API version; restart it before opening this app"
            );
        }
        return Ok(());
    }
    let state = state_dir();
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&state)?;
    let log_path = state.join("daemon.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)?;
    let mut paths = vec![bin.to_path_buf()];
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        paths.extend([home.join(".local/bin"), home.join(".cargo/bin")]);
    }
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    paths.extend(
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ]
        .map(Into::into),
    );
    let mut child = Command::new(bin.join("sigmadockd"))
        .args(["--socket"])
        .arg(&client.socket)
        .arg("--mcp-binary")
        .arg(bin.join("sigmadock-mcp"))
        .env("PATH", std::env::join_paths(paths)?)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .with_context(|| format!("Start bundled daemon; see {}", log_path.display()))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(reply) = client.call("ping", json!({}))
            && reply["version"].as_u64() == Some(u64::from(API_VERSION))
        {
            thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            bail!(
                "Bundled daemon exited ({status}); see {}",
                log_path.display()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
    // Keep a started daemon alive even if startup took longer than expected.
    thread::spawn(move || {
        let _ = child.wait();
    });
    bail!("Daemon startup timed out; see {}", log_path.display())
}
