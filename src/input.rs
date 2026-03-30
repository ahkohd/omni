use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;

use crate::{config, daemon};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InputDeviceEntry {
    id: String,
    name: String,
    is_default: bool,
    is_selected: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InputListResponse {
    ok: bool,
    configured_device: String,
    configured_device_available: bool,
    devices: Vec<InputDeviceEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InputShowResponse {
    ok: bool,
    configured_device: String,
    configured_device_available: bool,
    active_device: Option<String>,
    active_id: Option<String>,
    active_is_default: bool,
    default_device: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InputSetResponse {
    ok: bool,
    id: Option<String>,
    name: Option<String>,
    device: String,
    key: String,
    path: String,
    daemon_reloaded: bool,
    message: String,
}

pub fn list(json_output: bool) -> Result<()> {
    let config_value = config::load_config()?;
    let configured_device = configured_device_name(&config_value);
    let (devices, configured_available) = enumerate_input_devices(configured_device.as_deref())?;

    if json_output {
        let response = InputListResponse {
            ok: true,
            configured_device: configured_device
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            configured_device_available: configured_available,
            devices,
        };

        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    println!(
        "configured input: {}",
        configured_device
            .as_deref()
            .unwrap_or("default (system default input device)")
    );

    if configured_device.is_some() && !configured_available {
        println!("warning: configured input device is not currently available");
    }

    println!("available input devices:");
    for device in devices {
        let mut tags = Vec::new();
        if device.is_selected {
            tags.push("selected");
        }
        if device.is_default {
            tags.push("default");
        }

        if tags.is_empty() {
            println!("  [{}] {}", device.id, device.name);
        } else {
            println!("  [{}] {} ({})", device.id, device.name, tags.join(", "));
        }
    }

    Ok(())
}

pub fn show(json_output: bool) -> Result<()> {
    let config_value = config::load_config()?;
    let configured_device = configured_device_name(&config_value);
    let configured_device_value = configured_device
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let (devices, configured_available) = enumerate_input_devices(configured_device.as_deref())?;

    let active = resolve_active_device(&devices, configured_device.as_deref());
    let default_device = devices
        .iter()
        .find(|device| device.is_default)
        .map(|device| device.name.clone());

    if json_output {
        let response = InputShowResponse {
            ok: true,
            configured_device: configured_device_value,
            configured_device_available: configured_available,
            active_device: active.map(|device| device.name.clone()),
            active_id: active.map(|device| device.id.clone()),
            active_is_default: active.is_some_and(|device| device.is_default),
            default_device,
        };

        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    println!("configured input: {configured_device_value}");
    println!(
        "configured available: {}",
        if configured_available { "yes" } else { "no" }
    );

    match active {
        Some(device) => {
            if device.is_default {
                println!("active input: {} [{}] (default)", device.name, device.id);
            } else {
                println!("active input: {} [{}]", device.name, device.id);
            }
        }
        None => {
            println!("active input: unavailable");
            println!("hint: run `omni input list` then `omni input set <id>`");
        }
    }

    if let Some(default_device) = default_device {
        println!("default input: {default_device}");
    }

    Ok(())
}

pub fn set(id: Option<String>, name: Option<String>, json_output: bool) -> Result<()> {
    let mut config_value = config::load_config()?;

    let (selected_name, selected_id, selected_by_name) = match (id, name) {
        (Some(id), None) => {
            if id == "default" {
                ("default".to_string(), Some(id), None)
            } else {
                let (devices, _) = enumerate_input_devices(None)?;
                let resolved_name = resolve_device_name_by_id(&devices, &id)?;
                (resolved_name, Some(id), None)
            }
        }
        (None, Some(name)) => {
            let requested = name.trim();
            if requested.is_empty() {
                bail!("input device name cannot be empty");
            }

            if requested == "default" {
                ("default".to_string(), Some("default".to_string()), None)
            } else {
                let (devices, _) = enumerate_input_devices(None)?;
                let resolved_name = resolve_device_name_by_name(&devices, requested)?;
                (resolved_name, None, Some(requested.to_string()))
            }
        }
        (None, None) => bail!("provide either <id> or --name <device>"),
        (Some(_), Some(_)) => bail!("provide either <id> or --name <device>, not both"),
    };

    config::set_value_by_key(
        &mut config_value,
        "audio.device",
        toml::Value::String(selected_name.clone()),
    )?;
    let path = config::save_config(&config_value)?;

    let daemon_reloaded = daemon::reload_runtime_if_running()?.is_some();

    let message = if daemon_reloaded {
        "input device saved and daemon reloaded".to_string()
    } else {
        "input device saved (daemon not running)".to_string()
    };

    if json_output {
        let response = InputSetResponse {
            ok: true,
            id: selected_id,
            name: selected_by_name,
            device: selected_name,
            key: "audio.device".to_string(),
            path: path.display().to_string(),
            daemon_reloaded,
            message,
        };

        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!("updated audio.device in {}", path.display());
        println!("audio.device={selected_name}");
        if daemon_reloaded {
            println!("daemon runtime config reloaded");
        } else {
            println!("daemon not running; config applies on next start");
        }
    }

    Ok(())
}

fn configured_device_name(config: &toml::Value) -> Option<String> {
    config::get_value_by_key(config, "audio.device")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "default")
        .map(ToString::to_string)
}

fn enumerate_input_devices(
    configured_device: Option<&str>,
) -> Result<(Vec<InputDeviceEntry>, bool)> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|device| device.name().ok());

    let mut devices = Vec::new();
    for (index, device) in host
        .input_devices()
        .context("failed listing input audio devices")?
        .enumerate()
    {
        let name = device.name().unwrap_or_else(|_| "<unknown>".to_string());
        let is_default = default_name.as_deref() == Some(name.as_str());
        let is_selected = match configured_device {
            Some(configured) => configured == name,
            None => is_default,
        };

        devices.push(InputDeviceEntry {
            id: index.to_string(),
            name,
            is_default,
            is_selected,
        });
    }

    if devices.is_empty() {
        bail!("no input audio devices available");
    }

    let configured_device_available = configured_device
        .map(|configured| devices.iter().any(|device| device.name == configured))
        .unwrap_or(true);

    Ok((devices, configured_device_available))
}

fn resolve_active_device<'a>(
    devices: &'a [InputDeviceEntry],
    configured_device: Option<&str>,
) -> Option<&'a InputDeviceEntry> {
    match configured_device {
        Some(configured_device) => devices
            .iter()
            .find(|device| device.name == configured_device),
        None => devices.iter().find(|device| device.is_default),
    }
}

fn resolve_device_name_by_name(devices: &[InputDeviceEntry], name: &str) -> Result<String> {
    let Some(device) = devices.iter().find(|device| device.name == name) else {
        let available: Vec<&str> = devices.iter().map(|device| device.name.as_str()).collect();
        bail!(
            "input device '{}' not found (available devices: {})",
            name,
            available.join(", ")
        );
    };

    Ok(device.name.clone())
}

fn resolve_device_name_by_id(devices: &[InputDeviceEntry], id: &str) -> Result<String> {
    let parsed = id
        .parse::<usize>()
        .with_context(|| format!("invalid input device id: {id}"))?;

    let Some(device) = devices.get(parsed) else {
        let available: Vec<&str> = devices.iter().map(|device| device.id.as_str()).collect();
        bail!(
            "input device id {} not found (available ids: {})",
            parsed,
            available.join(", ")
        );
    };

    Ok(device.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_device_name_treats_default_as_unset() {
        let mut config = crate::config::default_config();
        crate::config::set_value_by_key(
            &mut config,
            "audio.device",
            toml::Value::String("default".into()),
        )
        .expect("set should work");

        assert!(configured_device_name(&config).is_none());
    }

    #[test]
    fn resolve_active_device_prefers_configured_name() {
        let devices = vec![
            InputDeviceEntry {
                id: "0".into(),
                name: "Mic A".into(),
                is_default: true,
                is_selected: false,
            },
            InputDeviceEntry {
                id: "1".into(),
                name: "Mic B".into(),
                is_default: false,
                is_selected: true,
            },
        ];

        let active = resolve_active_device(&devices, Some("Mic B")).expect("active should exist");
        assert_eq!(active.name, "Mic B");
    }

    #[test]
    fn resolve_active_device_uses_default_when_config_is_default() {
        let devices = vec![
            InputDeviceEntry {
                id: "0".into(),
                name: "Mic A".into(),
                is_default: true,
                is_selected: true,
            },
            InputDeviceEntry {
                id: "1".into(),
                name: "Mic B".into(),
                is_default: false,
                is_selected: false,
            },
        ];

        let active = resolve_active_device(&devices, None).expect("default should resolve");
        assert_eq!(active.name, "Mic A");
    }

    #[test]
    fn resolve_active_device_returns_none_when_configured_device_missing() {
        let devices = vec![InputDeviceEntry {
            id: "0".into(),
            name: "Mic A".into(),
            is_default: true,
            is_selected: false,
        }];

        assert!(resolve_active_device(&devices, Some("Missing Mic")).is_none());
    }

    #[test]
    fn resolve_device_name_by_name_returns_matching_name() {
        let devices = vec![
            InputDeviceEntry {
                id: "0".into(),
                name: "Mic A".into(),
                is_default: true,
                is_selected: true,
            },
            InputDeviceEntry {
                id: "1".into(),
                name: "Mic B".into(),
                is_default: false,
                is_selected: false,
            },
        ];

        let selected = resolve_device_name_by_name(&devices, "Mic B").expect("name should resolve");
        assert_eq!(selected, "Mic B");
        assert!(resolve_device_name_by_name(&devices, "Missing").is_err());
    }

    #[test]
    fn resolve_device_name_by_id_returns_matching_name() {
        let devices = vec![
            InputDeviceEntry {
                id: "0".into(),
                name: "Mic A".into(),
                is_default: true,
                is_selected: true,
            },
            InputDeviceEntry {
                id: "1".into(),
                name: "Mic B".into(),
                is_default: false,
                is_selected: false,
            },
        ];

        let selected = resolve_device_name_by_id(&devices, "1").expect("id should resolve");
        assert_eq!(selected, "Mic B");
    }

    #[test]
    fn resolve_device_name_by_id_rejects_invalid_id() {
        let devices = vec![InputDeviceEntry {
            id: "0".into(),
            name: "Mic A".into(),
            is_default: true,
            is_selected: true,
        }];

        assert!(resolve_device_name_by_id(&devices, "abc").is_err());
        assert!(resolve_device_name_by_id(&devices, "7").is_err());
    }
}
