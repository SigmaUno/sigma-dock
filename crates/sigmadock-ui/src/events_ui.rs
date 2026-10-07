//! One subscription thread coalesces invalidations; UI work stays capped at preview rate.
use crate::{
    Workspace,
    berths_ui::{self, Snapshot},
};
use gpui::Context;
use sigmadock_core::events::Event;
use std::{
    collections::HashSet,
    sync::{Arc, Condvar, Mutex, atomic::Ordering},
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Inbox {
    refresh: bool,
    resync: bool,
    output: HashSet<String>,
}

impl Inbox {
    fn record(&mut self, event: Event) -> Option<String> {
        match event {
            Event::Heartbeat => None,
            Event::Resync => {
                self.refresh = true;
                self.resync = true;
                None
            }
            Event::OutputAvailable { worker_id, .. } => {
                self.output.insert(worker_id.clone());
                Some(worker_id)
            }
            _ => {
                self.refresh = true;
                None
            }
        }
    }
}

#[derive(Default)]
struct WakeState {
    generation: u64,
    connected: bool,
    worker: Option<String>,
}

#[derive(Default)]
pub(crate) struct OutputWake {
    state: Mutex<WakeState>,
    changed: Condvar,
}
impl OutputWake {
    pub(crate) fn cancel_wait(&self) {
        let mut state = self.state.lock().unwrap();
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
    }
    pub(crate) fn watch(&self, worker: String) {
        self.state.lock().unwrap().worker = Some(worker);
        self.cancel_wait();
    }
    fn output(&self, worker: &str) {
        let mut state = self.state.lock().unwrap();
        if state.worker.as_deref() == Some(worker) {
            state.generation = state.generation.wrapping_add(1);
            self.changed.notify_all();
        }
    }
    pub(crate) fn generation(&self) -> u64 {
        self.state.lock().unwrap().generation
    }
    fn notify(&self, connected: bool) {
        let mut state = self.state.lock().unwrap();
        state.generation = state.generation.wrapping_add(1);
        state.connected = connected;
        self.changed.notify_all();
    }
    pub(crate) fn wait(&self, generation: u64) {
        let state = self.state.lock().unwrap();
        let timeout = if state.connected {
            Duration::from_secs(30)
        } else {
            Duration::from_millis(750)
        };
        drop(
            self.changed
                .wait_timeout_while(state, timeout, |state| state.generation == generation)
                .unwrap(),
        );
    }
}

impl Workspace {
    pub(crate) fn spawn_event_loop(&self, cx: &mut Context<Self>) {
        let inbox = Arc::new(Mutex::new(Inbox::default()));
        let event_inbox = inbox.clone();
        let client = self.client.clone();
        let shutdown = self.event_shutdown.clone();
        let wake = self.output_wake.clone();
        thread::spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                if let Ok(mut stream) = client.subscribe() {
                    {
                        let mut inbox = event_inbox.lock().unwrap();
                        inbox.refresh = true;
                        inbox.resync = true;
                    }
                    wake.notify(true);
                    while !shutdown.load(Ordering::Relaxed) {
                        let Ok(event) = stream.next_event() else {
                            break;
                        };
                        let worker = event_inbox.lock().unwrap().record(event);
                        if let Some(worker) = worker {
                            wake.output(&worker);
                        }
                    }
                    {
                        let mut inbox = event_inbox.lock().unwrap();
                        inbox.refresh = true;
                        inbox.resync = true;
                    }
                }
                wake.notify(false);
                thread::sleep(Duration::from_secs(1));
            }
        });
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let mut last_resync = Instant::now();
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(750))
                    .await;
                let pending = std::mem::take(&mut *inbox.lock().unwrap());
                let resync = pending.resync || last_resync.elapsed() >= Duration::from_secs(30);
                let refresh = pending.refresh || resync;
                let client = client.clone();
                let snapshot = if refresh {
                    if resync {
                        last_resync = Instant::now();
                    }
                    Some(
                        cx.background_executor()
                            .spawn({
                                let client = client.clone();
                                async move { Snapshot::load(&client) }
                            })
                            .await,
                    )
                } else {
                    None
                };
                let Ok(targets) = this.update(cx, |this, cx| {
                    if let Some(snapshot) = snapshot {
                        this.apply_snapshot(snapshot);
                        cx.notify();
                    }
                    if this.preferences.updates.due(crate::updates::now()) {
                        this.check_updates(false, cx);
                    }
                    this.capacity
                        .live
                        .iter()
                        .filter(|id| {
                            resync
                                || pending.output.contains(*id)
                                || !this.previews.contains_key(*id)
                        })
                        .map(|id| (id.clone(), this.previews.get(id).map_or(0, |p| p.cursor)))
                        .collect::<Vec<_>>()
                }) else {
                    break;
                };
                if targets.is_empty() {
                    continue;
                }
                let updates = cx
                    .background_executor()
                    .spawn(async move {
                        targets
                            .into_iter()
                            .filter_map(|(id, cursor)| {
                                berths_ui::fetch_output(&client, &id, cursor)
                                    .ok()
                                    .map(|out| (id, out))
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        let mut changed = false;
                        for (id, outputs) in updates {
                            if !this.capacity.live.contains(&id) {
                                continue;
                            }
                            let preview = this
                                .previews
                                .entry(id)
                                .or_insert_with(berths_ui::Preview::new);
                            for output in outputs {
                                changed |= preview.feed(&output);
                            }
                        }
                        if changed && this.terminal.is_none() {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_events_coalesce_without_triggering_metadata_polls() {
        let mut inbox = Inbox::default();
        for cursor in 1..=100 {
            inbox.record(Event::OutputAvailable {
                worker_id: "worker".into(),
                cursor,
                rows: 30,
                cols: 120,
                exited: false,
            });
        }
        inbox.record(Event::Heartbeat);
        assert_eq!(inbox.output.len(), 1);
        assert!(!inbox.refresh);
        inbox.record(Event::WorkerChanged {
            worker_id: "worker".into(),
        });
        assert!(inbox.refresh);
        assert!(!inbox.resync);
        inbox.record(Event::Resync);
        assert!(inbox.resync);
    }
    #[test]
    fn only_the_selected_terminal_wakes_and_cancellation_preserves_connection() {
        let wake = OutputWake::default();
        wake.notify(true);
        wake.watch("selected".into());
        let generation = wake.generation();
        wake.output("other");
        assert_eq!(wake.generation(), generation);
        wake.output("selected");
        assert_ne!(wake.generation(), generation);
        wake.cancel_wait();
        assert!(wake.state.lock().unwrap().connected);
    }
    #[test]
    fn wake_remembers_events_between_read_and_wait() {
        let wake = OutputWake::default();
        let previous = wake.generation();
        wake.notify(true);
        let start = Instant::now();
        wake.wait(previous);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
