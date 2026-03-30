use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct RuntimeGuard {
    runtime_dir: PathBuf,
    config_path: PathBuf,
}

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        let _ = run_omni_raw(
            &self.runtime_dir,
            &self.config_path,
            &["stop", "--json"],
            &[],
        );
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
        let _ = std::fs::remove_file(&self.config_path);
        if let Some(parent) = self.config_path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}

#[test]
fn synthetic_transcribe_lifecycle_smoke() {
    let runtime_dir = unique_runtime_dir();
    let config_path = unique_config_path();
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be creatable");
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).expect("config dir should be creatable");
    }

    let _guard = RuntimeGuard {
        runtime_dir: runtime_dir.clone(),
        config_path: config_path.clone(),
    };

    let _ = run_omni_json(&runtime_dir, &config_path, &["stop", "--json"], &[]);

    let start = run_omni_json(
        &runtime_dir,
        &config_path,
        &["transcribe", "start", "--json"],
        &[("OMNI_TEST_TRANSCRIPT", "integration transcript")],
    );
    assert_eq!(start.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(start.get("recording").and_then(|v| v.as_bool()), Some(true));

    let status = run_omni_json(
        &runtime_dir,
        &config_path,
        &["transcribe", "status", "--json"],
        &[],
    );
    assert_eq!(status.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        status.get("recording").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        status.get("transcript_preview").and_then(|v| v.as_str()),
        Some("integration transcript")
    );
    assert!(
        status
            .get("duration_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or_default()
            > 0
    );

    let daemon_status = run_omni_json(&runtime_dir, &config_path, &["status", "--json"], &[]);
    assert_eq!(
        daemon_status.get("ok").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        daemon_status.get("running").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        daemon_status
            .get("transcript_preview")
            .and_then(|v| v.as_str()),
        Some("integration transcript")
    );

    let stop = run_omni_json(
        &runtime_dir,
        &config_path,
        &["transcribe", "stop", "insert", "--json"],
        &[],
    );
    assert_eq!(stop.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(stop.get("recording").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        stop.get("transcript").and_then(|v| v.as_str()),
        Some("integration transcript")
    );

    let daemon_stop = run_omni_json(&runtime_dir, &config_path, &["stop", "--json"], &[]);
    assert_eq!(daemon_stop.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        daemon_stop.get("running").and_then(|v| v.as_bool()),
        Some(false)
    );
}

fn unique_runtime_dir() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("omni-smoke-{}-{nonce}", std::process::id()))
}

fn unique_config_path() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "omni-smoke-config-{}-{nonce}.toml",
        std::process::id()
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
