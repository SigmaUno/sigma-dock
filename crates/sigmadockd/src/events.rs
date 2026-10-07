use anyhow::{Result, bail};
use sigmadock_core::DaemonEvent;
use std::sync::{
    Mutex,
    mpsc::{self, Receiver, SyncSender},
};

#[derive(Default)]
pub(crate) struct EventHub {
    clients: Mutex<Vec<(u64, SyncSender<DaemonEvent>)>>,
    next_id: std::sync::atomic::AtomicU64,
}
impl EventHub {
    pub fn subscribe(&self) -> Result<(u64, Receiver<DaemonEvent>)> {
        let mut clients = self.clients.lock().unwrap();
        if clients.len() >= 16 {
            bail!("maximum event subscriptions reached");
        }
        let (sender, receiver) = mpsc::sync_channel(64);
        sender.try_send(DaemonEvent::Resync).unwrap();
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        clients.push((id, sender));
        Ok((id, receiver))
    }
    pub fn publish(&self, event: DaemonEvent) {
        // Slow clients reconnect/resync instead of consuming unbounded memory.
        self.clients
            .lock()
            .unwrap()
            .retain(|(_, sender)| sender.try_send(event.clone()).is_ok());
    }
    pub fn remove(&self, id: u64) {
        self.clients
            .lock()
            .unwrap()
            .retain(|(client, _)| *client != id);
    }
    pub fn close(&self) {
        self.clients.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lagging_client_cannot_block_other_clients_or_grow_memory() {
        let hub = EventHub::default();
        let (_, slow) = hub.subscribe().unwrap();
        let (_, fast) = hub.subscribe().unwrap();
        assert!(matches!(fast.recv().unwrap(), DaemonEvent::Resync));
        for _ in 0..100 {
            hub.publish(DaemonEvent::CapacityChanged);
            assert!(matches!(fast.recv().unwrap(), DaemonEvent::CapacityChanged));
        }
        assert_eq!(slow.try_iter().count(), 64);
        assert!(slow.recv().is_err());
        hub.close();
        assert!(fast.recv().is_err());
    }
}
