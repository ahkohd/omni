use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
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
fn show_ui_autospawns_transcribe_ui_client_when_configured() {
    let runtime_dir = unique_temp_path("oua-runtime");
    let config_path = unique_temp_path("oua-config").join("config.toml");
    let script_dir = unique_temp_path("oua-script");
    let marker_path = script_dir.join("spawned.marker");
    let script_path = script_dir.join("fake-omni-transcribe-ui.sh");

    fs::create_dir_all(&runtime_dir).expect("runtime dir should be creatable");
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).expect("config dir should be creatable");
    }
    fs::create_dir_all(&script_dir).expect("script dir should be creatable");

    write_test_config(&config_path);
    write_fake_ui_script(&script_path);

    let _guard = RuntimeGuard {
        runtime_dir: runtime_dir.clone(),
        config_path: config_path.clone(),
        script_dir: script_dir.clone(),
    };

    let _ = run_omni_json(&runtime_dir, &config_path, &["stop", "--json"], &[]);

    let start = run_omni_json(
        &runtime_dir,
        &config_path,
        &["start", "--json"],
        &[
            ("OMNI_TEST_TRANSCRIPT", "autospawn-smoke"),
            (
                "OMNI_TRANSCRIBE_UI_BIN",
                script_path
                    .to_str()
                    .expect("script path should be utf-8 for env"),
            ),
            (
                "OMNI_UI_SPAWN_MARKER",
                marker_path
                    .to_str()
                    .expect("marker path should be utf-8 for env"),
            ),
        ],
    );
    assert_eq!(start.get("ok").and_then(|v| v.as_bool()), Some(true));

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

    wait_for_marker(&marker_path, Duration::from_secs(3));

    let marker_contents = fs::read_to_string(&marker_path).expect("marker file should be readable");
    assert!(
        marker_contents.contains("spawned"),
        "marker contents should contain spawn signal: {marker_contents}"
    );

    let _ = run_omni_json(
        &runtime_dir,
        &config_path,
        &["transcribe", "stop", "--json"],
        &[],
    );
    let _ = run_omni_json(&runtime_dir, &config_path, &["stop", "--json"], &[]);
}

fn wait_for_marker(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() <= deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }

    panic!("marker file was not created in time: {}", path.display());
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

fn write_fake_ui_script(path: &Path) {
    let script = r#"#!/bin/sh
if [ -n "$OMNI_UI_SPAWN_MARKER" ]; then
  echo "spawned $(date +%s)" >> "$OMNI_UI_SPAWN_MARKER"
fi
exit 0
"#;

    fs::write(path, script).expect("should write fake ui script");

    let mut perms = fs::metadata(path)
        .expect("script metadata should be readable")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("script should be made executable");
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
