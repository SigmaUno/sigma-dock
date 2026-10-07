//! PTY ownership and bounded output replay independent of any UI.
use anyhow::{Result, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use sigmadock_core::{Output, OutputSignal, SessionContext, SessionState, output_text, unix_time};
use std::{
    collections::{HashSet, VecDeque},
    io::{Read, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
const CAPACITY: usize = 1024 * 1024;
const CHUNK: usize = 64 * 1024;
struct State {
    rows: u16,
    cols: u16,
    output: VecDeque<u8>,
    cursor: u64,
    last_activity: Instant,
    activity_at: u64,
    needs_input: bool,
    hook_waiting: HashSet<String>,
    exited: bool,
    eof: bool,
    exit_code: Option<u32>,
}
pub struct Session {
    stopping: AtomicBool,
    stop_complete: Arc<AtomicBool>,
    generation: u64,
    attention_token: Option<String>,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    state: Arc<Mutex<State>>,
    pub pid: Option<u32>,
    birth: Option<String>,
    idle_timeout: Duration,
}
impl Session {
    pub fn spawn(
        program: &str,
        args: &[String],
        env: &[(String, String)],
        cwd: &Path,
    ) -> Result<Self> {
        let pair = native_pty_system().openpty(PtySize {
            rows: 30,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        // Acquire the I/O handles before creating the child, so failures cannot orphan it.
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.cwd(cwd);
        for (key, value) in env {
            command.env(key, value);
        }
        let child = pair.slave.spawn_command(command)?;
        let pid = child.process_id();
        let birth = pid.and_then(|pid| {
            processes()
                .into_iter()
                .find(|(id, _, _)| *id == pid)
                .map(|(_, _, birth)| birth)
        });
        let child = Arc::new(Mutex::new(child));
        drop(pair.slave);
        let state = Arc::new(Mutex::new(State {
            rows: 30,
            cols: 120,
            output: VecDeque::new(),
            cursor: 0,
            last_activity: Instant::now(),
            activity_at: unix_time(),
            needs_input: false,
            hook_waiting: HashSet::new(),
            exited: false,
            eof: false,
            exit_code: None,
        }));
        let output_state = state.clone();
        thread::spawn(move || {
            let mut bytes = [0; 8192];
            let mut detector = Detector::default();
            loop {
                let size = match reader.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(size) => size,
                };
                let alert = detector.feed(&bytes[..size]);
                let mut state = output_state.lock().unwrap();
                state.last_activity = Instant::now();
                state.activity_at = unix_time();
                state.needs_input |= alert;
                state.cursor += size as u64;
                state.output.extend(&bytes[..size]);
                while state.output.len() > CAPACITY {
                    state.output.pop_front();
                }
            }
            output_state.lock().unwrap().eof = true;
        });
        let exit_state = state.clone();
        let wait_child = child.clone();
        thread::spawn(move || {
            loop {
                let mut child = wait_child.lock().unwrap();
                match child.try_wait() {
                    Ok(None) => {
                        drop(child);
                        thread::sleep(Duration::from_millis(20));
                    }
                    status => {
                        let mut state = exit_state.lock().unwrap();
                        state.exited = true;
                        state.exit_code =
                            Some(status.ok().flatten().map(|s| s.exit_code()).unwrap_or(1));
                        break;
                    }
                }
            }
        });
        static NEXT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Ok(Self {
            stopping: AtomicBool::new(false),
            stop_complete: Arc::new(AtomicBool::new(false)),
            generation: NEXT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            attention_token: env
                .iter()
                .find(|(name, _)| name == "SIGMA_DOCK_SESSION_TOKEN")
                .map(|(_, value)| value.clone()),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child,
            state,
            pid,
            birth,
            idle_timeout: Duration::from_secs(60),
        })
    }
    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }
    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > CHUNK {
            bail!("input exceeds 64 KiB");
        }
        self.writer.lock().unwrap().write_all(bytes)?;
        let mut state = self.state.lock().unwrap();
        state.needs_input = false;
        state.last_activity = Instant::now();
        state.activity_at = unix_time();
        Ok(())
    }
    /// Session-local hook reports cannot affect a resumed session with a new token.
    pub fn attention(
        &self,
        token: &str,
        key: &str,
        tool: &str,
        waiting: bool,
        clear_all: bool,
    ) -> Result<()> {
        if self.attention_token.as_deref() != Some(token) || token.is_empty() {
            bail!("attention report belongs to a different session");
        }
        if key.len() > 256 || tool.len() > 256 || (key.is_empty() && !clear_all) {
            bail!("invalid attention episode");
        }
        let mut state = self.state.lock().unwrap();
        if state.exited {
            bail!("session has exited");
        }
        if clear_all {
            state.hook_waiting.clear();
            state.needs_input = false;
        } else if waiting {
            if state.hook_waiting.len() >= 64 && !state.hook_waiting.contains(key) {
                bail!("too many pending attention episodes");
            }
            state.hook_waiting.insert(key.into());
        } else {
            state.hook_waiting.remove(key);
            // PermissionRequest payloads can omit tool_use_id; clear their named episode too.
            state.hook_waiting.remove(&format!("tool:{tool}"));
        }
        state.last_activity = Instant::now();
        state.activity_at = unix_time();
        Ok(())
    }
    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        if rows == 0 || cols == 0 || rows > 1000 || cols > 1000 {
            bail!("terminal dimensions must be 1..1000");
        }
        let mut state = self.state.lock().unwrap();
        self.master.lock().unwrap().resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        state.rows = rows;
        state.cols = cols;
        Ok(())
    }
    /// portable-pty creates a session/process-group leader. Stop the whole group,
    /// plus descendants that created their own session while their parent is alive.
    pub fn stop(&self) -> Result<()> {
        if self.stopping.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let Some(pid) = self.pid else {
            return self.child.lock().unwrap().kill().map_err(Into::into);
        };
        // An exited terminal can be reviewed much later. Never signal a recycled leader PID.
        let current = processes();
        if self.birth.as_ref().is_some_and(|birth| {
            current
                .iter()
                .any(|(id, _, now)| *id == pid && now != birth)
        }) {
            self.stop_complete.store(true, Ordering::SeqCst);
            return Ok(());
        }
        let descendants = descendants(pid);
        if self.state.lock().unwrap().exited && current.is_empty() {
            self.stop_complete.store(true, Ordering::SeqCst);
            return Ok(());
        }
        if let Err(error) = signal_group(pid, libc::SIGTERM) {
            self.stopping.store(false, Ordering::SeqCst);
            return Err(error);
        }
        signal_descendants(&descendants, libc::SIGTERM);
        let complete = self.stop_complete.clone();
        let birth = self.birth.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(2));
            if !birth.as_ref().is_some_and(|birth| {
                processes()
                    .iter()
                    .any(|(id, _, now)| *id == pid && now != birth)
            }) {
                let _ = signal_group(pid, libc::SIGKILL);
            }
            signal_descendants(&descendants, libc::SIGKILL);
            complete.store(true, Ordering::SeqCst);
        });
        Ok(())
    }
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }
    pub fn terminated(&self) -> bool {
        self.facts().0 == SessionState::Exited
            && (!self.stopping.load(Ordering::SeqCst) || self.stop_complete.load(Ordering::SeqCst))
    }
    pub fn facts(&self) -> (SessionState, Option<u32>) {
        let state = self.state.lock().unwrap();
        let status = if state.exited {
            SessionState::Exited
        } else if state.needs_input || !state.hook_waiting.is_empty() {
            SessionState::NeedsInput
        } else if state.last_activity.elapsed() > self.idle_timeout {
            SessionState::Idle
        } else {
            SessionState::Running
        };
        (status, state.exit_code)
    }
    pub fn checkpoint_key(&self) -> (u64, u64, SessionState) {
        let state = self.state.lock().unwrap();
        (
            state.cursor,
            state.activity_at,
            if state.exited {
                SessionState::Exited
            } else if state.needs_input || !state.hook_waiting.is_empty() {
                SessionState::NeedsInput
            } else if state.last_activity.elapsed() > self.idle_timeout {
                SessionState::Idle
            } else {
                SessionState::Running
            },
        )
    }
    pub fn checkpoint(&self, worker_id: &str, since: u64) -> SessionContext {
        let state = self.state.lock().unwrap();
        let offset = since
            .saturating_sub(state.cursor - state.output.len() as u64)
            .min(state.output.len() as u64) as usize;
        let bytes: Vec<_> = state
            .output
            .iter()
            .skip(offset)
            .rev()
            .take(16 * 1024)
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        SessionContext {
            worker_id: worker_id.into(),
            recorded_at: unix_time(),
            last_activity: state.activity_at,
            state: if state.exited {
                SessionState::Exited
            } else if state.needs_input || !state.hook_waiting.is_empty() {
                SessionState::NeedsInput
            } else if state.last_activity.elapsed() > self.idle_timeout {
                SessionState::Idle
            } else {
                SessionState::Running
            },
            pid: self.pid,
            text: output_text(&bytes),
            truncated: state.cursor.saturating_sub(since) > bytes.len() as u64,
        }
    }
    pub fn output_signal(&self) -> OutputSignal {
        let state = self.state.lock().unwrap();
        OutputSignal {
            cursor: state.cursor,
            cols: state.cols,
            rows: state.rows,
            exited: state.exited && state.eof,
            generation: self.generation,
        }
    }
    pub fn output(&self, cursor: u64) -> Output {
        let state = self.state.lock().unwrap();
        let start = state.cursor - state.output.len() as u64;
        let offset = cursor.clamp(start, state.cursor);
        let bytes: Vec<u8> = state
            .output
            .iter()
            .skip((offset - start) as usize)
            .take(CHUNK)
            .copied()
            .collect();
        Output {
            rows: Some(state.rows),
            cols: Some(state.cols),
            cursor: offset + bytes.len() as u64,
            exited: state.exited && state.eof && offset + bytes.len() as u64 == state.cursor,
            bytes,
            truncated: cursor < start || cursor > state.cursor,
        }
    }
}
fn signal_group(pid: u32, signal: i32) -> Result<()> {
    // SAFETY: a negative PID signals the PTY's process group, never the daemon's.
    if unsafe { libc::kill(-(pid as i32), signal) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error.into());
        }
    }
    Ok(())
}
fn processes() -> Vec<(u32, u32, String)> {
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,lstart="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((
                fields.next()?.parse().ok()?,
                fields.next()?.parse().ok()?,
                fields.collect::<Vec<_>>().join(" "),
            ))
        })
        .collect()
}
fn descendants(root: u32) -> Vec<(u32, String)> {
    let all = processes();
    let mut ids = vec![root];
    loop {
        let previous = ids.len();
        for (pid, parent, _) in &all {
            if ids.contains(parent) && !ids.contains(pid) {
                ids.push(*pid);
            }
        }
        if ids.len() == previous {
            break;
        }
    }
    all.into_iter()
        .filter(|(pid, _, _)| *pid != root && ids.contains(pid))
        .map(|(pid, _, birth)| (pid, birth))
        .collect()
}
fn signal_descendants(descendants: &[(u32, String)], signal: i32) {
    let current = processes();
    for (pid, birth) in descendants {
        if current
            .iter()
            .any(|(now, _, started)| now == pid && started == birth)
        {
            // SAFETY: verify the descendant's start time to avoid signalling a reused PID.
            unsafe {
                libc::kill(*pid as i32, signal);
            }
        }
    }
}
#[derive(Default)]
pub struct Detector {
    escaped: bool,
    osc: Option<Vec<u8>>,
    osc_escape: bool,
}
impl Detector {
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        let mut alert = false;
        for &byte in bytes {
            if let Some(osc) = &mut self.osc {
                if byte == 7 || (self.osc_escape && byte == b'\\') {
                    alert |= osc.starts_with(b"9;") || osc.starts_with(b"777;notify;");
                    self.osc = None;
                    self.osc_escape = false;
                } else {
                    self.osc_escape = byte == 0x1b;
                    if osc.len() < 4096 {
                        osc.push(byte);
                    }
                }
            } else if self.escaped {
                self.escaped = false;
                if byte == b']' {
                    self.osc = Some(Vec::new());
                }
            } else if byte == 0x1b {
                self.escaped = true;
            } else if byte == 7 {
                alert = true;
            }
        }
        alert
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_notifications_and_titles() {
        let mut detector = Detector::default();
        assert!(!detector.feed(b"\x1b]777;not"));
        assert!(detector.feed(b"ify;title;body\x1b\\"));
        assert!(!detector.feed(b"\x1b]0;title\x07"));
        assert!(detector.feed(b"\x07"));
        assert!(detector.feed(b"\x1b]9;hello\x07"));
    }
    #[test]
    fn hook_attention_survives_typing_and_other_episodes_until_resolved() {
        let session = Session::spawn(
            "/bin/sh",
            &["-c".into(), "read a; read b".into()],
            &[("SIGMA_DOCK_SESSION_TOKEN".into(), "current".into())],
            Path::new("/tmp"),
        )
        .unwrap();
        assert!(
            session
                .attention("old", "id:q", "AskUserQuestion", true, false)
                .is_err()
        );
        session
            .attention("current", "id:q", "AskUserQuestion", true, false)
            .unwrap();
        session
            .attention("current", "tool:Bash", "Bash", true, false)
            .unwrap();
        session.write(b"x").unwrap();
        assert_eq!(session.facts().0, SessionState::NeedsInput);
        session
            .attention("current", "id:q", "AskUserQuestion", false, false)
            .unwrap();
        assert_eq!(session.facts().0, SessionState::NeedsInput);
        session
            .attention("current", "id:permission", "Bash", false, false)
            .unwrap();
        assert_eq!(session.facts().0, SessionState::Running);
        session
            .attention("current", "id:q2", "AskUserQuestion", true, false)
            .unwrap();
        session.attention("current", "", "", false, true).unwrap();
        assert_eq!(session.facts().0, SessionState::Running);
        session.state.lock().unwrap().exited = true;
        assert!(
            session
                .attention("current", "id:q3", "AskUserQuestion", true, false)
                .is_err()
        );
        session.state.lock().unwrap().exited = false;
        session.stop().unwrap();
    }
    #[test]
    fn real_pty_roundtrip() {
        let session = Session::spawn(
            "/bin/sh",
            &[
                "-c".into(),
                "read value; printf 'reply:%s' \"$value\"".into(),
            ],
            &[],
            Path::new("/tmp"),
        )
        .unwrap();
        assert_eq!(
            (session.output(0).cols, session.output(0).rows),
            (Some(120), Some(30))
        );
        session.resize(24, 80).unwrap();
        assert_eq!(
            (session.output(0).cols, session.output(0).rows),
            (Some(80), Some(24))
        );
        assert!(session.resize(0, 80).is_err());
        assert_eq!(
            (session.output(0).cols, session.output(0).rows),
            (Some(80), Some(24))
        );
        session.write(b"hello\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !session.output(0).exited && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let output = session.output(0);
        assert!(String::from_utf8_lossy(&output.bytes).contains("reply:hello"));
        assert_eq!(session.facts().1, Some(0));
    }
}
