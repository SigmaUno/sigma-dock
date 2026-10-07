//! Local invalidation events; snapshots and PTY replay remain authoritative.
use crate::{API_VERSION, Client, read_frame};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{BufReader, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Resync,
    WorkerChanged {
        worker_id: String,
    },
    ProjectsChanged,
    CapacityChanged,
    QueueChanged,
    OutputAvailable {
        worker_id: String,
        cursor: u64,
        rows: u16,
        cols: u16,
        exited: bool,
    },
    Heartbeat,
}

pub struct EventStream {
    reader: BufReader<UnixStream>,
}
impl EventStream {
    /// Blocks until an event arrives; timeout, EOF or a malformed frame requires reconnect/resync.
    pub fn next_event(&mut self) -> Result<Event> {
        let frame = read_frame(&mut self.reader)?.context("event stream closed")?;
        let notification: Value = serde_json::from_slice(&frame)?;
        if notification["jsonrpc"] != "2.0" || notification["method"] != "event" {
            bail!("invalid event notification");
        }
        Ok(serde_json::from_value(notification["params"].clone())?)
    }
}
impl Client {
    pub fn subscribe(&self) -> Result<EventStream> {
        let mut stream = UnixStream::connect(&self.socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(6)))?;
        stream.set_write_timeout(Some(Duration::from_secs(6)))?;
        serde_json::to_writer(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":1,"method":"subscribe","params":{}}),
        )?;
        stream.write_all(b"\n")?;
        let mut reader = BufReader::new(stream);
        let frame = read_frame(&mut reader)?.context("subscribe connection closed")?;
        let reply: Value = serde_json::from_slice(&frame)?;
        if reply["result"]["version"].as_u64() != Some(u64::from(API_VERSION)) {
            bail!("daemon does not support compatible subscriptions: {reply}");
        }
        Ok(EventStream { reader })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::net::UnixListener,
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn subscription_preserves_buffered_events_and_reports_eof() {
        let socket = std::env::temp_dir().join(format!(
            "sigmadock-events-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request: Value = serde_json::from_slice(
                &read_frame(&mut BufReader::new(stream.try_clone().unwrap()))
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(request["method"], "subscribe");
            let frames = format!(
                "{}\n{}\n",
                json!({"jsonrpc":"2.0","id":1,"result":{"version":API_VERSION}}),
                json!({"jsonrpc":"2.0","method":"event","params":Event::Resync})
            );
            stream.write_all(frames.as_bytes()).unwrap();
        });
        let mut events = Client {
            socket: socket.clone(),
        }
        .subscribe()
        .unwrap();
        assert_eq!(events.next_event().unwrap(), Event::Resync);
        assert!(events.next_event().is_err());
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
    }
}
