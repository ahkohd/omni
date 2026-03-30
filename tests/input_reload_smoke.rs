use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct EnvGuard {
    runtime_dir: PathBuf,
    config_path: PathBuf,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.runtime_dir.join("daemon.sock"));
        let _ = std::fs::remove_file(&self.config_path);
        let _ = std::fs::remove_dir_all(&self.runtime_dir);

        if let Some(parent) = self.config_path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}

#[test]
fn input_set_auto_reloads_when_daemon_running() {
    let runtime_dir = unique_temp_path("oir");
    let config_path = unique_temp_path("oic").join("c.toml");

    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be creatable");
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).expect("config dir should be creatable");
    }

    let _guard = EnvGuard {
        runtime_dir: runtime_dir.clone(),
        config_path: config_path.clone(),
    };

    let socket_path = runtime_dir.join("daemon.sock");
    let seen_commands = Arc::new(Mutex::new(Vec::<String>::new()));
    let daemon = spawn_fake_daemon(socket_path.clone(), Arc::clone(&seen_commands));

    let set = run_omni_json(
        &runtime_dir,
        &config_path,
        &["input", "set", "default", "--json"],
        &[],
    );
    assert_eq!(set.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        set.get("daemonReloaded").and_then(|v| v.as_bool()),
        Some(true)
    );

    let configured = run_omni_json(
        &runtime_dir,
        &config_path,
        &["config", "get", "audio.device", "--json"],
        &[],
    );
    assert_eq!(configured.as_str(), Some("default"));

    daemon
        .join()
        .expect("fake daemon thread should finish without panic");

    let seen = seen_commands.lock().expect("lock should not be poisoned");
    assert!(
        seen.iter().any(|command| command == "ping"),
        "expected ping command, got {seen:?}"
    );
    assert!(
        seen.iter().any(|command| command == "reload"),
        "expected reload command, got {seen:?}"
    );
}

fn spawn_fake_daemon(
    socket_path: PathBuf,
    seen_commands: Arc<Mutex<Vec<String>>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }

        let listener =
            UnixListener::bind(&socket_path).expect("fake daemon should bind unix socket");
        listener
            .set_nonblocking(true)
            .expect("fake daemon should set non-blocking accept");

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((stream, _)) => {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    if reader
                        .read_line(&mut line)
                        .expect("fake daemon should read request")
                        == 0
                    {
                        continue;
                    }

                    let request: serde_json::Value = serde_json::from_str(line.trim())
                        .expect("fake daemon request should be JSON");
                    let command = request
                        .get("cmd")
                        .and_then(|value| value.as_str())
                        .unwrap_or("<unknown>")
                        .to_string();

                    if let Ok(mut seen) = seen_commands.lock() {
                        seen.push(command.clone());
                    }

                    let response = match command.as_str() {
                        "ping" => serde_json::json!({
                            "ok": true,
                            "running": true,
                            "recording": false,
                        }),
                        "reload" => serde_json::json!({
                            "ok": true,
                            "running": true,
                            "recording": false,
                            "message": "runtime config reloaded",
                        }),
                        _ => serde_json::json!({
                            "ok": false,
                            "running": true,
                            "recording": false,
                            "error": "unknown command",
                        }),
                    };

                    let mut stream = reader.into_inner();
                    writeln!(stream, "{}", response)
                        .expect("fake daemon should write response line");
                    stream.flush().expect("fake daemon should flush response");

                    if command == "reload" {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("fake daemon accept failed: {error}"),
            }
        }

        let _ = std::fs::remove_file(&socket_path);
    })
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

fn omni_bin_path() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join("omni")
}

fn run_omni_raw(
    runtime_dir: &Path,
    config_path: &Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> Output {
    let bin_path = omni_bin_path();

    let mut command = Command::new(&bin_path);
    command
        .args(args)
        .env("OMNI_RUNTIME_DIR", runtime_dir)
        .env("OMNI_CONFIG_FILE", config_path);

    for (key, value) in extra_env {
        command.env(key, value);
    }

    command
        .output()
        .unwrap_or_else(|error| panic!("failed to run {}: {error}", bin_path.display()))
}
