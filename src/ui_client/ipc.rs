use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::ui_client::config::UiRuntimeConfig;
use crate::ui_client::model::{DaemonSnapshot, InboundMessage, UiEventEnvelope};

pub fn spawn_reader(cfg: UiRuntimeConfig, tx: mpsc::Sender<InboundMessage>) {
    thread::spawn(move || run_reader(cfg, tx));
}

fn run_reader(cfg: UiRuntimeConfig, tx: mpsc::Sender<InboundMessage>) {
    loop {
        if let Ok(snapshot) = query_daemon_snapshot(&cfg.daemon_socket) {
            let _ = tx.send(InboundMessage::Snapshot(snapshot));
        }

        let stream = match UnixStream::connect(&cfg.ui_socket) {
            Ok(stream) => stream,
            Err(_) => {
                thread::sleep(cfg.reconnect_delay);
                continue;
            }
        };

        let _ = stream.set_read_timeout(Some(Duration::from_millis(450)));
        let mut reader = BufReader::new(stream);

        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim_end();
                    if trimmed.is_empty() {
                        continue;
                    }

                    if let Ok(event) = serde_json::from_str::<UiEventEnvelope>(trimmed) {
                        let _ = tx.send(InboundMessage::UiEvent(event));
                    }
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => break,
            }
        }

        thread::sleep(cfg.reconnect_delay);
    }
}

pub fn query_daemon_snapshot(socket_path: &Path) -> Result<DaemonSnapshot> {
    let mut stream = UnixStream::connect(socket_path)
        .with_context(|| format!("failed connecting daemon socket {}", socket_path.display()))?;

    let body = serde_json::json!({
        "cmd": "transcribe_status",
        "mode": null,
    });

    stream
        .write_all(format!("{}\n", body).as_bytes())
        .context("failed writing daemon snapshot request")?;

    let mut response_line = String::new();
    let mut reader = BufReader::new(stream);
    reader
        .read_line(&mut response_line)
        .context("failed reading daemon snapshot response")?;

    let value: serde_json::Value =
        serde_json::from_str(response_line.trim_end()).context("invalid daemon snapshot JSON")?;

    Ok(DaemonSnapshot {
        running: value
            .get("running")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        recording: value
            .get("recording")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        transcript_preview: value
            .get("transcript_preview")
            .and_then(|v| v.as_str())
            .map(ToString::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn query_daemon_snapshot_reads_expected_fields() {
        let socket_path = unique_socket_path();
        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent).expect("socket parent dir should be creatable");
        }

        let listener = UnixListener::bind(&socket_path).expect("listener should bind");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("should accept client");

            let mut request = String::new();
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            reader
                .read_line(&mut request)
                .expect("should read request line");
            assert!(
                request.contains("transcribe_status"),
                "request should ask for transcribe_status: {request}"
            );

            stream
                .write_all(
                    br#"{"ok":true,"running":true,"recording":true,"transcript_preview":"hello"}
"#,
                )
                .expect("should write response");
        });

        let snapshot = query_daemon_snapshot(&socket_path).expect("snapshot query should succeed");
        assert!(snapshot.running);
        assert!(snapshot.recording);
        assert_eq!(snapshot.transcript_preview.as_deref(), Some("hello"));

        server.join().expect("server thread should join");
        let _ = fs::remove_file(socket_path);
    }

    fn unique_socket_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();

        std::env::temp_dir().join(format!(
            "omni-ui-ipc-test-{}-{}.sock",
            std::process::id(),
            nonce
        ))
    }
}
