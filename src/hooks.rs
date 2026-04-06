use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::clipboard::ClipboardSession;

const HOOK_TIMEOUT: Duration = Duration::from_secs(2);
const UI_CLIENT_OVERRIDE_ENV: &str = "OMNI_TRANSCRIBE_UI_BIN";
const UI_CLIENT_BINARY_NAME: &str = "omni-transcribe-ui";
const UI_CLIENT_PID_FILE: &str = "transcribe-ui.pid";
const HOOK_LOG_FILE: &str = "hooks.log";

#[derive(Debug, Clone)]
pub struct HookExecutionResult {
    pub event: String,
    pub actions_ran: Vec<String>,
}

pub fn run_transcribe_start_hooks_with_transcript(
    transcript: &str,
    json_output: bool,
) -> Result<HookExecutionResult> {
    run_event_hooks("transcribe", "start", None, transcript, json_output)
}

pub fn run_stop_hooks_with_transcript(
    mode: Option<&str>,
    transcript: &str,
    json_output: bool,
) -> Result<HookExecutionResult> {
    let event_name = match mode {
        Some(mode) => format!("stop_{mode}"),
        None => "stop".to_string(),
    };

    run_event_hooks("transcribe", &event_name, mode, transcript, json_output)
}

fn run_event_hooks(
    domain: &str,
    event_name: &str,
    mode: Option<&str>,
    transcript: &str,
    json_output: bool,
) -> Result<HookExecutionResult> {
    let config = crate::config::load_config()?;
    let actions = hook_actions_for_event(&config, domain, event_name)?;
    let event = format!("{domain}.{event_name}");

    if actions.is_empty() {
        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ok": true,
                    "event": event,
                    "actionsRan": [],
                    "message": "no hooks configured"
                }))?
            );
        } else {
            println!("no hooks configured for {event}");
        }

        return Ok(HookExecutionResult {
            event,
            actions_ran: Vec::new(),
        });
    }

    let report = execute_actions(&event, mode, transcript, actions)?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "event": report.event,
                "actionsRan": report.actions_ran,
            }))?
        );
    } else {
        println!(
            "ran {} hook action(s) for {event}",
            report.actions_ran.len()
        );
    }

    Ok(report)
}

pub fn validate_hook_config(config: &toml::Value) -> Result<()> {
    let Some(raw_hooks) = crate::config::get_value_by_key(config, "event.hooks") else {
        return Ok(());
    };

    let Some(domains) = raw_hooks.as_table() else {
        bail!("event.hooks must be a TOML table");
    };

    for (domain, raw_domain) in domains {
        let Some(events) = raw_domain.as_table() else {
            bail!("event.hooks.{domain} must be a TOML table (use event.hooks.{domain}.<event>)");
        };

        for (event_name, value) in events {
            match value {
                toml::Value::String(_) => {}
                toml::Value::Array(items) => {
                    for item in items {
                        if !item.is_str() {
                            bail!(
                                "event.hooks.{domain}.{event_name} must contain only string actions"
                            );
                        }
                    }
                }
                _ => {
                    bail!("event.hooks.{domain}.{event_name} must be a string or array of strings")
                }
            }
        }
    }

    Ok(())
}

fn hook_actions_for_event(
    config: &toml::Value,
    domain: &str,
    event_name: &str,
) -> Result<Vec<String>> {
    let key = format!("event.hooks.{domain}.{event_name}");
    let Some(raw) = crate::config::get_value_by_key(config, &key) else {
        return Ok(Vec::new());
    };

    match raw {
        toml::Value::String(v) => Ok(vec![v.clone()]),
        toml::Value::Array(items) => {
            let mut actions = Vec::with_capacity(items.len());
            for item in items {
                let Some(as_str) = item.as_str() else {
                    bail!("hook action for {domain}.{event_name} must be a string");
                };
                actions.push(as_str.to_string());
            }
            Ok(actions)
        }
        _ => bail!("hook value for {domain}.{event_name} must be a string or array of strings"),
    }
}

fn execute_actions(
    event: &str,
    mode: Option<&str>,
    transcript: &str,
    actions: Vec<String>,
) -> Result<HookExecutionResult> {
    let mut clipboard = ClipboardSession::default();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX epoch")?
        .as_millis()
        .to_string();

    for action in &actions {
        match action.as_str() {
            "show_ui" => {
                if let Err(error) = ensure_default_ui_client_running() {
                    let message = format!("omni show_ui: failed to start ui client: {error:#}");
                    eprintln!("{message}");
                    append_hook_log(&message);
                }

                crate::ui_ipc::emit_event(
                    "ui.show",
                    serde_json::json!({
                        "event": event,
                        "mode": mode,
                    }),
                );
            }
            "hide_ui" => {
                crate::ui_ipc::emit_event(
                    "ui.hide",
                    serde_json::json!({
                        "event": event,
                        "mode": mode,
                    }),
                );
            }
            "stash" | "copy" | "paste" | "unstash" => {
                clipboard.execute_builtin(action, transcript)?;
            }
            "restore" => {
                bail!("builtin action 'restore' was renamed to 'unstash'");
            }
            _ => {
                if let Some(duration) = parse_sleep_action(action)? {
                    thread::sleep(duration);
                } else {
                    run_external_action(action, event, mode, transcript, &timestamp)?;
                }
            }
        }
    }

    Ok(HookExecutionResult {
        event: event.to_string(),
        actions_ran: actions,
    })
}

fn parse_sleep_action(action: &str) -> Result<Option<Duration>> {
    let mut parts = action.split_whitespace();
    let Some(name) = parts.next() else {
        return Ok(None);
    };

    if name != "sleep" {
        return Ok(None);
    }

    let Some(raw_ms) = parts.next() else {
        bail!("builtin action '{name}' requires milliseconds (e.g. \"sleep 120\")");
    };

    if parts.next().is_some() {
        bail!("builtin action '{name}' accepts exactly one millisecond value");
    }

    let millis = raw_ms
        .parse::<u64>()
        .with_context(|| format!("invalid millisecond value for '{name}': {raw_ms}"))?;

    Ok(Some(Duration::from_millis(millis)))
}

fn ensure_default_ui_client_running() -> Result<()> {
    if let Ok(runtime_dir) = resolve_runtime_dir()
        && ui_client_is_running(&runtime_dir)
    {
        return Ok(());
    }

    if let Some(override_value) = std::env::var_os(UI_CLIENT_OVERRIDE_ENV) {
        let trimmed = override_value.to_string_lossy().trim().to_string();
        if !trimmed.is_empty() {
            return spawn_ui_client(&trimmed);
        }
    }

    let mut errors = Vec::new();

    if let Some(sibling) = current_exe_sibling(UI_CLIENT_BINARY_NAME) {
        match spawn_ui_client(&sibling) {
            Ok(()) => return Ok(()),
            Err(error) => errors.push(format!("{} ({error:#})", sibling.display())),
        }
    }

    match spawn_ui_client(UI_CLIENT_BINARY_NAME) {
        Ok(()) => return Ok(()),
        Err(error) => errors.push(format!("{UI_CLIENT_BINARY_NAME} ({error:#})")),
    }

    if let Some(manifest_dir) = find_manifest_dir() {
        match spawn_ui_client_with_args(
            "cargo",
            &["run", "--bin", UI_CLIENT_BINARY_NAME, "--"],
            Some(&manifest_dir),
        ) {
            Ok(()) => return Ok(()),
            Err(error) => errors.push(format!(
                "cargo run --bin {UI_CLIENT_BINARY_NAME} ({error:#})"
            )),
        }
    }

    bail!("ui client launch attempts failed: {}", errors.join("; "))
}

fn spawn_ui_client(program: impl AsRef<OsStr>) -> Result<()> {
    spawn_ui_client_with_args(program, &[], None)
}

fn spawn_ui_client_with_args(
    program: impl AsRef<OsStr>,
    args: &[&str],
    current_dir: Option<&Path>,
) -> Result<()> {
    let mut command = Command::new(program.as_ref());
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    if let Some(path) = current_dir {
        command.current_dir(path);
    }

    let mut child = command.spawn().context("failed spawning ui client")?;

    // Reap child when it exits to avoid leaving a zombie process under the daemon.
    thread::spawn(move || {
        let _ = child.wait();
    });

    Ok(())
}

fn find_manifest_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;

    for dir in cwd.ancestors() {
        let manifest = dir.join("Cargo.toml");
        if manifest.exists() {
            return Some(dir.to_path_buf());
        }
    }

    None
}

fn current_exe_sibling(binary_name: &str) -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let parent = current.parent()?;
    let sibling = parent.join(binary_name);
    if sibling.exists() {
        Some(sibling)
    } else {
        None
    }
}

fn append_hook_log(message: &str) {
    let Ok(runtime_dir) = resolve_runtime_dir() else {
        return;
    };

    let _ = fs::create_dir_all(&runtime_dir);
    let path = runtime_dir.join(HOOK_LOG_FILE);

    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);

    let _ = writeln!(file, "[{now_ms}] {message}");
}

fn resolve_runtime_dir() -> Result<PathBuf> {
    if let Ok(custom) = std::env::var("OMNI_RUNTIME_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    if let Ok(xdg_runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let trimmed = xdg_runtime_dir.trim();
        if !trimmed.is_empty() {
            return Ok(Path::new(trimmed).join("omni"));
        }
    }

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("failed to resolve home directory"))?;
    Ok(home.join(".local").join("state").join("omni"))
}

fn ui_client_pid_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(UI_CLIENT_PID_FILE)
}

fn ui_client_is_running(runtime_dir: &Path) -> bool {
    let pid_path = ui_client_pid_path(runtime_dir);
    let Some(pid) = read_ui_client_pid(&pid_path) else {
        let _ = fs::remove_file(&pid_path);
        return false;
    };

    if is_ui_client_process(pid) {
        return true;
    }

    let _ = fs::remove_file(&pid_path);
    false
}

fn read_ui_client_pid(path: &Path) -> Option<u32> {
    let raw = fs::read_to_string(path).ok()?;
    raw.trim().parse::<u32>().ok()
}

fn is_ui_client_process(pid: u32) -> bool {
    let Some((state, command)) = process_snapshot(pid) else {
        return false;
    };

    if state.contains('Z') {
        return false;
    }

    if command.contains(UI_CLIENT_BINARY_NAME) {
        return true;
    }

    let Some(override_value) = std::env::var_os(UI_CLIENT_OVERRIDE_ENV) else {
        return false;
    };

    let override_path = Path::new(override_value.as_os_str());
    let Some(file_name) = override_path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    !file_name.is_empty() && command.contains(file_name)
}

fn process_snapshot(pid: u32) -> Option<(String, String)> {
    let pid_text = pid.to_string();
    let output = Command::new("ps")
        .args(["-o", "stat=", "-o", "command=", "-p", pid_text.as_str()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let row = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if row.is_empty() {
        return None;
    }

    let mut parts = row.split_whitespace();
    let state = parts.next()?.to_string();
    let command = parts.collect::<Vec<_>>().join(" ");
    Some((state, command))
}

fn run_external_action(
    action: &str,
    event: &str,
    mode: Option<&str>,
    transcript: &str,
    timestamp: &str,
) -> Result<()> {
    let argv = shell_words::split(action)
        .with_context(|| format!("failed parsing hook command: {action}"))?;

    if argv.is_empty() {
        bail!("hook command is empty");
    }

    let mut command = Command::new(&argv[0]);
    if argv.len() > 1 {
        command.args(&argv[1..]);
    }

    command.env("OMNI_EVENT", event);
    command.env("OMNI_MODE", mode.unwrap_or(""));
    command.env("OMNI_TRANSCRIPT", transcript);
    command.env("OMNI_TIMESTAMP", timestamp);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed spawning hook command: {action}"))?;

    let deadline = Instant::now() + HOOK_TIMEOUT;
    loop {
        if let Some(status) = child
            .try_wait()
            .context("failed waiting for hook command")?
        {
            if !status.success() {
                bail!("hook command failed ({action}) with status {status}");
            }
            return Ok(());
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            bail!("hook command timed out after {:?}: {action}", HOOK_TIMEOUT);
        }

        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_hook_config_accepts_string_or_string_array() {
        let mut config = crate::config::default_config();
        crate::config::set_value_by_key(
            &mut config,
            "event.hooks.transcribe.stop_copy",
            toml::Value::Array(vec![toml::Value::String("copy".into())]),
        )
        .expect("set should work");

        validate_hook_config(&config).expect("hook config should validate");
    }

    #[test]
    fn validate_hook_config_rejects_non_string_actions() {
        let mut config = crate::config::default_config();
        crate::config::set_value_by_key(
            &mut config,
            "event.hooks.transcribe.stop_copy",
            toml::Value::Array(vec![toml::Value::Integer(1)]),
        )
        .expect("set should work");

        assert!(validate_hook_config(&config).is_err());
    }

    #[test]
    fn validate_hook_config_rejects_flat_legacy_event_keys() {
        let mut config = crate::config::default_config();
        crate::config::set_value_by_key(
            &mut config,
            "event.hooks.stop_copy",
            toml::Value::Array(vec![toml::Value::String("copy".into())]),
        )
        .expect("set should work");

        assert!(validate_hook_config(&config).is_err());
    }

    #[test]
    fn parse_sleep_action_supports_builtin_sleep() {
        let duration = parse_sleep_action("sleep 120")
            .expect("sleep action should parse")
            .expect("sleep action should be recognized");

        assert_eq!(duration, Duration::from_millis(120));
    }

    #[test]
    fn parse_sleep_action_ignores_non_sleep_actions() {
        assert!(
            parse_sleep_action("copy")
                .expect("copy should not fail")
                .is_none()
        );
    }

    #[test]
    fn parse_sleep_action_rejects_missing_or_invalid_values() {
        assert!(parse_sleep_action("sleep").is_err());
        assert!(parse_sleep_action("sleep nope").is_err());
        assert!(parse_sleep_action("sleep 10 extra").is_err());
    }
}
