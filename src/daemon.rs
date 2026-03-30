use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::backend::{self, RealtimeTransport};
use crate::{config, hooks, recording, ui_ipc};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const REALTIME_COMMIT_INTERVAL: Duration = Duration::from_millis(450);
const REALTIME_LOOP_RECV_TIMEOUT: Duration = Duration::from_millis(35);
const REALTIME_EVENT_DRAIN_TIMEOUT: Duration = Duration::from_millis(10);
const REALTIME_DEBUG_EVENT_BUFFER: usize = 256;
const REALTIME_AUDIO_SILENCE_THRESHOLD: f32 = 0.008;
const UI_CLIENT_PID_FILE: &str = "transcribe-ui.pid";
const UI_SHUTDOWN_GRACE: Duration = Duration::from_millis(700);
const UI_SHUTDOWN_POLL: Duration = Duration::from_millis(30);

#[derive(Debug, Clone)]
pub struct RuntimePaths {
    pub runtime_dir: PathBuf,
    pub socket_path: PathBuf,
    pub ui_socket_path: PathBuf,
    pub pid_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct DaemonRequest {
    cmd: String,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub ok: bool,
    pub running: bool,
    pub recording: bool,
    pub pid: Option<u32>,
    pub message: Option<String>,
    pub transcript: Option<String>,
    pub transcript_preview: Option<String>,
    pub transcript_updated_at_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub debug: Option<RealtimeDebugInfo>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealtimeDebugEvent {
    pub seq: u64,
    pub event_type: String,
    pub at_ms: u64,
    pub text_fragment: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub error_event_id: Option<String>,
    pub raw_excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealtimeAudioDebugInfo {
    pub chunks_observed: u64,
    pub samples_observed: u64,
    pub last_rms: f32,
    pub avg_rms: f32,
    pub last_peak: f32,
    pub max_peak: f32,
    pub silent_chunks: u64,
    pub silence_threshold: f32,
}

impl Default for RealtimeAudioDebugInfo {
    fn default() -> Self {
        Self {
            chunks_observed: 0,
            samples_observed: 0,
            last_rms: 0.0,
            avg_rms: 0.0,
            last_peak: 0.0,
            max_peak: 0.0,
            silent_chunks: 0,
            silence_threshold: REALTIME_AUDIO_SILENCE_THRESHOLD,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RealtimeDebugInfo {
    pub chunks_sent: u64,
    pub commits_sent: u64,
    pub events_received: u64,
    pub errors_received: u64,
    pub last_event_type: Option<String>,
    pub last_event_at_ms: Option<u64>,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
    pub last_error_event_id: Option<String>,
    pub recent_events: Vec<RealtimeDebugEvent>,
    pub audio: RealtimeAudioDebugInfo,
    pub transcript_source: Option<String>,
    pub transcript_fallback_attempted: bool,
    pub transcript_fallback_used: bool,
    pub transcript_fallback_error: Option<String>,
}

#[derive(Default)]
struct RealtimeProgress {
    transcript_preview: String,
    last_non_empty_transcript: Option<String>,
    updated_at_ms: Option<u64>,
    debug: RealtimeDebugInfo,
}

type SharedRealtimeProgress = Arc<Mutex<RealtimeProgress>>;

enum ActiveRecording {
    Realtime {
        recorder: recording::Recorder,
        worker: thread::JoinHandle<Result<String>>,
        progress: SharedRealtimeProgress,
        backend: backend::OpenAiRealtimeBackend,
        started_at: Instant,
    },
    Synthetic {
        transcript: String,
        duration_ms: u64,
        started_at: Instant,
    },
}

#[derive(Default)]
struct DaemonState {
    recording: Option<ActiveRecording>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscriptResolutionSource {
    Realtime,
    Progress,
    Batch,
    EmptyNoAudio,
    Empty,
    Synthetic,
}

impl TranscriptResolutionSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Realtime => "realtime",
            Self::Progress => "progress",
            Self::Batch => "batch",
            Self::EmptyNoAudio => "empty_no_audio",
            Self::Empty => "empty",
            Self::Synthetic => "synthetic",
        }
    }
}

#[derive(Debug, Clone)]
struct StopTranscriptResolution {
    transcript: String,
    source: TranscriptResolutionSource,
    fallback_attempted: bool,
    fallback_used: bool,
    fallback_error: Option<String>,
}

pub fn start(json_output: bool) -> Result<()> {
    if is_running() {
        let status = status_response()?;
        return print_start_result(json_output, status.pid.unwrap_or_default(), true);
    }

    spawn_daemon_process()?;

    let started = wait_for_running(STARTUP_TIMEOUT);
    if !started {
        return Err(anyhow!("daemon did not start within timeout"));
    }

    let status = status_response()?;
    print_start_result(json_output, status.pid.unwrap_or_default(), false)
}

pub fn stop(json_output: bool) -> Result<()> {
    let paths = runtime_paths()?;

    if !is_running() {
        stop_ui_client_for_runtime(&paths.runtime_dir);

        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ok": true,
                    "running": false,
                    "recording": false,
                    "message": "daemon already stopped"
                }))?
            );
        } else {
            println!("daemon already stopped");
        }
        return Ok(());
    }

    let _ = send_command("stop", None);

    let stopped = wait_for_stopped(SHUTDOWN_TIMEOUT);
    if !stopped {
        return Err(anyhow!("daemon did not stop within timeout"));
    }

    // Daemon shutdown path should already terminate UI client; do a final best-effort
    // cleanup from the CLI side in case daemon exited unexpectedly.
    stop_ui_client_for_runtime(&paths.runtime_dir);

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "running": false,
                "recording": false,
                "message": "daemon stopped"
            }))?
        );
    } else {
        println!("daemon stopped");
    }

    Ok(())
}

pub fn status(json_output: bool) -> Result<()> {
    let status = status_response()?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else if status.running {
        if status.recording {
            if let Some(preview) = status.transcript_preview.as_deref() {
                println!(
                    "running (pid={}) recording=true duration_ms={} transcript_preview={}",
                    status.pid.unwrap_or_default(),
                    status.duration_ms.unwrap_or(0),
                    preview,
                );
            } else {
                println!(
                    "running (pid={}) recording=true duration_ms={}",
                    status.pid.unwrap_or_default(),
                    status.duration_ms.unwrap_or(0),
                );
            }
        } else {
            println!("running (pid={})", status.pid.unwrap_or_default());
        }
    } else {
        println!("stopped");
    }

    Ok(())
}

pub fn reload_runtime_if_running() -> Result<Option<DaemonResponse>> {
    if !is_running() {
        return Ok(None);
    }

    let response = send_command("reload", None)?;
    if !response.ok {
        bail!(
            response
                .error
                .unwrap_or_else(|| "reload failed".to_string())
        );
    }

    Ok(Some(response))
}

pub fn reload(json_output: bool) -> Result<()> {
    if let Some(response) = reload_runtime_if_running()? {
        if json_output {
            println!("{}", serde_json::to_string_pretty(&response)?);
        } else {
            println!(
                "{}",
                response
                    .message
                    .unwrap_or_else(|| "runtime config reloaded".to_string())
            );
        }
        return Ok(());
    }

    validate_runtime_config()?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "running": false,
                "recording": false,
                "message": "runtime config validated (daemon not running)",
            }))?
        );
    } else {
        println!("runtime config validated (daemon not running)");
    }

    Ok(())
}

pub fn transcribe_status(json_output: bool) -> Result<()> {
    if !is_running() {
        let response = DaemonResponse {
            ok: true,
            running: false,
            recording: false,
            pid: None,
            message: Some("daemon is not running".into()),
            transcript: None,
            transcript_preview: None,
            transcript_updated_at_ms: None,
            duration_ms: Some(0),
            debug: None,
            error: None,
        };

        if json_output {
            println!("{}", serde_json::to_string_pretty(&response)?);
        } else {
            println!("recording=false duration_ms=0");
        }
        return Ok(());
    }

    let response = send_command("transcribe_status", None)?;
    if !response.ok {
        bail!(
            response
                .error
                .unwrap_or_else(|| "transcribe_status failed".to_string())
        );
    }

    if json_output {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        if let Some(preview) = response.transcript_preview.as_deref() {
            println!(
                "recording={} duration_ms={} transcript_preview={}",
                response.recording,
                response.duration_ms.unwrap_or(0),
                preview
            );
        } else {
            println!(
                "recording={} duration_ms={}",
                response.recording,
                response.duration_ms.unwrap_or(0)
            );
        }
    }

    Ok(())
}

pub fn transcribe_start(
    background: bool,
    debug: bool,
    debug_json: bool,
    json_output: bool,
) -> Result<()> {
    if (debug || debug_json) && (background || json_output) {
        bail!("--debug/--debug-json require attached live mode (omit --background/--json)");
    }

    if !is_running() {
        spawn_daemon_process()?;
        if !wait_for_running(STARTUP_TIMEOUT) {
            bail!("daemon did not start within timeout");
        }
    }

    let response = send_command("transcribe_start", None)?;
    if !response.ok {
        bail!(
            response
                .error
                .unwrap_or_else(|| "transcribe_start failed".to_string())
        );
    }

    if json_output {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!(
            "{}",
            response
                .message
                .unwrap_or_else(|| "recording started".to_string())
        );
    }

    let live = !background && !json_output;
    if live {
        stream_live_recording_preview(debug, debug_json)?;
    }

    Ok(())
}

fn stream_live_recording_preview(debug: bool, debug_json: bool) -> Result<()> {
    println!("live preview attached (stop with: omni transcribe stop)");

    let mut last_preview = String::new();
    let mut last_debug_signature = String::new();
    let mut last_debug_event_seq = 0_u64;
    let mut last_debug_audio_signature = String::new();

    loop {
        let response = send_command("transcribe_status", None)?;
        if !response.ok {
            bail!(
                response
                    .error
                    .unwrap_or_else(|| "transcribe_status failed in live mode".to_string())
            );
        }

        if let Some(preview) = response.transcript_preview.as_deref() {
            render_preview_delta(&mut last_preview, preview)?;
        }

        if debug {
            maybe_print_live_debug(&response, &mut last_debug_signature);
        }
        if debug_json {
            maybe_print_live_debug_json(
                &response,
                &mut last_debug_event_seq,
                &mut last_debug_audio_signature,
            );
        }

        if !response.recording {
            break;
        }

        thread::sleep(Duration::from_millis(120));
    }

    if !last_preview.is_empty() {
        println!();
    }

    println!("live preview ended");
    Ok(())
}

fn maybe_print_live_debug(response: &DaemonResponse, last_signature: &mut String) {
    let Some(debug) = response.debug.as_ref() else {
        return;
    };

    let silent_pct = if debug.audio.chunks_observed == 0 {
        0.0
    } else {
        (debug.audio.silent_chunks as f32 * 100.0) / debug.audio.chunks_observed as f32
    };

    let signature = format!(
        "chunks={} commits={} events={} errors={} mic_rms_db={:.1} mic_peak_db={:.1} silence={:.0}% last={} last_error={}",
        debug.chunks_sent,
        debug.commits_sent,
        debug.events_received,
        debug.errors_received,
        normalized_level_to_dbfs(debug.audio.last_rms),
        normalized_level_to_dbfs(debug.audio.last_peak),
        silent_pct,
        debug.last_event_type.as_deref().unwrap_or("<none>"),
        debug.last_error_message.as_deref().unwrap_or("<none>"),
    );

    if &signature == last_signature {
        return;
    }

    eprintln!("[live-debug] {signature}");
    *last_signature = signature;
}

fn maybe_print_live_debug_json(
    response: &DaemonResponse,
    last_event_seq: &mut u64,
    last_audio_signature: &mut String,
) {
    let Some(debug) = response.debug.as_ref() else {
        return;
    };

    for event in &debug.recent_events {
        if event.seq <= *last_event_seq {
            continue;
        }

        let mut payload = serde_json::json!({
            "kind": "realtime_event",
            "seq": event.seq,
            "type": event.event_type,
            "at_ms": event.at_ms,
        });

        if let Some(text_fragment) = event.text_fragment.as_deref() {
            payload["text_fragment"] = serde_json::json!(text_fragment);
        }
        if let Some(error_code) = event.error_code.as_deref() {
            payload["error_code"] = serde_json::json!(error_code);
        }
        if let Some(error_message) = event.error_message.as_deref() {
            payload["error_message"] = serde_json::json!(error_message);
        }
        if let Some(error_event_id) = event.error_event_id.as_deref() {
            payload["error_event_id"] = serde_json::json!(error_event_id);
        }
        if let Some(raw_excerpt) = event.raw_excerpt.as_deref() {
            payload["raw_excerpt"] = serde_json::json!(raw_excerpt);
        }

        eprintln!("{payload}");
        *last_event_seq = event.seq;
    }

    let audio = &debug.audio;
    let audio_signature = format!(
        "{}:{}:{:.4}:{:.4}:{:.4}:{}",
        audio.chunks_observed,
        audio.samples_observed,
        audio.last_rms,
        audio.last_peak,
        audio.avg_rms,
        audio.silent_chunks,
    );

    if &audio_signature != last_audio_signature {
        let silent_pct = if audio.chunks_observed == 0 {
            0.0
        } else {
            (audio.silent_chunks as f32 * 100.0) / audio.chunks_observed as f32
        };

        let payload = serde_json::json!({
            "kind": "audio_meter",
            "chunks": audio.chunks_observed,
            "samples": audio.samples_observed,
            "last_rms": audio.last_rms,
            "avg_rms": audio.avg_rms,
            "last_peak": audio.last_peak,
            "max_peak": audio.max_peak,
            "last_rms_dbfs": normalized_level_to_dbfs(audio.last_rms),
            "last_peak_dbfs": normalized_level_to_dbfs(audio.last_peak),
            "silent_chunks": audio.silent_chunks,
            "silence_pct": silent_pct,
            "silence_threshold": audio.silence_threshold,
        });

        eprintln!("{payload}");
        *last_audio_signature = audio_signature;
    }
}

fn render_preview_delta(last_preview: &mut String, next_preview: &str) -> Result<()> {
    if next_preview == *last_preview {
        return Ok(());
    }

    if next_preview.starts_with(last_preview.as_str()) {
        let delta = &next_preview[last_preview.len()..];
        if !delta.is_empty() {
            print!("{delta}");
            std::io::stdout()
                .flush()
                .context("failed flushing live preview")?;
        }
    } else {
        if !last_preview.is_empty() {
            println!();
        }
        print!("{next_preview}");
        std::io::stdout()
            .flush()
            .context("failed flushing live preview")?;
    }

    *last_preview = next_preview.to_string();
    Ok(())
}

pub fn transcribe_stop(mode: Option<String>, json_output: bool) -> Result<()> {
    if !is_running() {
        let response = DaemonResponse {
            ok: true,
            running: false,
            recording: false,
            pid: None,
            message: Some("recording not active (daemon not running)".into()),
            transcript: Some(String::new()),
            transcript_preview: None,
            transcript_updated_at_ms: None,
            duration_ms: Some(0),
            debug: None,
            error: None,
        };

        if json_output {
            println!("{}", serde_json::to_string_pretty(&response)?);
        } else {
            println!(
                "{}",
                response
                    .message
                    .unwrap_or_else(|| "recording not active".to_string())
            );
        }
        return Ok(());
    }

    let response = send_command("transcribe_stop", mode.clone())?;
    if !response.ok {
        bail!(
            response
                .error
                .unwrap_or_else(|| "transcribe_stop failed".to_string())
        );
    }

    if json_output {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        if let Some(message) = response.message {
            println!("{message}");
        }

        if let Some(transcript) = response.transcript {
            if !transcript.is_empty() {
                println!("{transcript}");
            }
        }
    }

    Ok(())
}

pub fn run_foreground() -> Result<()> {
    let paths = runtime_paths()?;
    ensure_runtime_dir(&paths.runtime_dir)?;

    if paths.socket_path.exists() {
        let _ = fs::remove_file(&paths.socket_path);
    }
    if paths.ui_socket_path.exists() {
        let _ = fs::remove_file(&paths.ui_socket_path);
    }

    let listener = UnixListener::bind(&paths.socket_path)
        .with_context(|| format!("failed binding socket at {}", paths.socket_path.display()))?;

    let pid = std::process::id();
    fs::write(&paths.pid_path, format!("{pid}\n"))
        .with_context(|| format!("failed writing pid file {}", paths.pid_path.display()))?;

    let mut ui_runtime = ui_ipc::UiEventRuntime::start(paths.ui_socket_path.clone())?;
    ui_ipc::install_global_publisher(ui_runtime.publisher());

    let mut should_stop = false;
    let mut state = DaemonState::default();

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle_client(stream, pid, &mut should_stop, &mut state) {
                    eprintln!("omni daemon client error: {err:#}");
                }

                if should_stop {
                    break;
                }
            }
            Err(err) => {
                eprintln!("omni daemon socket accept error: {err}");
                break;
            }
        }
    }

    if let Some(active) = state.recording.take() {
        let _ = cleanup_active_recording(active);
    }

    ui_ipc::clear_global_publisher();
    ui_runtime.shutdown();

    stop_ui_client_for_runtime(&paths.runtime_dir);
    cleanup_runtime_files(&paths)?;
    Ok(())
}

pub fn runtime_paths() -> Result<RuntimePaths> {
    let runtime_dir = if let Ok(custom) = std::env::var("OMNI_RUNTIME_DIR") {
        let trimmed = custom.trim();
        if trimmed.is_empty() {
            default_runtime_dir()?
        } else {
            PathBuf::from(trimmed)
        }
    } else {
        default_runtime_dir()?
    };

    Ok(RuntimePaths {
        socket_path: runtime_dir.join("daemon.sock"),
        ui_socket_path: runtime_dir.join("ui.sock"),
        pid_path: runtime_dir.join("daemon.pid"),
        runtime_dir,
    })
}

fn spawn_daemon_process() -> Result<()> {
    let paths = runtime_paths()?;
    ensure_runtime_dir(&paths.runtime_dir)?;

    if paths.socket_path.exists() {
        let _ = fs::remove_file(&paths.socket_path);
    }

    let exe = std::env::current_exe().context("failed to determine current executable path")?;
    Command::new(exe)
        .arg("__daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn daemon process")?;

    Ok(())
}

fn default_runtime_dir() -> Result<PathBuf> {
    if let Ok(xdg_runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let trimmed = xdg_runtime_dir.trim();
        if !trimmed.is_empty() {
            return Ok(Path::new(trimmed).join("omni"));
        }
    }

    let home = dirs::home_dir().ok_or_else(|| anyhow!("failed to resolve home directory"))?;
    Ok(home.join(".local").join("state").join("omni"))
}

fn ensure_runtime_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed creating runtime directory {}", path.display()))
}

fn cleanup_runtime_files(paths: &RuntimePaths) -> Result<()> {
    if paths.socket_path.exists() {
        fs::remove_file(&paths.socket_path)
            .with_context(|| format!("failed removing socket {}", paths.socket_path.display()))?;
    }

    if paths.ui_socket_path.exists() {
        fs::remove_file(&paths.ui_socket_path).with_context(|| {
            format!(
                "failed removing UI socket {}",
                paths.ui_socket_path.display()
            )
        })?;
    }

    if paths.pid_path.exists() {
        fs::remove_file(&paths.pid_path)
            .with_context(|| format!("failed removing pid file {}", paths.pid_path.display()))?;
    }

    let ui_pid_path = ui_client_pid_path(&paths.runtime_dir);
    if ui_pid_path.exists() {
        let _ = fs::remove_file(&ui_pid_path);
    }

    Ok(())
}

fn ui_client_pid_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(UI_CLIENT_PID_FILE)
}

fn stop_ui_client_for_runtime(runtime_dir: &Path) {
    let pid_path = ui_client_pid_path(runtime_dir);
    let Some(pid) = read_ui_client_pid(&pid_path) else {
        let _ = fs::remove_file(&pid_path);
        return;
    };

    if !is_ui_client_process(pid) {
        let _ = fs::remove_file(&pid_path);
        return;
    }

    let _ = send_signal_to_pid(pid, "-TERM");

    let deadline = Instant::now() + UI_SHUTDOWN_GRACE;
    while Instant::now() <= deadline {
        if !process_is_alive_non_zombie(pid) {
            let _ = fs::remove_file(&pid_path);
            return;
        }
        thread::sleep(UI_SHUTDOWN_POLL);
    }

    let _ = send_signal_to_pid(pid, "-KILL");

    let kill_deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() <= kill_deadline {
        if !process_is_alive_non_zombie(pid) {
            let _ = fs::remove_file(&pid_path);
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn read_ui_client_pid(path: &Path) -> Option<u32> {
    let raw = fs::read_to_string(path).ok()?;
    raw.trim().parse::<u32>().ok()
}

fn send_signal_to_pid(pid: u32, signal: &str) -> bool {
    Command::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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

fn process_is_alive_non_zombie(pid: u32) -> bool {
    if let Some((state, _)) = process_snapshot(pid) {
        return !state.contains('Z');
    }

    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn is_ui_client_process(pid: u32) -> bool {
    if let Some((state, command)) = process_snapshot(pid) {
        return !state.contains('Z') && command.contains("omni-transcribe-ui");
    }

    false
}

fn handle_client(
    mut stream: UnixStream,
    pid: u32,
    should_stop: &mut bool,
    state: &mut DaemonState,
) -> Result<()> {
    let mut line = String::new();
    let mut reader = BufReader::new(stream.try_clone().context("failed cloning unix stream")?);
    reader
        .read_line(&mut line)
        .context("failed reading request line")?;

    let request: DaemonRequest =
        serde_json::from_str(line.trim_end()).context("failed parsing daemon request JSON")?;

    let response = match dispatch_command(request, pid, should_stop, state) {
        Ok(response) => response,
        Err(error) => DaemonResponse {
            ok: false,
            running: true,
            recording: state.recording.is_some(),
            pid: Some(pid),
            message: None,
            transcript: None,
            transcript_preview: None,
            transcript_updated_at_ms: None,
            duration_ms: None,
            debug: None,
            error: Some(error.to_string()),
        },
    };

    let body = serde_json::to_string(&response)?;
    stream
        .write_all(format!("{body}\n").as_bytes())
        .context("failed writing daemon response")?;

    Ok(())
}

fn dispatch_command(
    request: DaemonRequest,
    pid: u32,
    should_stop: &mut bool,
    state: &mut DaemonState,
) -> Result<DaemonResponse> {
    match request.cmd.as_str() {
        "ping" => Ok(DaemonResponse {
            ok: true,
            running: true,
            recording: state.recording.is_some(),
            pid: Some(pid),
            message: None,
            transcript: None,
            transcript_preview: None,
            transcript_updated_at_ms: None,
            duration_ms: None,
            debug: None,
            error: None,
        }),
        "status" => handle_status(pid, state),
        "stop" => {
            if let Some(active) = state.recording.take() {
                let _ = cleanup_active_recording(active);
            }

            *should_stop = true;
            Ok(DaemonResponse {
                ok: true,
                running: false,
                recording: false,
                pid: Some(pid),
                message: Some("stopping".into()),
                transcript: None,
                transcript_preview: None,
                transcript_updated_at_ms: None,
                duration_ms: None,
                debug: None,
                error: None,
            })
        }
        "transcribe_start" => handle_transcribe_start(pid, state),
        "transcribe_status" => handle_transcribe_status(pid, state),
        "transcribe_stop" => handle_transcribe_stop(pid, state, request.mode.as_deref()),
        "reload" => handle_reload(pid, state),
        other => bail!("unknown command: {other}"),
    }
}

fn handle_transcribe_start(pid: u32, state: &mut DaemonState) -> Result<DaemonResponse> {
    if state.recording.is_some() {
        return Ok(DaemonResponse {
            ok: true,
            running: true,
            recording: true,
            pid: Some(pid),
            message: Some("recording already active".into()),
            transcript: None,
            transcript_preview: None,
            transcript_updated_at_ms: None,
            duration_ms: None,
            debug: None,
            error: None,
        });
    }

    if let Ok(transcript) = std::env::var("OMNI_TEST_TRANSCRIPT") {
        state.recording = Some(ActiveRecording::Synthetic {
            transcript,
            duration_ms: 1,
            started_at: Instant::now(),
        });

        let _ = hooks::run_transcribe_start_hooks_with_transcript("", false)?;
        ui_ipc::emit_event(
            "transcribe.started",
            serde_json::json!({
                "synthetic": true,
            }),
        );

        return Ok(DaemonResponse {
            ok: true,
            running: true,
            recording: true,
            pid: Some(pid),
            message: Some("recording started (synthetic test mode)".into()),
            transcript: None,
            transcript_preview: None,
            transcript_updated_at_ms: None,
            duration_ms: None,
            debug: None,
            error: None,
        });
    }

    let config_value = config::load_config()?;
    let backend = backend::OpenAiRealtimeBackend::from_config(&config_value)?;
    let mut recording_config = recording_config_from_config(&config_value)?;

    let mut transport = backend.build_transport();
    transport.connect()?;
    transport.send_session_update_model(&backend.model)?;

    let (chunk_tx, chunk_rx) = mpsc::channel::<Vec<i16>>();
    recording_config.chunk_sender = Some(chunk_tx);

    let progress = Arc::new(Mutex::new(RealtimeProgress::default()));
    let recorder = recording::Recorder::start(recording_config)?;
    let worker = spawn_realtime_worker(transport, chunk_rx, Arc::clone(&progress));
    state.recording = Some(ActiveRecording::Realtime {
        recorder,
        worker,
        progress,
        backend: backend.clone(),
        started_at: Instant::now(),
    });

    let _ = hooks::run_transcribe_start_hooks_with_transcript("", false)?;
    ui_ipc::emit_event(
        "transcribe.started",
        serde_json::json!({
            "synthetic": false,
            "llm_api": backend.llm_api,
        }),
    );

    Ok(DaemonResponse {
        ok: true,
        running: true,
        recording: true,
        pid: Some(pid),
        message: Some(format!("recording started (llmApi={})", backend.llm_api)),
        transcript: None,
        transcript_preview: None,
        transcript_updated_at_ms: None,
        duration_ms: None,
        debug: None,
        error: None,
    })
}

fn handle_status(pid: u32, state: &mut DaemonState) -> Result<DaemonResponse> {
    let (recording, duration_ms, transcript_preview, transcript_updated_at_ms, debug) =
        recording_runtime_state(state.recording.as_ref());

    Ok(DaemonResponse {
        ok: true,
        running: true,
        recording,
        pid: Some(pid),
        message: None,
        transcript: None,
        transcript_preview,
        transcript_updated_at_ms,
        duration_ms,
        debug,
        error: None,
    })
}

fn handle_transcribe_status(pid: u32, state: &mut DaemonState) -> Result<DaemonResponse> {
    let (recording, duration_ms, transcript_preview, transcript_updated_at_ms, debug) =
        recording_runtime_state(state.recording.as_ref());

    Ok(DaemonResponse {
        ok: true,
        running: true,
        recording,
        pid: Some(pid),
        message: Some(if recording {
            "recording active".into()
        } else {
            "recording idle".into()
        }),
        transcript: None,
        transcript_preview,
        transcript_updated_at_ms,
        duration_ms,
        debug,
        error: None,
    })
}

fn recording_runtime_state(
    recording: Option<&ActiveRecording>,
) -> (
    bool,
    Option<u64>,
    Option<String>,
    Option<u64>,
    Option<RealtimeDebugInfo>,
) {
    match recording {
        Some(ActiveRecording::Realtime {
            started_at,
            progress,
            ..
        }) => {
            let (preview, updated_at_ms, debug) = progress_snapshot(progress);
            (
                true,
                Some(started_at.elapsed().as_millis() as u64),
                preview,
                updated_at_ms,
                debug,
            )
        }
        Some(ActiveRecording::Synthetic {
            transcript,
            started_at,
            duration_ms,
        }) => {
            let measured = started_at.elapsed().as_millis() as u64;
            (
                true,
                Some((*duration_ms).max(measured)),
                if transcript.is_empty() {
                    None
                } else {
                    Some(transcript.clone())
                },
                Some(now_ms()),
                Some(RealtimeDebugInfo::default()),
            )
        }
        None => (false, Some(0), None, None, None),
    }
}

fn handle_transcribe_stop(
    pid: u32,
    state: &mut DaemonState,
    mode: Option<&str>,
) -> Result<DaemonResponse> {
    let Some(active) = state.recording.take() else {
        return Ok(DaemonResponse {
            ok: true,
            running: true,
            recording: false,
            pid: Some(pid),
            message: Some("recording not active".into()),
            transcript: Some(String::new()),
            transcript_preview: None,
            transcript_updated_at_ms: None,
            duration_ms: Some(0),
            debug: None,
            error: None,
        });
    };

    let (resolution, duration_ms, debug) = match active {
        ActiveRecording::Synthetic {
            transcript,
            duration_ms,
            started_at,
        } => (
            StopTranscriptResolution {
                transcript,
                source: TranscriptResolutionSource::Synthetic,
                fallback_attempted: false,
                fallback_used: false,
                fallback_error: None,
            },
            duration_ms.max(started_at.elapsed().as_millis() as u64),
            None,
        ),
        ActiveRecording::Realtime {
            recorder,
            worker,
            progress,
            backend,
            started_at,
        } => {
            let finished = recorder.stop()?;
            let realtime_transcript = join_realtime_worker(worker)?;

            let resolution = resolve_transcript_on_stop(
                &finished,
                &realtime_transcript,
                best_progress_transcript(&progress),
                &backend,
            );

            let mut debug = progress_snapshot(&progress).2.unwrap_or_default();
            apply_stop_transcript_resolution_debug(&mut debug, &resolution);

            let measured = started_at.elapsed().as_millis() as u64;
            (resolution, finished.duration_ms.max(measured), Some(debug))
        }
    };

    let _ = hooks::run_stop_hooks_with_transcript(mode, &resolution.transcript, false)?;

    let source = resolution.source.as_str();
    let fallback_used = resolution.fallback_used;

    let message = match mode {
        Some(mode) => format!(
            "recording stopped (mode={mode}, source={source}, fallback_used={fallback_used})"
        ),
        None => format!("recording stopped (source={source}, fallback_used={fallback_used})"),
    };

    ui_ipc::emit_event(
        "transcribe.stopped",
        serde_json::json!({
            "mode": mode,
            "duration_ms": duration_ms,
            "source": source,
            "fallback_used": fallback_used,
            "transcript": resolution.transcript.clone(),
        }),
    );

    Ok(DaemonResponse {
        ok: true,
        running: true,
        recording: false,
        pid: Some(pid),
        message: Some(message),
        transcript: Some(resolution.transcript),
        transcript_preview: None,
        transcript_updated_at_ms: None,
        duration_ms: Some(duration_ms),
        debug,
        error: None,
    })
}

fn resolve_transcript_on_stop(
    finished: &recording::FinishedRecording,
    realtime_transcript: &str,
    progress_fallback: Option<String>,
    backend: &backend::OpenAiRealtimeBackend,
) -> StopTranscriptResolution {
    if finished.samples.is_empty() {
        return StopTranscriptResolution {
            transcript: String::new(),
            source: TranscriptResolutionSource::EmptyNoAudio,
            fallback_attempted: false,
            fallback_used: false,
            fallback_error: None,
        };
    }

    let final_text = realtime_transcript.trim();
    if !final_text.is_empty() {
        return StopTranscriptResolution {
            transcript: final_text.to_string(),
            source: TranscriptResolutionSource::Realtime,
            fallback_attempted: false,
            fallback_used: false,
            fallback_error: None,
        };
    }

    if let Some(progress) = progress_fallback
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return StopTranscriptResolution {
            transcript: progress.to_string(),
            source: TranscriptResolutionSource::Progress,
            fallback_attempted: false,
            fallback_used: false,
            fallback_error: None,
        };
    }

    let wav = match recording::wav_bytes(finished) {
        Ok(wav) => wav,
        Err(error) => {
            return StopTranscriptResolution {
                transcript: String::new(),
                source: TranscriptResolutionSource::Empty,
                fallback_attempted: true,
                fallback_used: false,
                fallback_error: Some(error.to_string()),
            };
        }
    };

    let fallback_text = match backend.transcribe_wav_bytes(wav) {
        Ok(text) => text,
        Err(error) => {
            return StopTranscriptResolution {
                transcript: String::new(),
                source: TranscriptResolutionSource::Empty,
                fallback_attempted: true,
                fallback_used: false,
                fallback_error: Some(error.to_string()),
            };
        }
    };

    let trimmed = fallback_text.trim().to_string();
    if trimmed.is_empty() {
        return StopTranscriptResolution {
            transcript: String::new(),
            source: TranscriptResolutionSource::Empty,
            fallback_attempted: true,
            fallback_used: false,
            fallback_error: Some("batch transcription returned empty text".into()),
        };
    }

    StopTranscriptResolution {
        transcript: trimmed,
        source: TranscriptResolutionSource::Batch,
        fallback_attempted: true,
        fallback_used: true,
        fallback_error: None,
    }
}

fn apply_stop_transcript_resolution_debug(
    debug: &mut RealtimeDebugInfo,
    resolution: &StopTranscriptResolution,
) {
    debug.transcript_source = Some(resolution.source.as_str().to_string());
    debug.transcript_fallback_attempted = resolution.fallback_attempted;
    debug.transcript_fallback_used = resolution.fallback_used;
    debug.transcript_fallback_error = resolution.fallback_error.clone();
}

fn handle_reload(pid: u32, state: &mut DaemonState) -> Result<DaemonResponse> {
    validate_runtime_config()?;

    Ok(DaemonResponse {
        ok: true,
        running: true,
        recording: state.recording.is_some(),
        pid: Some(pid),
        message: Some("runtime config reloaded".into()),
        transcript: None,
        transcript_preview: None,
        transcript_updated_at_ms: None,
        duration_ms: None,
        debug: None,
        error: None,
    })
}

fn validate_runtime_config() -> Result<()> {
    let config_value = config::load_config()?;
    let _backend = backend::OpenAiRealtimeBackend::from_config(&config_value)?;
    let _recording = recording_config_from_config(&config_value)?;
    hooks::validate_hook_config(&config_value)?;
    Ok(())
}

fn spawn_realtime_worker(
    mut transport: backend::OpenAiRealtimeTransport,
    chunk_rx: mpsc::Receiver<Vec<i16>>,
    progress: SharedRealtimeProgress,
) -> thread::JoinHandle<Result<String>> {
    thread::spawn(move || {
        let mut transcript = String::new();
        let mut pending_audio_since_commit = false;
        let mut next_commit_at = Instant::now() + REALTIME_COMMIT_INTERVAL;

        loop {
            match chunk_rx.recv_timeout(REALTIME_LOOP_RECV_TIMEOUT) {
                Ok(chunk) => {
                    if chunk.is_empty() {
                        continue;
                    }

                    transport.send_audio_chunk(&chunk)?;
                    progress_note_chunk(&progress, &chunk);
                    pending_audio_since_commit = true;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }

            let _ = drain_realtime_events(
                &mut transport,
                &mut transcript,
                &progress,
                REALTIME_EVENT_DRAIN_TIMEOUT,
                24,
            )?;

            if pending_audio_since_commit && Instant::now() >= next_commit_at {
                transport.send_commit()?;
                progress_note_commit(&progress);
                pending_audio_since_commit = false;
                next_commit_at = Instant::now() + REALTIME_COMMIT_INTERVAL;

                let _ = drain_realtime_events(
                    &mut transport,
                    &mut transcript,
                    &progress,
                    Duration::from_millis(30),
                    96,
                )?;
            }
        }

        if pending_audio_since_commit {
            transport.send_commit_with_final(true)?;
            progress_note_commit(&progress);
        }

        let _ = collect_realtime_transcript(&mut transport, &progress, &mut transcript)?;

        set_progress_snapshot(&progress, &transcript);
        transport.close()?;

        Ok(transcript.trim().to_string())
    })
}

fn join_realtime_worker(worker: thread::JoinHandle<Result<String>>) -> Result<String> {
    match worker.join() {
        Ok(result) => result,
        Err(_) => bail!("realtime worker thread panicked"),
    }
}

#[derive(Debug, Default)]
struct RealtimeDrainStats {
    drained: usize,
    saw_transcript_output: bool,
}

fn drain_realtime_events(
    transport: &mut backend::OpenAiRealtimeTransport,
    transcript: &mut String,
    progress: &SharedRealtimeProgress,
    timeout: Duration,
    max_events: usize,
) -> Result<RealtimeDrainStats> {
    let mut stats = RealtimeDrainStats::default();

    for _ in 0..max_events {
        let Some(event) = transport.read_event_with_timeout(timeout)? else {
            break;
        };

        progress_note_event(progress, &event);

        if let Some(update) = transcript_update_from_event(&event) {
            let update_for_ui = update.clone();
            apply_transcript_update(transcript, update);
            set_progress_snapshot(progress, transcript);
            emit_transcript_update_for_ui(&update_for_ui, transcript);
            stats.saw_transcript_output = true;
        }

        stats.drained += 1;
    }

    Ok(stats)
}

fn collect_realtime_transcript(
    transport: &mut backend::OpenAiRealtimeTransport,
    progress: &SharedRealtimeProgress,
    transcript: &mut String,
) -> Result<bool> {
    let hard_deadline = Instant::now() + Duration::from_secs(6);
    let mut idle_deadline = Instant::now() + Duration::from_millis(700);
    let mut saw_transcript_output = false;

    while Instant::now() <= hard_deadline {
        let Some(event) = transport.read_event_with_timeout(Duration::from_millis(200))? else {
            if Instant::now() >= idle_deadline {
                break;
            }
            continue;
        };

        progress_note_event(progress, &event);
        idle_deadline = Instant::now() + Duration::from_millis(700);

        if let Some(update) = transcript_update_from_event(&event) {
            let update_for_ui = update.clone();
            apply_transcript_update(transcript, update);
            set_progress_snapshot(progress, transcript);
            emit_transcript_update_for_ui(&update_for_ui, transcript);
            saw_transcript_output = true;
        }

        if event_indicates_completion(&event) {
            // keep draining briefly for trailing events that can arrive after completion markers
            let drain = drain_realtime_events(
                transport,
                transcript,
                progress,
                Duration::from_millis(15),
                64,
            )?;
            saw_transcript_output |= drain.saw_transcript_output;
            idle_deadline = Instant::now() + Duration::from_millis(250);
        }
    }

    Ok(saw_transcript_output)
}

fn set_progress_snapshot(progress: &SharedRealtimeProgress, transcript: &str) {
    if let Ok(mut state) = progress.lock() {
        let trimmed = transcript.trim().to_string();
        if trimmed.is_empty() {
            return;
        }

        state.transcript_preview = trimmed.clone();
        state.last_non_empty_transcript = Some(trimmed);
        state.updated_at_ms = Some(now_ms());
    }
}

fn best_progress_transcript(progress: &SharedRealtimeProgress) -> Option<String> {
    let Ok(state) = progress.lock() else {
        return None;
    };

    if let Some(value) = state.last_non_empty_transcript.as_ref() {
        if !value.is_empty() {
            return Some(value.clone());
        }
    }

    if state.transcript_preview.is_empty() {
        None
    } else {
        Some(state.transcript_preview.clone())
    }
}

fn progress_snapshot(
    progress: &SharedRealtimeProgress,
) -> (Option<String>, Option<u64>, Option<RealtimeDebugInfo>) {
    let Ok(state) = progress.lock() else {
        return (None, None, None);
    };

    let preview = if state.transcript_preview.is_empty() {
        None
    } else {
        Some(state.transcript_preview.clone())
    };

    (preview, state.updated_at_ms, Some(state.debug.clone()))
}

fn progress_note_chunk(progress: &SharedRealtimeProgress, chunk: &[i16]) {
    if chunk.is_empty() {
        return;
    }

    let (rms, peak) = chunk_audio_levels(chunk);
    let mut ui_payload = None;

    if let Ok(mut state) = progress.lock() {
        state.debug.chunks_sent = state.debug.chunks_sent.saturating_add(1);

        let audio = &mut state.debug.audio;
        audio.chunks_observed = audio.chunks_observed.saturating_add(1);
        audio.samples_observed = audio.samples_observed.saturating_add(chunk.len() as u64);
        audio.last_rms = rms;
        audio.last_peak = peak;
        audio.max_peak = audio.max_peak.max(peak);
        audio.silence_threshold = REALTIME_AUDIO_SILENCE_THRESHOLD;

        let observed = audio.chunks_observed as f32;
        if observed <= 1.0 {
            audio.avg_rms = rms;
        } else {
            audio.avg_rms += (rms - audio.avg_rms) / observed;
        }

        if rms <= REALTIME_AUDIO_SILENCE_THRESHOLD {
            audio.silent_chunks = audio.silent_chunks.saturating_add(1);
        }

        ui_payload = Some(serde_json::json!({
            "rms": rms,
            "peak": peak,
            "rms_dbfs": normalized_level_to_dbfs(rms),
            "peak_dbfs": normalized_level_to_dbfs(peak),
            "silent": rms <= REALTIME_AUDIO_SILENCE_THRESHOLD,
            "silence_threshold": REALTIME_AUDIO_SILENCE_THRESHOLD,
            "chunks": audio.chunks_observed,
            "samples": audio.samples_observed,
        }));
    }

    if let Some(payload) = ui_payload {
        ui_ipc::emit_event("audio.energy", payload);
    }
}

fn chunk_audio_levels(chunk: &[i16]) -> (f32, f32) {
    if chunk.is_empty() {
        return (0.0, 0.0);
    }

    let mut sum_squares = 0.0_f64;
    let mut peak = 0.0_f32;

    for &sample in chunk {
        let normalized = sample as f32 / i16::MAX as f32;
        let abs = normalized.abs().min(1.0);
        peak = peak.max(abs);
        let n = abs as f64;
        sum_squares += n * n;
    }

    let rms = (sum_squares / chunk.len() as f64).sqrt() as f32;
    (rms, peak)
}

fn normalized_level_to_dbfs(level: f32) -> f32 {
    let clamped = level.abs().max(1.0e-6);
    20.0 * clamped.log10()
}

fn progress_note_commit(progress: &SharedRealtimeProgress) {
    if let Ok(mut state) = progress.lock() {
        state.debug.commits_sent = state.debug.commits_sent.saturating_add(1);
    }
}

fn progress_note_event(progress: &SharedRealtimeProgress, event: &serde_json::Value) {
    if let Ok(mut state) = progress.lock() {
        state.debug.events_received = state.debug.events_received.saturating_add(1);
        let seq = state.debug.events_received;
        let event_type = event
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("<unknown>")
            .to_string();
        let at_ms = now_ms();
        let text_fragment = event_text_fragment(event);
        let (error_code, error_message, error_event_id) = event_error_metadata(event);
        let raw_excerpt = event_raw_excerpt(event, &event_type);

        state.debug.last_event_type = Some(event_type.clone());
        state.debug.last_event_at_ms = Some(at_ms);

        if error_code.is_some() || error_message.is_some() || event_type == "error" {
            state.debug.errors_received = state.debug.errors_received.saturating_add(1);
            state.debug.last_error_code = error_code.clone();
            state.debug.last_error_message = error_message.clone();
            state.debug.last_error_event_id = error_event_id.clone();
        }

        state.debug.recent_events.push(RealtimeDebugEvent {
            seq,
            event_type,
            at_ms,
            text_fragment,
            error_code,
            error_message,
            error_event_id,
            raw_excerpt,
        });

        let overflow = state
            .debug
            .recent_events
            .len()
            .saturating_sub(REALTIME_DEBUG_EVENT_BUFFER);
        if overflow > 0 {
            state.debug.recent_events.drain(0..overflow);
        }
    }
}

fn event_text_fragment(event: &serde_json::Value) -> Option<String> {
    first_non_empty([
        text_from_field(event, "delta"),
        text_from_field(event, "text"),
        text_from_field(event, "transcript"),
        event.get("transcription").and_then(text_from_value),
        event.get("part").and_then(text_from_value),
        event.get("item").and_then(text_from_value),
        event.get("response").and_then(text_from_value),
    ])
    .map(|value| shorten(&value, 160))
}

fn event_raw_excerpt(event: &serde_json::Value, event_type: &str) -> Option<String> {
    if !event_type.starts_with("transcription") && event_type != "error" {
        return None;
    }

    Some(shorten(&event.to_string(), 380))
}

fn shorten(value: &str, max_chars: usize) -> String {
    let mut shortened = value.trim().to_string();
    if shortened.chars().count() <= max_chars {
        return shortened;
    }

    shortened = shortened.chars().take(max_chars).collect::<String>();
    shortened.push('…');
    shortened
}

fn event_error_metadata(
    event: &serde_json::Value,
) -> (Option<String>, Option<String>, Option<String>) {
    let error = event.get("error");

    let error_code = error
        .and_then(|value| value.get("code"))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
        .or_else(|| {
            event
                .get("code")
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
        });

    let error_message = error
        .and_then(|value| value.get("message"))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
        .or_else(|| {
            error
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
        })
        .or_else(|| {
            event
                .get("message")
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
        });

    let error_event_id = error
        .and_then(|value| value.get("event_id"))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
        .or_else(|| {
            event
                .get("event_id")
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
        });

    (error_code, error_message, error_event_id)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone)]
enum TranscriptUpdate {
    Delta(String),
    Snapshot(String),
}

#[cfg(test)]
fn transcript_fragment_from_event(event: &serde_json::Value) -> Option<String> {
    transcript_update_from_event(event).map(|update| match update {
        TranscriptUpdate::Delta(value) | TranscriptUpdate::Snapshot(value) => value,
    })
}

fn transcript_update_from_event(event: &serde_json::Value) -> Option<TranscriptUpdate> {
    let event_type = event
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("");

    let candidate = first_non_empty([
        text_from_field(event, "delta"),
        text_from_field(event, "text"),
        text_from_field(event, "transcript"),
        event.get("transcription").and_then(text_from_value),
        event.get("part").and_then(extract_text_from_part),
        event.get("item").and_then(extract_text_from_item),
        event.get("response").and_then(extract_text_from_response),
        extract_text_from_response(event),
        extract_text_from_item(event),
        extract_text_from_part(event),
    ]);

    match event_type {
        "response.output_text.delta"
        | "response.audio_transcript.delta"
        | "response.text.delta"
        | "conversation.item.input_audio_transcription.delta"
        | "conversation.item.input_audio_transcription.partial"
        | "transcription.delta" => candidate.map(TranscriptUpdate::Delta),
        "response.output_text.done"
        | "response.audio_transcript.done"
        | "response.text.done"
        | "conversation.item.input_audio_transcription.completed"
        | "conversation.item.input_audio_transcription.done"
        | "transcription.done" => candidate.map(TranscriptUpdate::Snapshot),
        "response.content_part.added" => candidate.map(TranscriptUpdate::Delta),
        "response.content_part.done"
        | "response.output_item.added"
        | "response.output_item.done"
        | "response.done"
        | "response.completed" => candidate.map(TranscriptUpdate::Snapshot),
        _ => candidate.map(TranscriptUpdate::Snapshot),
    }
}

fn text_from_field(event: &serde_json::Value, key: &str) -> Option<String> {
    event.get(key).and_then(text_from_value)
}

fn text_from_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => {
            if value.is_empty() {
                None
            } else {
                Some(value.clone())
            }
        }
        serde_json::Value::Array(items) => {
            let mut fragments = Vec::new();
            for item in items {
                if let Some(text) = text_from_value(item) {
                    fragments.push(text);
                }
            }
            join_fragments(fragments)
        }
        serde_json::Value::Object(map) => {
            let preferred_keys = [
                "text",
                "transcript",
                "delta",
                "value",
                "partial",
                "content",
                "parts",
                "item",
                "response",
                "output",
                "output_text",
                "message",
            ];

            for key in preferred_keys {
                if let Some(candidate) = map.get(key).and_then(text_from_value) {
                    if !candidate.is_empty() {
                        return Some(candidate);
                    }
                }
            }

            None
        }
        _ => None,
    }
}

fn apply_transcript_update(transcript: &mut String, update: TranscriptUpdate) {
    match update {
        TranscriptUpdate::Delta(delta) => {
            if !delta.is_empty() {
                transcript.push_str(&delta);
            }
        }
        TranscriptUpdate::Snapshot(snapshot) => merge_transcript_snapshot(transcript, &snapshot),
    }
}

fn emit_transcript_update_for_ui(update: &TranscriptUpdate, transcript: &str) {
    let preview = transcript.trim();
    if preview.is_empty() {
        return;
    }

    match update {
        TranscriptUpdate::Delta(delta) => {
            if delta.is_empty() {
                return;
            }

            ui_ipc::emit_event(
                "transcript.delta",
                serde_json::json!({
                    "delta": delta,
                    "preview": preview,
                }),
            );
        }
        TranscriptUpdate::Snapshot(snapshot) => {
            if snapshot.trim().is_empty() {
                return;
            }

            ui_ipc::emit_event(
                "transcript.snapshot",
                serde_json::json!({
                    "text": preview,
                }),
            );
        }
    }
}

fn merge_transcript_snapshot(transcript: &mut String, snapshot: &str) {
    let snapshot = snapshot.trim();
    if snapshot.is_empty() {
        return;
    }

    let current = transcript.trim().to_string();
    if current.is_empty() {
        *transcript = snapshot.to_string();
        return;
    }

    if snapshot == current {
        return;
    }

    if snapshot.starts_with(current.as_str()) {
        *transcript = snapshot.to_string();
        return;
    }

    if current.ends_with(snapshot) {
        return;
    }

    let needs_separator = transcript
        .chars()
        .last()
        .is_some_and(|ch| !ch.is_whitespace())
        && snapshot
            .chars()
            .next()
            .is_some_and(|ch| !ch.is_whitespace());

    if needs_separator {
        transcript.push(' ');
    }

    transcript.push_str(snapshot);
}

fn first_non_empty<const N: usize>(candidates: [Option<String>; N]) -> Option<String> {
    candidates
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty())
}

fn extract_text_from_part(part: &serde_json::Value) -> Option<String> {
    text_from_value(part)
}

fn extract_text_from_item(item: &serde_json::Value) -> Option<String> {
    text_from_value(item)
}

fn extract_text_from_response(response: &serde_json::Value) -> Option<String> {
    text_from_value(response)
}

fn join_fragments(fragments: Vec<String>) -> Option<String> {
    let mut joined = String::new();
    for fragment in fragments {
        if fragment.is_empty() {
            continue;
        }
        joined.push_str(&fragment);
    }

    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

fn event_indicates_completion(event: &serde_json::Value) -> bool {
    let Some(event_type) = event.get("type").and_then(|value| value.as_str()) else {
        return false;
    };

    if matches!(
        event_type,
        "response.done"
            | "response.completed"
            | "response.failed"
            | "response.error"
            | "response.output_item.done"
            | "conversation.item.input_audio_transcription.completed"
            | "transcription.done"
    ) {
        return true;
    }

    if event_type.starts_with("response.")
        && event
            .get("status")
            .and_then(|value| value.as_str())
            .is_some_and(|status| matches!(status, "completed" | "failed" | "cancelled"))
    {
        return true;
    }

    event
        .get("response")
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str())
        .is_some_and(|status| matches!(status, "completed" | "failed" | "cancelled"))
}

fn cleanup_active_recording(active: ActiveRecording) -> Result<()> {
    match active {
        ActiveRecording::Realtime {
            recorder,
            worker,
            progress: _,
            backend: _,
            started_at: _,
        } => {
            let _ = recorder.stop();
            let _ = join_realtime_worker(worker);
            Ok(())
        }
        ActiveRecording::Synthetic { .. } => Ok(()),
    }
}

fn recording_config_from_config(config: &toml::Value) -> Result<recording::RecordingConfig> {
    let device_name = config::get_value_by_key(config, "audio.device")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty() && *v != "default")
        .map(ToString::to_string);

    let sample_rate = config::get_value_by_key(config, "audio.sample_rate")
        .and_then(|v| v.as_integer())
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(16_000);

    let channels = config::get_value_by_key(config, "audio.channels")
        .and_then(|v| v.as_integer())
        .and_then(|v| u16::try_from(v).ok())
        .unwrap_or(1);

    if sample_rate == 0 {
        bail!("audio.sample_rate must be > 0");
    }
    if channels == 0 {
        bail!("audio.channels must be > 0");
    }

    Ok(recording::RecordingConfig {
        device_name,
        sample_rate,
        channels,
        chunk_sender: None,
    })
}

fn send_command(cmd: &str, mode: Option<String>) -> Result<DaemonResponse> {
    send_request(DaemonRequest {
        cmd: cmd.to_string(),
        mode,
    })
}

fn send_request(request: DaemonRequest) -> Result<DaemonResponse> {
    let paths = runtime_paths()?;
    let mut stream = UnixStream::connect(&paths.socket_path).with_context(|| {
        format!(
            "failed connecting to socket {}",
            paths.socket_path.display()
        )
    })?;

    let body = serde_json::to_string(&request)?;
    stream
        .write_all(format!("{body}\n").as_bytes())
        .context("failed writing daemon request")?;

    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    reader
        .read_line(&mut line)
        .context("failed reading daemon response")?;

    let response: DaemonResponse =
        serde_json::from_str(line.trim_end()).context("failed parsing daemon response JSON")?;

    Ok(response)
}

fn is_running() -> bool {
    send_command("ping", None)
        .map(|resp| resp.ok && resp.running)
        .unwrap_or(false)
}

pub fn status_snapshot() -> Result<DaemonResponse> {
    status_response()
}

fn status_response() -> Result<DaemonResponse> {
    if let Ok(response) = send_command("status", None) {
        return Ok(response);
    }

    Ok(DaemonResponse {
        ok: true,
        running: false,
        recording: false,
        pid: read_pid_file().ok(),
        message: None,
        transcript: None,
        transcript_preview: None,
        transcript_updated_at_ms: None,
        duration_ms: None,
        debug: None,
        error: None,
    })
}

fn read_pid_file() -> Result<u32> {
    let paths = runtime_paths()?;
    let raw = fs::read_to_string(paths.pid_path).context("failed reading pid file")?;
    raw.trim()
        .parse::<u32>()
        .context("failed parsing pid value in pid file")
}

fn wait_for_running(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() <= deadline {
        if is_running() {
            return true;
        }
        thread::sleep(POLL_INTERVAL);
    }

    false
}

fn wait_for_stopped(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() <= deadline {
        if !is_running() {
            return true;
        }
        thread::sleep(POLL_INTERVAL);
    }

    false
}

fn print_start_result(json_output: bool, pid: u32, already_running: bool) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "running": true,
                "recording": false,
                "pid": pid,
                "alreadyRunning": already_running,
            }))?
        );
    } else if already_running {
        println!("daemon already running (pid={pid})");
    } else {
        println!("daemon started (pid={pid})");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn load_realtime_fixture(name: &str) -> Vec<serde_json::Value> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name);

        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed reading fixture {}: {error}", path.display()));

        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|error| {
                    panic!(
                        "failed parsing fixture line in {}: {error}; line={line}",
                        path.display()
                    )
                })
            })
            .collect()
    }

    fn replay_fixture_events(events: &[serde_json::Value]) -> (String, usize, bool) {
        let mut transcript = String::new();
        let mut updates = 0;
        let mut saw_completion = false;

        for event in events {
            if let Some(update) = transcript_update_from_event(event) {
                apply_transcript_update(&mut transcript, update);
                updates += 1;
            }

            if event_indicates_completion(event) {
                saw_completion = true;
            }
        }

        (transcript.trim().to_string(), updates, saw_completion)
    }

    #[test]
    fn fixture_empty_delta_stream_remains_empty_and_completes() {
        let events = load_realtime_fixture("realtime-empty-delta-stream.jsonl");
        let (transcript, updates, saw_completion) = replay_fixture_events(&events);

        assert_eq!(updates, 0);
        assert!(transcript.is_empty());
        assert!(saw_completion);
    }

    #[test]
    fn fixture_sparse_final_response_extracts_snapshot_text() {
        let events = load_realtime_fixture("realtime-sparse-final-response.jsonl");
        let (transcript, updates, saw_completion) = replay_fixture_events(&events);

        assert_eq!(updates, 1);
        assert_eq!(transcript, "hello from sparse final");
        assert!(saw_completion);
    }

    #[test]
    fn fixture_delta_then_snapshot_merges_without_duplication() {
        let events = load_realtime_fixture("realtime-delta-then-snapshot.jsonl");
        let (transcript, updates, saw_completion) = replay_fixture_events(&events);

        assert_eq!(updates, 4);
        assert_eq!(transcript, "hello world");
        assert!(saw_completion);
    }

    #[test]
    fn fixture_top_level_error_event_is_captured_in_debug_metadata() {
        let events = load_realtime_fixture("realtime-empty-delta-with-top-level-error.jsonl");
        let (transcript, updates, saw_completion) = replay_fixture_events(&events);

        assert_eq!(updates, 0);
        assert!(transcript.is_empty());
        assert!(saw_completion);

        let progress = Arc::new(Mutex::new(RealtimeProgress::default()));
        for event in &events {
            progress_note_event(&progress, event);
        }

        let (_, _, debug) = progress_snapshot(&progress);
        let debug = debug.expect("debug info should exist");

        assert_eq!(debug.events_received, events.len() as u64);
        assert_eq!(debug.errors_received, 1);
        assert_eq!(debug.last_error_code.as_deref(), Some("unknown_event"));
        assert_eq!(
            debug.last_error_message.as_deref(),
            Some("Unknown event type: response.create")
        );
        assert_eq!(
            debug.last_error_event_id.as_deref(),
            Some("evt_unknown_001")
        );

        let last = debug
            .recent_events
            .last()
            .expect("event list should not be empty");
        assert_eq!(last.event_type, "error");
        assert_eq!(last.error_code.as_deref(), Some("unknown_event"));
        assert!(
            last.raw_excerpt
                .as_deref()
                .unwrap_or_default()
                .contains("unknown_event")
        );
    }

    #[test]
    fn transcript_fragment_from_event_handles_supported_shapes() {
        let delta = serde_json::json!({
            "type": "response.output_text.delta",
            "delta": "hel"
        });
        assert_eq!(
            transcript_fragment_from_event(&delta).as_deref(),
            Some("hel")
        );

        let completed = serde_json::json!({
            "type": "conversation.item.input_audio_transcription.completed",
            "transcript": "hello"
        });
        assert_eq!(
            transcript_fragment_from_event(&completed).as_deref(),
            Some("hello")
        );

        let transcription_delta = serde_json::json!({
            "type": "transcription.delta",
            "delta": " there"
        });
        assert_eq!(
            transcript_fragment_from_event(&transcription_delta).as_deref(),
            Some(" there")
        );

        let content_part = serde_json::json!({
            "type": "response.content_part.added",
            "part": {
                "type": "output_text",
                "text": " world"
            }
        });
        assert_eq!(
            transcript_fragment_from_event(&content_part).as_deref(),
            Some(" world")
        );

        let output_item_done = serde_json::json!({
            "type": "response.output_item.done",
            "item": {
                "content": [
                    {
                        "type": "output_text",
                        "text": "!"
                    }
                ]
            }
        });
        assert_eq!(
            transcript_fragment_from_event(&output_item_done).as_deref(),
            Some("!")
        );

        let response_done = serde_json::json!({
            "type": "response.done",
            "response": {
                "output": [
                    {
                        "content": [
                            {
                                "type": "output_text",
                                "text": " done"
                            }
                        ]
                    }
                ]
            }
        });
        assert_eq!(
            transcript_fragment_from_event(&response_done).as_deref(),
            Some(" done")
        );
    }

    #[test]
    fn completion_detector_recognizes_terminal_events() {
        let terminal = serde_json::json!({"type": "response.done"});
        let output_item_terminal = serde_json::json!({"type": "response.output_item.done"});
        let transcription_terminal = serde_json::json!({"type": "transcription.done"});
        let status_terminal = serde_json::json!({
            "type": "response.in_progress",
            "response": {
                "status": "completed"
            }
        });
        let non_terminal = serde_json::json!({"type": "response.output_text.delta"});

        assert!(event_indicates_completion(&terminal));
        assert!(event_indicates_completion(&output_item_terminal));
        assert!(event_indicates_completion(&transcription_terminal));
        assert!(event_indicates_completion(&status_terminal));
        assert!(!event_indicates_completion(&non_terminal));
    }

    #[test]
    fn recording_config_uses_defaults_and_validates_positive_numbers() {
        let config = crate::config::default_config();
        let parsed = recording_config_from_config(&config).expect("config should parse");

        assert_eq!(parsed.sample_rate, 16_000);
        assert_eq!(parsed.channels, 1);

        let mut invalid = crate::config::default_config();
        crate::config::set_value_by_key(&mut invalid, "audio.sample_rate", toml::Value::Integer(0))
            .expect("set should work");
        assert!(recording_config_from_config(&invalid).is_err());
    }

    #[test]
    fn synthetic_transcribe_start_activates_recording_state() {
        let _guard = ENV_LOCK.lock().expect("env lock should not be poisoned");

        unsafe {
            std::env::set_var("OMNI_TEST_TRANSCRIPT", "hello from test");
        }

        let mut state = DaemonState::default();
        let response =
            handle_transcribe_start(1234, &mut state).expect("transcribe start should succeed");

        assert!(response.ok);
        assert!(response.recording);
        assert!(state.recording.is_some());

        unsafe {
            std::env::remove_var("OMNI_TEST_TRANSCRIPT");
        }
    }

    #[test]
    fn synthetic_transcribe_stop_returns_transcript_and_clears_state() {
        let mut state = DaemonState {
            recording: Some(ActiveRecording::Synthetic {
                transcript: "synthetic transcript".into(),
                duration_ms: 42,
                started_at: Instant::now(),
            }),
        };

        let response =
            handle_transcribe_stop(1234, &mut state, None).expect("transcribe stop should succeed");

        assert!(response.ok);
        assert_eq!(response.transcript.as_deref(), Some("synthetic transcript"));
        assert_eq!(response.duration_ms, Some(42));
        assert_eq!(
            response.message.as_deref(),
            Some("recording stopped (source=synthetic, fallback_used=false)")
        );
        assert!(state.recording.is_none());
    }

    #[test]
    fn transcribe_status_reflects_active_synthetic_recording() {
        let mut state = DaemonState {
            recording: Some(ActiveRecording::Synthetic {
                transcript: "active".into(),
                duration_ms: 7,
                started_at: Instant::now(),
            }),
        };

        let response = handle_transcribe_status(1234, &mut state).expect("status should succeed");
        assert!(response.ok);
        assert!(response.recording);
        assert!(response.duration_ms.unwrap_or_default() >= 7);
        assert_eq!(response.transcript_preview.as_deref(), Some("active"));
        assert!(response.transcript_updated_at_ms.is_some());
        assert_eq!(response.message.as_deref(), Some("recording active"));
    }

    #[test]
    fn daemon_status_includes_recording_runtime_fields() {
        let mut state = DaemonState {
            recording: Some(ActiveRecording::Synthetic {
                transcript: "status preview".into(),
                duration_ms: 4,
                started_at: Instant::now(),
            }),
        };

        let response = handle_status(1234, &mut state).expect("status should succeed");
        assert!(response.ok);
        assert!(response.recording);
        assert_eq!(response.message, None);
        assert_eq!(
            response.transcript_preview.as_deref(),
            Some("status preview")
        );
        assert!(response.duration_ms.unwrap_or_default() >= 4);
    }

    #[test]
    fn progress_snapshot_reports_latest_fragment() {
        let progress = Arc::new(Mutex::new(RealtimeProgress::default()));

        set_progress_snapshot(&progress, "hello");
        set_progress_snapshot(&progress, "hello world");

        let (preview, updated_at_ms, debug) = progress_snapshot(&progress);
        assert_eq!(preview.as_deref(), Some("hello world"));
        assert!(updated_at_ms.is_some());
        assert!(debug.is_some());
    }

    #[test]
    fn progress_snapshot_preserves_last_non_empty_when_final_update_empty() {
        let progress = Arc::new(Mutex::new(RealtimeProgress::default()));

        set_progress_snapshot(&progress, "hello from realtime");
        set_progress_snapshot(&progress, "");

        let (preview, _, _) = progress_snapshot(&progress);
        assert_eq!(preview.as_deref(), Some("hello from realtime"));

        let best = best_progress_transcript(&progress);
        assert_eq!(best.as_deref(), Some("hello from realtime"));
    }

    #[test]
    fn progress_note_chunk_tracks_audio_levels_and_silence() {
        let progress = Arc::new(Mutex::new(RealtimeProgress::default()));

        progress_note_chunk(&progress, &[0, 0, 0, 0]);
        progress_note_chunk(&progress, &[10_000, -10_000, 8_000, -8_000]);

        let (_, _, debug) = progress_snapshot(&progress);
        let debug = debug.expect("debug info should exist");

        assert_eq!(debug.chunks_sent, 2);
        assert_eq!(debug.audio.chunks_observed, 2);
        assert_eq!(debug.audio.samples_observed, 8);
        assert!(debug.audio.silent_chunks >= 1);
        assert!(debug.audio.last_peak > 0.2);
        assert!(debug.audio.last_rms > 0.2);
        assert!(debug.audio.max_peak >= debug.audio.last_peak);
        assert_eq!(
            debug.audio.silence_threshold,
            REALTIME_AUDIO_SILENCE_THRESHOLD
        );
    }

    #[test]
    fn chunk_audio_levels_computes_rms_and_peak() {
        let (rms, peak) = chunk_audio_levels(&[i16::MAX, 0]);
        assert!((rms - 0.707).abs() < 0.01, "rms was {rms}");
        assert!((peak - 1.0).abs() < 0.001, "peak was {peak}");
    }

    #[test]
    fn progress_note_event_captures_recent_events() {
        let progress = Arc::new(Mutex::new(RealtimeProgress::default()));

        progress_note_event(
            &progress,
            &serde_json::json!({"type": "transcription.delta"}),
        );
        progress_note_event(
            &progress,
            &serde_json::json!({
                "type": "error",
                "error": {
                    "code": "invalid_request_error",
                    "message": "bad request",
                    "event_id": "evt_123"
                }
            }),
        );

        let (_, _, debug) = progress_snapshot(&progress);
        let debug = debug.expect("debug info should exist");

        assert_eq!(debug.events_received, 2);
        assert_eq!(debug.errors_received, 1);
        assert_eq!(debug.last_event_type.as_deref(), Some("error"));
        assert_eq!(
            debug.last_error_code.as_deref(),
            Some("invalid_request_error")
        );
        assert_eq!(debug.last_error_message.as_deref(), Some("bad request"));
        assert_eq!(debug.last_error_event_id.as_deref(), Some("evt_123"));
        assert_eq!(debug.recent_events.len(), 2);
        assert_eq!(debug.recent_events[0].seq, 1);
        assert_eq!(debug.recent_events[0].event_type, "transcription.delta");
        assert_eq!(debug.recent_events[1].seq, 2);
        assert_eq!(debug.recent_events[1].event_type, "error");
        assert_eq!(
            debug.recent_events[1].error_code.as_deref(),
            Some("invalid_request_error")
        );
    }

    #[test]
    fn transcript_updates_merge_delta_and_snapshots() {
        let mut transcript = String::new();

        apply_transcript_update(&mut transcript, TranscriptUpdate::Delta("Hello".into()));
        apply_transcript_update(&mut transcript, TranscriptUpdate::Delta(", ".into()));
        apply_transcript_update(&mut transcript, TranscriptUpdate::Delta("Victor".into()));
        apply_transcript_update(
            &mut transcript,
            TranscriptUpdate::Snapshot("Hello, Victor".into()),
        );
        apply_transcript_update(
            &mut transcript,
            TranscriptUpdate::Snapshot("software engineer".into()),
        );

        assert_eq!(transcript, "Hello, Victor software engineer");
    }

    #[test]
    fn transcribe_stop_is_noop_when_not_active() {
        let mut state = DaemonState::default();
        let response = handle_transcribe_stop(1234, &mut state, Some("copy"))
            .expect("transcribe stop should no-op successfully");

        assert!(response.ok);
        assert!(!response.recording);
        assert_eq!(response.message.as_deref(), Some("recording not active"));
    }

    #[test]
    fn apply_stop_resolution_debug_sets_resolution_fields() {
        let mut debug = RealtimeDebugInfo::default();
        let resolution = StopTranscriptResolution {
            transcript: "hello".into(),
            source: TranscriptResolutionSource::Batch,
            fallback_attempted: true,
            fallback_used: true,
            fallback_error: None,
        };

        apply_stop_transcript_resolution_debug(&mut debug, &resolution);

        assert_eq!(debug.transcript_source.as_deref(), Some("batch"));
        assert!(debug.transcript_fallback_attempted);
        assert!(debug.transcript_fallback_used);
        assert!(debug.transcript_fallback_error.is_none());
    }

    #[test]
    fn resolve_transcript_prefers_realtime_text() {
        let finished = recording::FinishedRecording {
            samples: vec![1, 2, 3],
            sample_rate: 16_000,
            channels: 1,
            duration_ms: 1,
        };
        let backend = backend::OpenAiRealtimeBackend {
            llm_api: "openai-realtime".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: String::new(),
            model: "voxtral-realtime".into(),
        };

        let resolved = resolve_transcript_on_stop(
            &finished,
            "  realtime text  ",
            Some("progress fallback".into()),
            &backend,
        );

        assert_eq!(resolved.transcript, "realtime text");
        assert_eq!(resolved.source, TranscriptResolutionSource::Realtime);
        assert!(!resolved.fallback_attempted);
        assert!(!resolved.fallback_used);
        assert!(resolved.fallback_error.is_none());
    }

    #[test]
    fn resolve_transcript_uses_progress_when_realtime_empty() {
        let finished = recording::FinishedRecording {
            samples: vec![1, 2, 3],
            sample_rate: 16_000,
            channels: 1,
            duration_ms: 1,
        };
        let backend = backend::OpenAiRealtimeBackend {
            llm_api: "openai-realtime".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: String::new(),
            model: "voxtral-realtime".into(),
        };

        let resolved =
            resolve_transcript_on_stop(&finished, "", Some("progress fallback".into()), &backend);

        assert_eq!(resolved.transcript, "progress fallback");
        assert_eq!(resolved.source, TranscriptResolutionSource::Progress);
        assert!(!resolved.fallback_attempted);
        assert!(!resolved.fallback_used);
        assert!(resolved.fallback_error.is_none());
    }

    #[test]
    fn resolve_transcript_is_empty_when_no_samples() {
        let finished = recording::FinishedRecording {
            samples: vec![],
            sample_rate: 16_000,
            channels: 1,
            duration_ms: 0,
        };
        let backend = backend::OpenAiRealtimeBackend {
            llm_api: "openai-realtime".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: String::new(),
            model: "voxtral-realtime".into(),
        };

        let resolved = resolve_transcript_on_stop(&finished, "", Some("progress".into()), &backend);

        assert!(resolved.transcript.is_empty());
        assert_eq!(resolved.source, TranscriptResolutionSource::EmptyNoAudio);
        assert!(!resolved.fallback_attempted);
        assert!(!resolved.fallback_used);
        assert!(resolved.fallback_error.is_none());
    }

    #[test]
    fn resolve_transcript_marks_fallback_error_when_wav_build_fails() {
        let finished = recording::FinishedRecording {
            samples: vec![1, 2, 3],
            sample_rate: 16_000,
            channels: 0,
            duration_ms: 1,
        };
        let backend = backend::OpenAiRealtimeBackend {
            llm_api: "openai-realtime".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: String::new(),
            model: "voxtral-realtime".into(),
        };

        let resolved = resolve_transcript_on_stop(&finished, "", None, &backend);

        assert!(resolved.transcript.is_empty());
        assert_eq!(resolved.source, TranscriptResolutionSource::Empty);
        assert!(resolved.fallback_attempted);
        assert!(!resolved.fallback_used);
        assert!(resolved.fallback_error.is_some());
    }
}
