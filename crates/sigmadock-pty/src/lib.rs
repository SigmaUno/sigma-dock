//! PTY ownership and bounded output replay independent of any UI.
use anyhow::{Result, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use sigmadock_core::{Output, OutputSignal, SessionContext, SessionState, output_text, unix_time};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    path::Path,
    sync::{Arc, Mutex},
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
    exited: bool,
    eof: bool,
    exit_code: Option<u32>,
}
pub struct Session {
    generation: u64,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    state: Arc<Mutex<State>>,
    pub pid: Option<u32>,
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
            generation: NEXT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child,
            state,
            pid,
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
    pub fn stop(&self) -> Result<()> {
        let mut child = self.child.lock().unwrap();
        if !self.state.lock().unwrap().exited {
            child.kill()?;
        }
        Ok(())
    }
    pub fn facts(&self) -> (SessionState, Option<u32>) {
        let state = self.state.lock().unwrap();
        let status = if state.exited {
            SessionState::Exited
        } else if state.needs_input {
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
            } else if state.needs_input {
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
            } else if state.needs_input {
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
