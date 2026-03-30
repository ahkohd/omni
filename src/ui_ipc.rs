use std::fs;
use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;

const UI_EVENT_SOCKET_LOOP_TICK: Duration = Duration::from_millis(40);
const UI_EVENT_CLIENT_WRITE_TIMEOUT: Duration = Duration::from_millis(10);

#[derive(Debug, Clone)]
pub struct UiEventPublisher {
    tx: mpsc::Sender<UiBusMessage>,
    seq: Arc<AtomicU64>,
}

impl UiEventPublisher {
    pub fn emit(&self, event_type: &str, payload: serde_json::Value) {
        let event = UiEventEnvelope {
            v: 1,
            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
            at_ms: now_ms(),
            event_type: event_type.to_string(),
            payload,
        };

        let _ = self.tx.send(UiBusMessage::Publish(event));
    }
}

pub struct UiEventRuntime {
    publisher: UiEventPublisher,
    tx: mpsc::Sender<UiBusMessage>,
    worker: Option<thread::JoinHandle<()>>,
}

impl UiEventRuntime {
    pub fn start(socket_path: PathBuf) -> Result<Self> {
        if socket_path.exists() {
            let _ = fs::remove_file(&socket_path);
        }

        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("failed binding UI socket at {}", socket_path.display()))?;
        listener
            .set_nonblocking(true)
            .context("failed setting UI socket listener nonblocking")?;

        let (tx, rx) = mpsc::channel::<UiBusMessage>();
        let publisher = UiEventPublisher {
            tx: tx.clone(),
            seq: Arc::new(AtomicU64::new(0)),
        };

        let worker = thread::spawn(move || {
            run_event_bus(listener, rx, socket_path);
        });

        Ok(Self {
            publisher,
            tx,
            worker: Some(worker),
        })
    }

    pub fn publisher(&self) -> UiEventPublisher {
        self.publisher.clone()
    }

    pub fn shutdown(&mut self) {
        let _ = self.tx.send(UiBusMessage::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for UiEventRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Debug)]
enum UiBusMessage {
    Publish(UiEventEnvelope),
    Shutdown,
}

#[derive(Debug, Serialize)]
struct UiEventEnvelope {
    v: u8,
    seq: u64,
    at_ms: u64,
    #[serde(rename = "type")]
    event_type: String,
    payload: serde_json::Value,
}

fn run_event_bus(listener: UnixListener, rx: mpsc::Receiver<UiBusMessage>, socket_path: PathBuf) {
    let mut clients: Vec<UnixStream> = Vec::new();

    loop {
        while let Ok((stream, _)) = listener.accept() {
            let _ = stream.set_write_timeout(Some(UI_EVENT_CLIENT_WRITE_TIMEOUT));
            clients.push(stream);
        }

        match rx.recv_timeout(UI_EVENT_SOCKET_LOOP_TICK) {
            Ok(UiBusMessage::Publish(event)) => {
                let Ok(body) = serde_json::to_string(&event) else {
                    continue;
                };
                let line = format!("{body}\n");
                broadcast_line(&mut clients, &line);
            }
            Ok(UiBusMessage::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }

    let _ = fs::remove_file(&socket_path);
}

fn broadcast_line(clients: &mut Vec<UnixStream>, line: &str) {
    let mut i = 0;
    while i < clients.len() {
        let ok = clients[i].write_all(line.as_bytes()).is_ok();
        if ok {
            i += 1;
        } else {
            clients.swap_remove(i);
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

static GLOBAL_PUBLISHER: OnceLock<Mutex<Option<UiEventPublisher>>> = OnceLock::new();

fn publisher_slot() -> &'static Mutex<Option<UiEventPublisher>> {
    GLOBAL_PUBLISHER.get_or_init(|| Mutex::new(None))
}

pub fn install_global_publisher(publisher: UiEventPublisher) {
    if let Ok(mut slot) = publisher_slot().lock() {
        *slot = Some(publisher);
    }
}

pub fn clear_global_publisher() {
    if let Ok(mut slot) = publisher_slot().lock() {
        *slot = None;
    }
}

pub fn emit_event(event_type: &str, payload: serde_json::Value) {
    let publisher = publisher_slot().lock().ok().and_then(|slot| slot.clone());

    if let Some(publisher) = publisher {
        publisher.emit(event_type, payload);
    }
}
