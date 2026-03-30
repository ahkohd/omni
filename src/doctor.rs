#[cfg(unix)]
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use reqwest::blocking::Client;
use serde::Serialize;

use crate::{backend, config, daemon};

#[derive(Debug, Serialize)]
struct DoctorCheck {
    name: String,
    ok: bool,
    detail: String,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    ok: bool,
    checks: Vec<DoctorCheck>,
}

const AUDIO_PROBE_DURATION: Duration = Duration::from_millis(1_500);
const AUDIO_SILENCE_THRESHOLD: f32 = 0.008;

#[derive(Debug, Clone)]
struct AudioProbeConfig {
    device_name: Option<String>,
    sample_rate: u32,
    channels: u16,
    duration: Duration,
    silence_threshold: f32,
}

#[derive(Debug, Clone)]
struct AudioProbeResult {
    device_name: String,
    sample_rate: u32,
    channels: u16,
    duration_ms: u64,
    chunks: u64,
    samples: u64,
    rms: f32,
    peak: f32,
    silent_chunks: u64,
}

#[derive(Debug, Default)]
struct AudioProbeAccumulator {
    chunks: u64,
    samples: u64,
    sum_squares: f64,
    peak: f32,
    silent_chunks: u64,
}

pub fn run(json_output: bool) -> Result<()> {
    let mut checks = Vec::new();

    match config::config_path() {
        Ok(path) => checks.push(DoctorCheck {
            name: "config.path".into(),
            ok: true,
            detail: path.display().to_string(),
        }),
        Err(error) => checks.push(DoctorCheck {
            name: "config.path".into(),
            ok: false,
            detail: error.to_string(),
        }),
    }

    let config_value = match config::load_config() {
        Ok(config) => {
            checks.push(DoctorCheck {
                name: "config.parse".into(),
                ok: true,
                detail: "loaded config successfully".into(),
            });
            Some(config)
        }
        Err(error) => {
            checks.push(DoctorCheck {
                name: "config.parse".into(),
                ok: false,
                detail: error.to_string(),
            });
            None
        }
    };

    let backend = if let Some(config) = &config_value {
        match backend::OpenAiRealtimeBackend::from_config(config) {
            Ok(backend) => {
                checks.push(DoctorCheck {
                    name: "server.llmApi".into(),
                    ok: true,
                    detail: backend.llm_api.clone(),
                });
                Some(backend)
            }
            Err(error) => {
                checks.push(DoctorCheck {
                    name: "server.llmApi".into(),
                    ok: false,
                    detail: error.to_string(),
                });
                None
            }
        }
    } else {
        None
    };

    match daemon::status_snapshot() {
        Ok(status) => {
            let detail = if status.running {
                format!(
                    "running pid={} recording={}",
                    status.pid.unwrap_or_default(),
                    status.recording
                )
            } else {
                "stopped".to_string()
            };

            checks.push(DoctorCheck {
                name: "daemon.status".into(),
                ok: status.running,
                detail,
            });
        }
        Err(error) => checks.push(DoctorCheck {
            name: "daemon.status".into(),
            ok: false,
            detail: error.to_string(),
        }),
    }

    if let Some(backend) = &backend {
        let endpoint = format!("{}/models", backend.base_url.trim_end_matches('/'));
        let client = Client::builder().timeout(Duration::from_secs(3)).build()?;
        let mut request = client.get(&endpoint);
        if !backend.api_key.is_empty() {
            request = request.bearer_auth(&backend.api_key);
        }

        match request.send() {
            Ok(response) => {
                let status = response.status();
                let body = response.text().unwrap_or_default();
                let reachability_ok =
                    status.is_success() || status.as_u16() == 401 || status.as_u16() == 403;
                checks.push(DoctorCheck {
                    name: "server.reachability".into(),
                    ok: reachability_ok,
                    detail: format!("{endpoint} -> HTTP {status}"),
                });

                if status.is_success() {
                    match configured_model_loaded(&body, &backend.model) {
                        Ok((true, model_ids)) => checks.push(DoctorCheck {
                            name: "server.model.loaded".into(),
                            ok: true,
                            detail: format!(
                                "configured model {} found in /models ({})",
                                backend.model,
                                summarize_model_ids(&model_ids)
                            ),
                        }),
                        Ok((false, model_ids)) => checks.push(DoctorCheck {
                            name: "server.model.loaded".into(),
                            ok: false,
                            detail: format!(
                                "configured model {} not listed by /models ({})",
                                backend.model,
                                summarize_model_ids(&model_ids)
                            ),
                        }),
                        Err(error) => checks.push(DoctorCheck {
                            name: "server.model.loaded".into(),
                            ok: false,
                            detail: format!("failed parsing /models response: {error}"),
                        }),
                    }
                } else {
                    checks.push(DoctorCheck {
                        name: "server.model.loaded".into(),
                        ok: false,
                        detail: format!(
                            "cannot verify configured model {} because /models returned HTTP {status}",
                            backend.model
                        ),
                    });
                }
            }
            Err(error) => {
                checks.push(DoctorCheck {
                    name: "server.reachability".into(),
                    ok: false,
                    detail: format!("{endpoint} -> {error}"),
                });
                checks.push(DoctorCheck {
                    name: "server.model.loaded".into(),
                    ok: false,
                    detail: format!("cannot verify configured model {}: {error}", backend.model),
                });
            }
        }
    }

    let audio_probe_config = match audio_probe_config_from_config(config_value.as_ref()) {
        Ok(config) => {
            checks.push(DoctorCheck {
                name: "audio.probe.config".into(),
                ok: true,
                detail: format!(
                    "device={} sample_rate={} channels={} duration_ms={} silence_threshold={:.3}",
                    config.device_name.as_deref().unwrap_or("default"),
                    config.sample_rate,
                    config.channels,
                    config.duration.as_millis(),
                    config.silence_threshold,
                ),
            });
            Some(config)
        }
        Err(error) => {
            checks.push(DoctorCheck {
                name: "audio.probe.config".into(),
                ok: false,
                detail: error.to_string(),
            });
            None
        }
    };

    if let Some(audio_probe_config) = audio_probe_config {
        match run_audio_probe(&audio_probe_config) {
            Ok(result) => {
                checks.push(DoctorCheck {
                    name: "audio.input_device".into(),
                    ok: true,
                    detail: format!(
                        "{} (sample_rate={} channels={})",
                        result.device_name, result.sample_rate, result.channels
                    ),
                });

                checks.push(DoctorCheck {
                    name: "audio.probe.capture".into(),
                    ok: result.samples > 0,
                    detail: format!(
                        "captured {} chunks / {} samples over {}ms",
                        result.chunks, result.samples, result.duration_ms
                    ),
                });

                let silence_pct = if result.chunks == 0 {
                    0.0
                } else {
                    (result.silent_chunks as f32 * 100.0) / result.chunks as f32
                };
                let level_ok = result.peak > audio_probe_config.silence_threshold;

                let level_detail = format!(
                    "rms={:.6} ({:.1} dBFS) peak={:.6} ({:.1} dBFS) silence={}/{} ({:.0}%) threshold={:.3} ({:.1} dBFS)",
                    result.rms,
                    normalized_level_to_dbfs(result.rms),
                    result.peak,
                    normalized_level_to_dbfs(result.peak),
                    result.silent_chunks,
                    result.chunks,
                    silence_pct,
                    audio_probe_config.silence_threshold,
                    normalized_level_to_dbfs(audio_probe_config.silence_threshold),
                );
                checks.push(DoctorCheck {
                    name: "audio.probe.level".into(),
                    ok: level_ok,
                    detail: if level_ok {
                        level_detail
                    } else {
                        format!(
                            "{level_detail}; low input level detected (try speaking during doctor run)"
                        )
                    },
                });
            }
            Err(error) => {
                checks.push(DoctorCheck {
                    name: "audio.input_device".into(),
                    ok: false,
                    detail: format!(
                        "requested={} ({error})",
                        audio_probe_config
                            .device_name
                            .as_deref()
                            .unwrap_or("default")
                    ),
                });
                checks.push(DoctorCheck {
                    name: "audio.probe.capture".into(),
                    ok: false,
                    detail: error.to_string(),
                });
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let tools = ["pbcopy", "pbpaste", "osascript"];
        for tool in tools {
            let ok = command_exists(tool);
            checks.push(DoctorCheck {
                name: format!("tool.{tool}"),
                ok,
                detail: if ok {
                    "found".into()
                } else {
                    "missing from PATH".into()
                },
            });
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let wl_paste = command_exists("wl-paste");
        let wl_copy = command_exists("wl-copy");
        let xclip = command_exists("xclip");
        let wtype = command_exists("wtype");
        let xdotool = command_exists("xdotool");

        checks.push(DoctorCheck {
            name: "tool.clipboard.read".into(),
            ok: wl_paste || xclip,
            detail: format!(
                "need wl-paste or xclip (wl-paste: {}, xclip: {})",
                tool_presence(wl_paste),
                tool_presence(xclip)
            ),
        });

        checks.push(DoctorCheck {
            name: "tool.clipboard.write".into(),
            ok: wl_copy || xclip,
            detail: format!(
                "need wl-copy or xclip (wl-copy: {}, xclip: {})",
                tool_presence(wl_copy),
                tool_presence(xclip)
            ),
        });

        checks.push(DoctorCheck {
            name: "tool.clipboard.paste".into(),
            ok: wtype || xdotool,
            detail: format!(
                "need wtype (Wayland) or xdotool (X11) (wtype: {}, xdotool: {})",
                tool_presence(wtype),
                tool_presence(xdotool)
            ),
        });
    }

    let report = DoctorReport {
        ok: checks.iter().all(|check| check.ok),
        checks,
    };

    if json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("doctor: {}", if report.ok { "ok" } else { "issues found" });
        for check in report.checks {
            let marker = if check.ok { "✓" } else { "✗" };
            println!("{marker} {} — {}", check.name, check.detail);
        }
    }

    Ok(())
}

fn configured_model_loaded(body: &str, configured_model: &str) -> Result<(bool, Vec<String>)> {
    let payload: serde_json::Value =
        serde_json::from_str(body).map_err(|error| anyhow!(error.to_string()))?;
    let model_ids = model_ids_from_payload(&payload);
    if model_ids.is_empty() {
        return Err(anyhow!("no model ids found in /models payload"));
    }

    let loaded = model_ids.iter().any(|id| id == configured_model);
    Ok((loaded, model_ids))
}

fn model_ids_from_payload(payload: &serde_json::Value) -> Vec<String> {
    fn collect_ids(items: &[serde_json::Value]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| item.get("id").and_then(|value| value.as_str()))
            .map(ToString::to_string)
            .collect()
    }

    if let Some(items) = payload.get("data").and_then(|value| value.as_array()) {
        return collect_ids(items);
    }

    if let Some(items) = payload.get("models").and_then(|value| value.as_array()) {
        return collect_ids(items);
    }

    if let Some(items) = payload.as_array() {
        return collect_ids(items);
    }

    Vec::new()
}

fn summarize_model_ids(model_ids: &[String]) -> String {
    if model_ids.is_empty() {
        return "none".into();
    }

    let mut sample: Vec<String> = model_ids.iter().take(5).cloned().collect();
    if model_ids.len() > 5 {
        sample.push(format!("... +{} more", model_ids.len() - 5));
    }

    sample.join(", ")
}

fn audio_probe_config_from_config(config: Option<&toml::Value>) -> Result<AudioProbeConfig> {
    let device_name = config
        .and_then(|value| crate::config::get_value_by_key(value, "audio.device"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "default")
        .map(ToString::to_string);

    let sample_rate = config
        .and_then(|value| crate::config::get_value_by_key(value, "audio.sample_rate"))
        .and_then(|value| value.as_integer())
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(16_000);

    let channels = config
        .and_then(|value| crate::config::get_value_by_key(value, "audio.channels"))
        .and_then(|value| value.as_integer())
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(1);

    if sample_rate == 0 {
        bail!("audio.sample_rate must be > 0 for audio probe");
    }
    if channels == 0 {
        bail!("audio.channels must be > 0 for audio probe");
    }

    Ok(AudioProbeConfig {
        device_name,
        sample_rate,
        channels,
        duration: AUDIO_PROBE_DURATION,
        silence_threshold: AUDIO_SILENCE_THRESHOLD,
    })
}

fn run_audio_probe(config: &AudioProbeConfig) -> Result<AudioProbeResult> {
    let host = cpal::default_host();
    let device = select_input_device(&host, config.device_name.as_deref())?;
    let device_name = device.name().unwrap_or_else(|_| "<unknown>".to_string());

    let supported = device
        .default_input_config()
        .context("failed getting default input config for audio probe")?;

    let mut stream_config: cpal::StreamConfig = supported.config();
    stream_config.sample_rate = cpal::SampleRate(config.sample_rate);
    stream_config.channels = config.channels;

    let accumulator = Arc::new(Mutex::new(AudioProbeAccumulator::default()));
    let err_fn = |error| {
        eprintln!("omni doctor audio probe stream error: {error}");
    };

    let stream = match supported.sample_format() {
        cpal::SampleFormat::I16 => {
            let accumulator = Arc::clone(&accumulator);
            let silence_threshold = config.silence_threshold;
            device.build_input_stream(
                &stream_config,
                move |data: &[i16], _| accumulate_i16(data, &accumulator, silence_threshold),
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let accumulator = Arc::clone(&accumulator);
            let silence_threshold = config.silence_threshold;
            device.build_input_stream(
                &stream_config,
                move |data: &[u16], _| accumulate_u16(data, &accumulator, silence_threshold),
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::F32 => {
            let accumulator = Arc::clone(&accumulator);
            let silence_threshold = config.silence_threshold;
            device.build_input_stream(
                &stream_config,
                move |data: &[f32], _| accumulate_f32(data, &accumulator, silence_threshold),
                err_fn,
                None,
            )
        }
        sample_format => {
            bail!("unsupported input sample format for audio probe: {sample_format:?}")
        }
    }
    .context("failed building audio probe input stream")?;

    stream
        .play()
        .context("failed starting audio probe input stream")?;
    thread::sleep(config.duration);
    drop(stream);

    let state = accumulator
        .lock()
        .map_err(|_| anyhow!("audio probe accumulator lock poisoned"))?;

    let rms = if state.samples == 0 {
        0.0
    } else {
        (state.sum_squares / state.samples as f64).sqrt() as f32
    };

    Ok(AudioProbeResult {
        device_name,
        sample_rate: stream_config.sample_rate.0,
        channels: stream_config.channels,
        duration_ms: config.duration.as_millis() as u64,
        chunks: state.chunks,
        samples: state.samples,
        rms,
        peak: state.peak,
        silent_chunks: state.silent_chunks,
    })
}

fn select_input_device(host: &cpal::Host, requested_name: Option<&str>) -> Result<cpal::Device> {
    if let Some(name) = requested_name
        && name != "default"
    {
        let devices = host
            .input_devices()
            .context("failed listing input audio devices")?;

        for device in devices {
            if let Ok(device_name) = device.name()
                && device_name == name
            {
                return Ok(device);
            }
        }

        bail!(
            "requested input device not found: {name}. Run `omni input list` then `omni input set <id>` (or `omni input set default`)"
        );
    }

    host.default_input_device()
        .ok_or_else(|| anyhow!("no default input device available"))
}

fn accumulate_i16(
    data: &[i16],
    accumulator: &Arc<Mutex<AudioProbeAccumulator>>,
    silence_threshold: f32,
) {
    if data.is_empty() {
        return;
    }

    let mut sum_squares = 0.0_f64;
    let mut peak = 0.0_f32;
    for &sample in data {
        let normalized = sample as f32 / i16::MAX as f32;
        let abs = normalized.abs().min(1.0);
        peak = peak.max(abs);
        let value = abs as f64;
        sum_squares += value * value;
    }

    update_probe_accumulator(
        accumulator,
        data.len(),
        sum_squares,
        peak,
        silence_threshold,
    );
}

fn accumulate_u16(
    data: &[u16],
    accumulator: &Arc<Mutex<AudioProbeAccumulator>>,
    silence_threshold: f32,
) {
    if data.is_empty() {
        return;
    }

    let mut sum_squares = 0.0_f64;
    let mut peak = 0.0_f32;
    for &sample in data {
        let centered = sample as f32 - 32_768.0;
        let normalized = centered / 32_768.0;
        let abs = normalized.abs().min(1.0);
        peak = peak.max(abs);
        let value = abs as f64;
        sum_squares += value * value;
    }

    update_probe_accumulator(
        accumulator,
        data.len(),
        sum_squares,
        peak,
        silence_threshold,
    );
}

fn accumulate_f32(
    data: &[f32],
    accumulator: &Arc<Mutex<AudioProbeAccumulator>>,
    silence_threshold: f32,
) {
    if data.is_empty() {
        return;
    }

    let mut sum_squares = 0.0_f64;
    let mut peak = 0.0_f32;
    for &sample in data {
        let normalized = sample.clamp(-1.0, 1.0);
        let abs = normalized.abs();
        peak = peak.max(abs);
        let value = abs as f64;
        sum_squares += value * value;
    }

    update_probe_accumulator(
        accumulator,
        data.len(),
        sum_squares,
        peak,
        silence_threshold,
    );
}

fn update_probe_accumulator(
    accumulator: &Arc<Mutex<AudioProbeAccumulator>>,
    chunk_len: usize,
    chunk_sum_squares: f64,
    chunk_peak: f32,
    silence_threshold: f32,
) {
    let chunk_rms = (chunk_sum_squares / chunk_len as f64).sqrt() as f32;

    if let Ok(mut state) = accumulator.lock() {
        state.chunks = state.chunks.saturating_add(1);
        state.samples = state.samples.saturating_add(chunk_len as u64);
        state.sum_squares += chunk_sum_squares;
        state.peak = state.peak.max(chunk_peak);

        if chunk_rms <= silence_threshold {
            state.silent_chunks = state.silent_chunks.saturating_add(1);
        }
    }
}

fn normalized_level_to_dbfs(level: f32) -> f32 {
    let clamped = level.abs().max(1.0e-6);
    20.0 * clamped.log10()
}

#[cfg(unix)]
fn command_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn tool_presence(found: bool) -> &'static str {
    if found { "found" } else { "missing" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_model_loaded_detects_model_present() {
        let body = r#"{"object":"list","data":[{"id":"voxtral-realtime"},{"id":"other-model"}]}"#;

        let (loaded, ids) =
            configured_model_loaded(body, "voxtral-realtime").expect("models payload should parse");

        assert!(loaded);
        assert_eq!(ids[0], "voxtral-realtime");
    }

    #[test]
    fn configured_model_loaded_detects_model_missing() {
        let body = r#"{"models":[{"id":"a"},{"id":"b"}]}"#;

        let (loaded, ids) =
            configured_model_loaded(body, "voxtral-realtime").expect("models payload should parse");

        assert!(!loaded);
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn model_ids_from_payload_handles_array_shape() {
        let payload = serde_json::json!([
            {"id": "x"},
            {"id": "y"}
        ]);

        assert_eq!(
            model_ids_from_payload(&payload),
            vec!["x".to_string(), "y".to_string()]
        );
    }

    #[test]
    fn audio_probe_config_defaults_are_valid() {
        let config = audio_probe_config_from_config(None).expect("defaults should parse");
        assert_eq!(config.sample_rate, 16_000);
        assert_eq!(config.channels, 1);
        assert!(config.device_name.is_none());
        assert!(config.duration.as_millis() > 0);
    }

    #[test]
    fn audio_probe_config_rejects_zero_sample_rate() {
        let mut config = crate::config::default_config();
        crate::config::set_value_by_key(&mut config, "audio.sample_rate", toml::Value::Integer(0))
            .expect("set should work");

        assert!(audio_probe_config_from_config(Some(&config)).is_err());
    }

    #[test]
    fn normalized_level_to_dbfs_behaves_as_expected() {
        let unity = normalized_level_to_dbfs(1.0);
        let silence = normalized_level_to_dbfs(0.0);

        assert!((unity - 0.0).abs() < 0.001);
        assert!(silence <= -100.0);
    }

    #[test]
    fn update_probe_accumulator_tracks_levels_and_silence() {
        let accumulator = Arc::new(Mutex::new(AudioProbeAccumulator::default()));

        update_probe_accumulator(&accumulator, 4, 0.0, 0.0, AUDIO_SILENCE_THRESHOLD);
        update_probe_accumulator(&accumulator, 4, 1.0, 0.6, AUDIO_SILENCE_THRESHOLD);

        let state = accumulator.lock().expect("lock should work");
        assert_eq!(state.chunks, 2);
        assert_eq!(state.samples, 8);
        assert!(state.silent_chunks >= 1);
        assert!(state.peak >= 0.6);
    }
}
