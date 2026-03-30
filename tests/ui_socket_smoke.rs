use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct EnvGuard {
    runtime_dir: PathBuf,
    config_path: PathBuf,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        let _ = run_omni_raw(
            &self.runtime_dir,
            &self.config_path,
            &["stop", "--json"],
            &[],
        );
        let _ = fs::remove_file(self.runtime_dir.join("daemon.sock"));
        let _ = fs::remove_file(self.runtime_dir.join("ui.sock"));
        let _ = fs::remove_file(self.runtime_dir.join("daemon.pid"));
        let _ = fs::remove_dir_all(&self.runtime_dir);

        let _ = fs::remove_file(&self.config_path);
        if let Some(parent) = self.config_path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}

#[test]
fn ui_socket_streams_hook_and_lifecycle_events() {
    let runtime_dir = unique_temp_path("oui");
    let config_path = unique_temp_path("oic").join("config.toml");

    fs::create_dir_all(&runtime_dir).expect("runtime dir should be creatable");
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).expect("config dir should be creatable");
    }

    write_test_config(&config_path);

    let _guard = EnvGuard {
        runtime_dir: runtime_dir.clone(),
        config_path: config_path.clone(),
    };

    let _ = run_omni_json(&runtime_dir, &config_path, &["stop", "--json"], &[]);

    let start = run_omni_json(
        &runtime_dir,
        &config_path,
        &["start", "--json"],
        &[("OMNI_TEST_TRANSCRIPT", "ui-socket-smoke")],
    );
    assert_eq!(start.get("ok").and_then(|v| v.as_bool()), Some(true));

    let socket_path = runtime_dir.join("ui.sock");
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let collector = thread::spawn(move || collect_ui_event_lines(socket_path, ready_tx));

    ready_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("ui collector should connect in time");

    // Give the daemon UI event bus a brief window to accept and register the client stream.
    thread::sleep(Duration::from_millis(120));

    let transcribe_start = run_omni_json(
        &runtime_dir,
        &config_path,
        &["transcribe", "start", "--json"],
        &[],
    );
    assert_eq!(
        transcribe_start.get("recording").and_then(|v| v.as_bool()),
        Some(true)
    );

    let transcribe_stop = run_omni_json(
        &runtime_dir,
        &config_path,
        &["transcribe", "stop", "--json"],
        &[],
    );
    assert_eq!(
        transcribe_stop.get("recording").and_then(|v| v.as_bool()),
        Some(false)
    );

    let _ = run_omni_json(&runtime_dir, &config_path, &["stop", "--json"], &[]);

    let lines = collector
        .join()
        .expect("collector thread should join successfully");
    assert!(!lines.is_empty(), "expected ui events, got none");

    let mut event_types = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }

        let parsed: serde_json::Value =
            serde_json::from_str(&line).expect("event line should be JSON");
        let event_type = parsed
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        event_types.push(event_type);
    }

    assert!(
        event_types.iter().any(|kind| kind == "ui.show"),
        "event types: {event_types:?}"
    );
    assert!(
        event_types.iter().any(|kind| kind == "transcribe.started"),
        "event types: {event_types:?}"
    );
    assert!(
        event_types.iter().any(|kind| kind == "ui.hide"),
        "event types: {event_types:?}"
    );
    assert!(
        event_types.iter().any(|kind| kind == "transcribe.stopped"),
        "event types: {event_types:?}"
    );
}

fn collect_ui_event_lines(socket_path: PathBuf, ready_tx: mpsc::Sender<()>) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let stream = loop {
        match UnixStream::connect(&socket_path) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            Err(error) => panic!(
                "failed connecting ui socket {}: {error}",
                socket_path.display()
            ),
        }
    };

    stream
        .set_read_timeout(Some(Duration::from_secs(4)))
        .expect("collector should set read timeout");

    let _ = ready_tx.send(());

    let mut reader = BufReader::new(stream);
    let mut lines = Vec::new();
    let end = Instant::now() + Duration::from_secs(4);

    while Instant::now() < end {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => lines.push(line),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => panic!("failed reading ui socket events: {error}"),
        }
    }

    lines
}

fn write_test_config(path: &Path) {
    let content = r#"
[server]
llmApi = "openai-realtime"
baseUrl = "http://127.0.0.1:8000/v1"
apiKey = ""
model = "voxtral"

[audio]
device = "default"
sample_rate = 16000
channels = 1

[event.hooks.transcribe]
start = ["show_ui"]
stop = ["hide_ui"]
"#;

    fs::write(path, content).expect("should write test config");
}

fn unique_temp_path(prefix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after unix epoch")
        .as_nanos() as u64;
    std::env::temp_dir().join(format!(
        "{prefix}-{:x}-{:x}",
        std::process::id(),
        nonce & 0xffff_ffff
    ))
}

fn run_omni_json(
    runtime_dir: &Path,
    config_path: &Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> serde_json::Value {
    let output = run_omni_raw(runtime_dir, config_path, args, extra_env);

    assert!(
        output.status.success(),
        "omni {:?} failed\nstatus={}\nstdout={}\nstderr={}",
        args,
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON")
}

fn run_omni_raw(
    runtime_dir: &Path,
    config_path: &Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_omni"));
    command
        .args(args)
        .env("OMNI_RUNTIME_DIR", runtime_dir)
        .env("OMNI_CONFIG_FILE", config_path);

    for (key, value) in extra_env {
        command.env(key, value);
    }

    command.output().expect("failed to run omni command")
}
