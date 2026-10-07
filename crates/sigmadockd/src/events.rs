//! Bounded fan-out. Slow subscribers reconnect and resync instead of blocking supervision.
use crate::Daemon;
use anyhow::Result;
use serde_json::{Value, json};
use sigmadock_core::events::Event;
use std::{
    collections::HashMap,
    io::Write,
    os::unix::net::UnixStream,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::Duration,
};

#[derive(Default)]
pub(crate) struct Hub {
    subscribers: Vec<SyncSender<Event>>,
    workers: HashMap<String, Value>,
    projects: Value,
    capacity: Value,
    queue: Value,
    output: HashMap<String, (u64, u16, u16, bool)>,
}
impl Hub {
    pub(crate) fn subscribe(&mut self) -> Receiver<Event> {
        self.publish(Event::Heartbeat);
        let (sender, receiver) = mpsc::sync_channel(64);
        sender.try_send(Event::Resync).unwrap();
        self.subscribers.push(sender);
        receiver
    }
    pub(crate) fn publish(&mut self, event: Event) {
        self.subscribers
            .retain(|subscriber| subscriber.try_send(event.clone()).is_ok());
    }
}

pub(crate) fn observe(daemon: &mut Daemon) -> Result<()> {
    if daemon.events.subscribers.is_empty() {
        return Ok(());
    }
    let workers: HashMap<_, _> = daemon
        .workers
        .iter()
        .map(|(id, worker)| Ok((id.clone(), serde_json::to_value(worker)?)))
        .collect::<Result<_>>()?;
    let projects = serde_json::to_value(daemon.store.projects()?)?;
    let capacity = serde_json::to_value(daemon.capacity()?)?;
    let queue = serde_json::to_value(daemon.store.queue()?)?;
    let output: HashMap<_, _> = daemon
        .sessions
        .iter()
        .map(|(id, session)| (id.clone(), session.output_key()))
        .collect();
    let hub = &mut daemon.events;
    for (id, value) in &workers {
        if hub.workers.get(id) != Some(value) {
            hub.publish(Event::WorkerChanged {
                worker_id: id.clone(),
            });
        }
    }
    for id in hub
        .workers
        .keys()
        .filter(|id| !workers.contains_key(*id))
        .cloned()
        .collect::<Vec<_>>()
    {
        hub.publish(Event::WorkerChanged { worker_id: id });
    }
    if projects != hub.projects {
        hub.publish(Event::ProjectsChanged);
    }
    if capacity != hub.capacity {
        hub.publish(Event::CapacityChanged);
    }
    if queue != hub.queue {
        hub.publish(Event::QueueChanged);
    }
    for (id, &(cursor, rows, cols, exited)) in &output {
        if hub.output.get(id) != output.get(id) {
            hub.publish(Event::OutputAvailable {
                worker_id: id.clone(),
                cursor,
                rows,
                cols,
                exited,
            });
        }
    }
    hub.workers = workers;
    hub.projects = projects;
    hub.capacity = capacity;
    hub.queue = queue;
    hub.output = output;
    Ok(())
}

pub(crate) fn serve(
    mut stream: UnixStream,
    receiver: Receiver<Event>,
    shutdown: &AtomicBool,
    id: Value,
) -> Result<()> {
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    serde_json::to_writer(
        &mut stream,
        &json!({"jsonrpc":"2.0","id":id,"result":{"version":sigmadock_core::API_VERSION}}),
    )?;
    stream.write_all(b"\n")?;
    while !shutdown.load(Ordering::Relaxed) {
        let event = match receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => Event::Heartbeat,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        serde_json::to_writer(
            &mut stream,
            &json!({"jsonrpc":"2.0","method":"event","params":event}),
        )?;
        stream.write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subscriptions_resync_and_slow_readers_are_disconnected() {
        let mut hub = Hub::default();
        let slow = hub.subscribe();
        let fast = hub.subscribe();
        assert_eq!(fast.recv().unwrap(), Event::Resync);
        for _ in 0..65 {
            hub.publish(Event::CapacityChanged);
            assert_eq!(fast.recv().unwrap(), Event::CapacityChanged);
        }
        assert_eq!(hub.subscribers.len(), 1);
        assert_eq!(slow.try_iter().count(), 64);
        assert!(slow.recv().is_err());
    }
}
