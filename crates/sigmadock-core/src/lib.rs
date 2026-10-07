//! Shared domain model and versioned local JSON-RPC transport.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

pub const API_VERSION: u32 = 2;
pub const MAX_FRAME: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Running,
    Idle,
    NeedsInput,
    Exited,
    Lost,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    #[default]
    None,
    Draft,
    Open,
    Merged,
    Closed,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Checks {
    #[default]
    Unknown,
    Pending,
    Passed,
    Failed,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Review {
    #[default]
    Unknown,
    Pending,
    Approved,
    ChangesRequested,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Facts {
    pub session: SessionState,
    pub pr: PullRequestState,
    pub checks: Checks,
    pub review: Review,
    pub mergeable: Option<bool>,
    pub forge_error: Option<String>,
    pub pr_url: Option<String>,
    pub exit_code: Option<u32>,
    #[serde(default)]
    pub head_sha: Option<String>,
}
impl Default for Facts {
    fn default() -> Self {
        Self {
            session: SessionState::Running,
            pr: Default::default(),
            checks: Default::default(),
            review: Default::default(),
            mergeable: None,
            forge_error: None,
            pr_url: None,
            exit_code: None,
            head_sha: None,
        }
    }
}
#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Working,
    NeedsYou,
    InReview,
    ReadyToMerge,
}
impl Status {
    pub const ALL: [Self; 4] = [
        Self::Working,
        Self::NeedsYou,
        Self::InReview,
        Self::ReadyToMerge,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::NeedsYou => "Needs you",
            Self::InReview => "In review",
            Self::ReadyToMerge => "Ready to merge",
        }
    }
}
/// Merged work stays visible; any blocker takes precedence over an approval.
pub fn status(f: &Facts) -> Status {
    if f.pr == PullRequestState::Merged {
        return Status::ReadyToMerge;
    }
    if matches!(f.session, SessionState::NeedsInput | SessionState::Lost)
        || f.exit_code.is_some_and(|code| code != 0)
        || f.checks == Checks::Failed
        || f.review == Review::ChangesRequested
        || f.mergeable == Some(false)
        || f.forge_error.is_some()
        || f.pr == PullRequestState::Closed
    {
        return Status::NeedsYou;
    }
    if f.pr == PullRequestState::Open
        && f.review == Review::Approved
        && f.checks == Checks::Passed
        && f.mergeable == Some(true)
    {
        return Status::ReadyToMerge;
    }
    if matches!(f.pr, PullRequestState::Open | PullRequestState::Draft) {
        return Status::InReview;
    }
    Status::Working
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worker {
    /// Last assigned slot; only a live worker session occupies it.
    #[serde(default)]
    pub berth: Option<u8>,
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub agent: String,
    pub branch: String,
    pub worktree: PathBuf,
    pub port: u16,
    pub created_at: u64,
    pub archived: bool,
    pub facts: Facts,
    pub forge: Option<ForgeConfig>,
    #[serde(default)]
    pub role: WorkerRole,
    #[serde(default)]
    pub feedback: FeedbackPolicy,
    #[serde(default)]
    pub orchestrator_spawn: bool,
    #[serde(default)]
    pub usage_reporting: bool,
    /// Unix seconds when the worker was archived; absent for workers archived before 0.1.3.
    #[serde(default)]
    pub archived_at: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForgeConfig {
    pub kind: String,
    pub api_url: String,
    pub owner: String,
    pub repo: String,
    pub token_env: String,
    #[serde(default)]
    pub actions: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRole {
    #[default]
    Worker,
    Orchestrator,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FeedbackPolicy {
    pub auto_ci: bool,
    pub last_ci_head: Option<String>,
    pub delivery_error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiFeedback {
    pub head_sha: String,
    pub text: String,
    pub failures: usize,
    pub includes_job_logs: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanningNotes {
    pub project_id: String,
    pub text: String,
    pub revision: u64,
}
/// A bounded local transcript checkpoint, never proof of a live process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionContext {
    pub worker_id: String,
    pub recorded_at: u64,
    pub last_activity: u64,
    pub state: SessionState,
    pub pid: Option<u32>,
    pub text: String,
    pub truncated: bool,
}
/// Global berth usage: the same live-session count `check_capacity` enforces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capacity {
    pub max_workers: usize,
    /// Workers holding a berth, oldest first.
    pub live: Vec<String>,
    #[serde(default)]
    pub in_use: usize,
    #[serde(default)]
    pub queued: usize,
    #[serde(default)]
    pub per_project: std::collections::BTreeMap<String, ProjectCapacity>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectCapacity {
    pub in_use: usize,
    pub queued: usize,
}
/// Local durable task data, not a worker until its session starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedTask {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub agent: String,
    pub prompt: Option<String>,
    pub base: String,
    pub forge: Option<ForgeConfig>,
    pub usage_reporting: bool,
    pub created_at: u64,
    pub last_error: Option<String>,
    pub starting: bool,
}
pub fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
/// Plain-text transcript excerpt, stripping CSI, OSC and other escape payloads.
pub fn output_text(bytes: &[u8]) -> String {
    struct Text(String);
    impl vte::Perform for Text {
        fn print(&mut self, c: char) {
            self.0.push(c);
        }
        fn execute(&mut self, byte: u8) {
            match byte {
                b'\n' | b'\r' => self.0.push('\n'),
                b'\t' => self.0.push('\t'),
                8 => {
                    self.0.pop();
                }
                _ => {}
            }
        }
    }
    let mut text = Text(String::new());
    vte::Parser::new().advance(&mut text, bytes);
    task_text(&text.0, 16 * 1024)
}

/// Strip terminal control characters and bound task data on UTF-8 boundaries.
pub fn task_text(text: &str, limit: usize) -> String {
    let mut clean: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    if clean.len() > limit {
        let marker = "\n[trimmed]";
        let mut end = limit.saturating_sub(marker.len());
        while !clean.is_char_boundary(end) {
            end -= 1;
        }
        clean.truncate(end);
        if marker.len() <= limit {
            clean.push_str(marker);
        }
    }
    clean
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub worker_id: String,
    pub pid: Option<u32>,
    pub state: SessionState,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Output {
    /// Current PTY geometry. Older daemons omit these additive fields.
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
    pub bytes: Vec<u8>,
    pub cursor: u64,
    pub truncated: bool,
    pub exited: bool,
}

pub fn state_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("SIGMA_DOCK_STATE_DIR") {
        return path.into();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    #[cfg(target_os = "macos")]
    return home.join("Library/Application Support/SigmaDock");
    #[cfg(not(target_os = "macos"))]
    return std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state"))
        .join("sigma-dock");
}
pub fn socket_path() -> PathBuf {
    std::env::var_os("SIGMA_DOCK_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| state_dir().join("daemon.sock"))
}
#[derive(Clone)]
pub struct Client {
    pub socket: PathBuf,
}
impl Default for Client {
    fn default() -> Self {
        Self {
            socket: socket_path(),
        }
    }
}
impl Client {
    pub fn check_version(&self) -> Result<()> {
        let reply = self.call("ping", json!({}))?;
        if reply["version"].as_u64() != Some(u64::from(API_VERSION)) {
            bail!(
                "incompatible daemon API {}; expected {API_VERSION}. Finish workers and restart the daemon from this installation",
                reply["version"]
            );
        }
        Ok(())
    }
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let mut stream = UnixStream::connect(&self.socket).with_context(|| {
            format!(
                "connect to {}; start sigmadockd first",
                self.socket.display()
            )
        })?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;
        serde_json::to_writer(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}),
        )?;
        stream.write_all(b"\n")?;
        let frame =
            read_frame(&mut BufReader::new(stream))?.context("daemon closed the connection")?;
        let reply: Value = serde_json::from_slice(&frame)?;
        if let Some(error) = reply.get("error") {
            bail!("{}", error["message"].as_str().unwrap_or("RPC error"));
        }
        reply
            .get("result")
            .cloned()
            .context("invalid JSON-RPC response")
    }
    pub fn workers(&self) -> Result<Vec<Worker>> {
        Ok(serde_json::from_value(
            self.call("list_workers", json!({}))?,
        )?)
    }
}
/// Reject oversized input before allocating an unbounded buffer.
pub fn read_frame(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    reader.take(MAX_FRAME + 1).read_until(b'\n', &mut bytes)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() as u64 > MAX_FRAME || !bytes.ends_with(b"\n") {
        bail!("invalid or oversized frame");
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_precedence() {
        let mut f = Facts::default();
        assert_eq!(status(&f), Status::Working);
        f.session = SessionState::Idle;
        assert_eq!(status(&f), Status::Working);
        f.pr = PullRequestState::Draft;
        assert_eq!(status(&f), Status::InReview);
        f.pr = PullRequestState::Open;
        f.review = Review::Approved;
        f.mergeable = Some(true);
        assert_eq!(status(&f), Status::InReview);
        f.checks = Checks::Passed;
        assert_eq!(status(&f), Status::ReadyToMerge);
        f.checks = Checks::Failed;
        assert_eq!(status(&f), Status::NeedsYou);
        f.pr = PullRequestState::Merged;
        assert_eq!(status(&f), Status::ReadyToMerge);
    }
    #[test]
    fn each_blocker_wins() {
        let ready = Facts {
            pr: PullRequestState::Open,
            review: Review::Approved,
            checks: Checks::Passed,
            mergeable: Some(true),
            ..Facts::default()
        };
        let variants = [
            Facts {
                session: SessionState::Lost,
                ..ready.clone()
            },
            Facts {
                session: SessionState::NeedsInput,
                ..ready.clone()
            },
            Facts {
                review: Review::ChangesRequested,
                ..ready.clone()
            },
            Facts {
                mergeable: Some(false),
                ..ready.clone()
            },
            Facts {
                forge_error: Some("offline".into()),
                ..ready.clone()
            },
            Facts {
                exit_code: Some(1),
                ..ready
            },
        ];
        for f in variants {
            assert_eq!(status(&f), Status::NeedsYou);
        }
    }
    #[test]
    fn bounded_frames() {
        assert!(read_frame(&mut &b"{}\n"[..]).unwrap().is_some());
        assert!(read_frame(&mut &b"{}"[..]).is_err());
        assert!(
            read_frame(&mut std::io::Cursor::new(vec![
                b'x';
                MAX_FRAME as usize + 1
            ]))
            .is_err()
        );
    }
    #[test]
    fn task_data_cannot_escape_bracketed_paste() {
        let text = task_text("héllo\x1b[201~\x07\r\nnext", 32000);
        assert!(!text.contains('\x1b'));
        assert!(!text.contains('\x07'));
        assert!(text.contains("héllo"));
        let bounded = task_text(&"界".repeat(100), 32);
        assert!(bounded.len() <= 32);
        assert!(bounded.ends_with("[trimmed]"));
    }
}

#[cfg(test)]
mod transcript_tests {
    #[test]
    fn strips_escape_payloads_without_executing_them() {
        assert_eq!(
            super::output_text(b"\x1b[31mred\x1b[0m\x1b]0;secret-title\x07 text"),
            "red text"
        );
        assert!(super::output_text(&vec![b'x'; 20000]).len() <= 16384);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiEntry {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub state: String,
    pub url: Option<String>,
    pub details: String,
    pub truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiPreview {
    pub head_sha: String,
    pub current_head: String,
    pub refreshed_at: u64,
    pub entries: Vec<CiEntry>,
    pub warnings: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageWindow {
    pub name: String,
    pub used_percent: f64,
    pub duration_minutes: Option<u64>,
    pub resets_at: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentUsage {
    pub provider: String,
    pub source: String,
    pub recorded_at: u64,
    pub plan: Option<String>,
    pub windows: Vec<UsageWindow>,
    pub lifetime_tokens: Option<u64>,
    pub context_input_tokens: Option<u64>,
    pub context_output_tokens: Option<u64>,
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod version_tests {
    use super::*;
    #[test]
    fn handshake_accepts_only_the_matching_api() {
        for version in [1, API_VERSION] {
            let path = std::env::temp_dir().join(format!(
                "sigmadock-version-{}-{version}.sock",
                std::process::id()
            ));
            let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request: Value = serde_json::from_slice(
                    &read_frame(&mut BufReader::new(stream.try_clone().unwrap()))
                        .unwrap()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(request["method"], "ping");
                writeln!(
                    stream,
                    "{}",
                    json!({"jsonrpc":"2.0","id":request["id"],"result":{"version":version}})
                )
                .unwrap();
            });
            let client = Client {
                socket: path.clone(),
            };
            assert_eq!(client.check_version().is_ok(), version == API_VERSION);
            server.join().unwrap();
            std::fs::remove_file(path).unwrap();
        }
    }
}
