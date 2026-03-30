use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct RuntimeGuard {
    runtime_dir: PathBuf,
    config_path: PathBuf,
    script_dir: PathBuf,
}

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        let _ = run_omni_raw(
            &self.runtime_dir,
            &self.config_path,
            &["stop", "--json"],
            &[],
        );
        let _ = fs::remove_dir_all(&self.runtime_dir);
        let _ = fs::remove_file(&self.config_path);
        if let Some(parent) = self.config_path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
        let _ = fs::remove_dir_all(&self.script_dir);
    }
}

#[test]
fn stop_terminates_ui_client_process_from_pid_file() {
    let runtime_dir = unique_temp_path("oui-stop-runtime");
    let config_path = unique_temp_path("oui-stop-config").join("config.toml");
    let script_dir = unique_temp_path("oui-stop-script");
    let script_path = script_dir.join("omni-transcribe-ui");

    fs::create_dir_all(&runtime_dir).expect("runtime dir should be creatable");
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).expect("config dir should be creatable");
    }
    fs::create_dir_all(&script_dir).expect("script dir should be creatable");

    write_test_config(&config_path);
    write_sleep_script(&script_path);

    let _guard = RuntimeGuard {
        runtime_dir: runtime_dir.clone(),
        config_path: config_path.clone(),
        script_dir: script_dir.clone(),
    };

    let start = run_omni_json(&runtime_dir, &config_path, &["start", "--json"], &[]);
    assert_eq!(start.get("ok").and_then(|v| v.as_bool()), Some(true));

    let mut ui_child = Command::new(&script_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("fake ui process should spawn");

    let ui_pid = ui_child.id();
    let ui_pid_path = runtime_dir.join("transcribe-ui.pid");
    fs::write(&ui_pid_path, format!("{ui_pid}\n")).expect("pid file should be writable");

    let stop = run_omni_json(&runtime_dir, &config_path, &["stop", "--json"], &[]);
    assert_eq!(stop.get("ok").and_then(|v| v.as_bool()), Some(true));

    let exited = wait_for_child_exit(&mut ui_child, Duration::from_secs(3));
    assert!(exited, "expected ui child to exit after daemon stop");

    assert!(
        !ui_pid_path.exists(),
        "expected ui pid file to be removed after stop"
    );
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;

    while Instant::now() <= deadline {
        match child.try_wait() {
            Ok(Some(_status)) => return true,
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(_) => return false,
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    false
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

fn write_sleep_script(path: &Path) {
    let script = r#"#!/bin/sh
while true; do
  sleep 1
done
"#;

    fs::write(path, script).expect("should write fake ui script");

    let mut perms = fs::metadata(path)
        .expect("script metadata should be readable")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("script should be executable");
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
