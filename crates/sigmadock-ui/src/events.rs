//! One shared subscription drives workspace refreshes, previews and the open terminal.
use sigmadock_core::{Client, DaemonEvent, OutputSignal};
use std::{
    collections::{HashMap, HashSet},
    net::Shutdown,
    os::unix::net::UnixStream,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Default)]
struct Inbox {
    connected: bool,
    epoch: u64,
    snapshot_dirty: bool,
    outputs: HashMap<String, OutputSignal>,
    worker_revisions: HashMap<String, u64>,
    dirty_outputs: HashSet<String>,
}
impl Inbox {
    fn apply(&mut self, event: DaemonEvent) {
        match event {
            DaemonEvent::Resync => {
                self.connected = true;
                self.epoch += 1;
                self.snapshot_dirty = true;
                self.outputs.clear();
                self.worker_revisions.clear();
                self.dirty_outputs.clear();
            }
            DaemonEvent::OutputAvailable { worker_id, signal } => {
                self.outputs.insert(worker_id.clone(), signal);
                self.dirty_outputs.insert(worker_id);
            }
            DaemonEvent::WorkerChanged { worker_id } => {
                *self.worker_revisions.entry(worker_id).or_default() += 1;
                self.snapshot_dirty = true;
            }
            DaemonEvent::ProjectsChanged
            | DaemonEvent::CapacityChanged
            | DaemonEvent::QueueChanged => self.snapshot_dirty = true,
            DaemonEvent::Heartbeat => {}
        }
    }
    fn stamp(&self, worker: &str) -> WakeStamp {
        WakeStamp {
            connected: self.connected,
            epoch: self.epoch,
            signal: self.outputs.get(worker).copied(),
            revision: self.worker_revisions.get(worker).copied().unwrap_or(0),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WakeStamp {
    pub connected: bool,
    pub epoch: u64,
    pub signal: Option<OutputSignal>,
    revision: u64,
}
#[derive(Clone, Default)]
pub(crate) struct EventFeed(Arc<(Mutex<Inbox>, Condvar)>);
impl EventFeed {
    #[cfg(test)]
    pub fn emit_for_test(&self, event: DaemonEvent) {
        self.0.0.lock().unwrap().apply(event);
        self.0.1.notify_all();
    }
    pub fn take_refresh(&self) -> (u64, bool) {
        let mut inbox = self.0.0.lock().unwrap();
        (inbox.epoch, std::mem::take(&mut inbox.snapshot_dirty))
    }
    pub fn preview_request(&self, worker: &str) -> (bool, bool, Option<OutputSignal>) {
        let mut inbox = self.0.0.lock().unwrap();
        (
            inbox.connected,
            inbox.dirty_outputs.remove(worker),
            inbox.outputs.get(worker).copied(),
        )
    }
    pub fn retry_output(&self, worker: String) {
        self.0.0.lock().unwrap().dirty_outputs.insert(worker);
    }
    pub fn stamp(&self, worker: &str) -> WakeStamp {
        self.0.0.lock().unwrap().stamp(worker)
    }
    pub fn wait_for_output(
        &self,
        worker: &str,
        previous: Option<WakeStamp>,
        alive: &AtomicBool,
    ) -> Option<WakeStamp> {
        let (lock, changed) = &*self.0;
        let mut inbox = lock.lock().unwrap();
        loop {
            if !alive.load(Ordering::Relaxed) {
                return None;
            }
            let stamp = inbox.stamp(worker);
            if previous != Some(stamp) {
                return Some(stamp);
            }
            if !inbox.connected {
                // Slow polling fallback when an API-2 daemon lacks subscriptions.
                drop(
                    changed
                        .wait_timeout(inbox, Duration::from_millis(250))
                        .unwrap(),
                );
                return alive.load(Ordering::Relaxed).then(|| self.stamp(worker));
            }
            inbox = changed
                .wait_timeout(inbox, Duration::from_millis(200))
                .unwrap()
                .0;
        }
    }
}
pub(crate) struct EventMonitor {
    pub feed: EventFeed,
    stop: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<UnixStream>>>,
}
impl EventMonitor {
    pub fn start(client: Client) -> Self {
        let feed = EventFeed::default();
        let stop = Arc::new(AtomicBool::new(false));
        let socket = Arc::new(Mutex::new(None));
        let monitor = Self {
            feed: feed.clone(),
            stop: stop.clone(),
            socket: socket.clone(),
        };
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Ok(mut subscription) = client.subscribe()
                    && let Ok(handle) = subscription.shutdown_handle()
                {
                    *socket.lock().unwrap() = Some(handle);
                    if stop.load(Ordering::Relaxed) {
                        if let Some(socket) = socket.lock().unwrap().take() {
                            let _ = socket.shutdown(Shutdown::Both);
                        }
                        break;
                    }
                    while !stop.load(Ordering::Relaxed) {
                        let Ok(event) = subscription.next_event() else {
                            break;
                        };
                        feed.0.0.lock().unwrap().apply(event);
                        feed.0.1.notify_all();
                    }
                }
                socket.lock().unwrap().take();
                {
                    let mut inbox = feed.0.0.lock().unwrap();
                    if inbox.connected {
                        inbox.snapshot_dirty = true;
                    }
                    inbox.connected = false;
                }
                feed.0.1.notify_all();
                // Bounded reconnect delay; no GUI executor or daemon lock is held.
                for _ in 0..10 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
        monitor
    }
}
impl Drop for EventMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(socket) = self.socket.lock().unwrap().take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        self.feed.0.1.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subscription_reconnects_and_drop_interrupts_a_blocked_reader() {
        use std::{
            io::{BufReader, Read, Write},
            os::unix::net::UnixListener,
        };
        let path =
            std::env::temp_dir().join(format!("sigmadock-reconnect-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            for connection in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let request: serde_json::Value = serde_json::from_slice(
                    &sigmadock_core::read_frame(&mut BufReader::new(stream.try_clone().unwrap()))
                        .unwrap()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(request["method"], "subscribe");
                writeln!(stream, "{}", serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"version":sigmadock_core::API_VERSION}})).unwrap();
                // Fragment a notification to verify that buffering preserves partial frames.
                stream
                    .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"event\",")
                    .unwrap();
                thread::sleep(Duration::from_millis(10));
                stream
                    .write_all(b"\"params\":{\"type\":\"resync\"}}\n")
                    .unwrap();
                if connection == 1 {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
                }
            }
        });
        let monitor = EventMonitor::start(Client {
            socket: path.clone(),
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while monitor.feed.take_refresh().0 < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "subscription did not reconnect"
            );
            thread::sleep(Duration::from_millis(10));
        }
        drop(monitor);
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn bursts_coalesce_and_reconnect_invalidates_old_output_generations() {
        let feed = EventFeed::default();
        let signal = OutputSignal {
            cursor: 5,
            cols: 80,
            rows: 24,
            exited: false,
            generation: 1,
        };
        let mut inbox = feed.0.0.lock().unwrap();
        inbox.apply(DaemonEvent::Resync);
        for _ in 0..100 {
            inbox.apply(DaemonEvent::OutputAvailable {
                worker_id: "w".into(),
                signal,
            });
            inbox.apply(DaemonEvent::WorkerChanged {
                worker_id: "w".into(),
            });
        }
        drop(inbox);
        assert_eq!(feed.take_refresh(), (1, true));
        assert_eq!(feed.take_refresh(), (1, false));
        assert_eq!(feed.preview_request("w"), (true, true, Some(signal)));
        assert_eq!(feed.preview_request("w"), (true, false, Some(signal)));
        feed.0.0.lock().unwrap().apply(DaemonEvent::Resync);
        assert_eq!(feed.take_refresh(), (2, true));
        assert_eq!(feed.preview_request("w"), (true, false, None));
    }
    #[test]
    fn terminal_waits_for_its_worker_and_cancel_is_bounded() {
        let feed = EventFeed::default();
        feed.0.0.lock().unwrap().apply(DaemonEvent::Resync);
        let previous = feed.stamp("w");
        let alive = Arc::new(AtomicBool::new(true));
        let waiter = feed.clone();
        let waiting = alive.clone();
        let thread = thread::spawn(move || waiter.wait_for_output("w", Some(previous), &waiting));
        feed.0
            .0
            .lock()
            .unwrap()
            .apply(DaemonEvent::OutputAvailable {
                worker_id: "other".into(),
                signal: OutputSignal {
                    cursor: 1,
                    cols: 80,
                    rows: 24,
                    exited: false,
                    generation: 1,
                },
            });
        assert_eq!(feed.stamp("w"), previous);
        alive.store(false, Ordering::Relaxed);
        feed.0.1.notify_all();
        assert!(thread.join().unwrap().is_none());
    }
}
