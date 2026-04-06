use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use toml::Value;

const CONFIG_FILE_NAME: &str = "config.toml";

pub fn config_path() -> Result<PathBuf> {
    if let Ok(custom) = std::env::var("OMNI_CONFIG_FILE") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let trimmed = xdg.trim();
        if !trimmed.is_empty() {
            return Ok(Path::new(trimmed).join("omni").join(CONFIG_FILE_NAME));
        }
    }

    let home = dirs::home_dir().ok_or_else(|| anyhow!("failed to resolve home directory"))?;
    Ok(home.join(".config").join("omni").join(CONFIG_FILE_NAME))
}

pub fn default_config() -> Value {
    let mut root = toml::map::Map::new();

    let mut server = toml::map::Map::new();
    server.insert("llmApi".into(), Value::String("openai-realtime".into()));
    server.insert(
        "baseUrl".into(),
        Value::String("http://127.0.0.1:8000/v1".into()),
    );
    server.insert("apiKey".into(), Value::String("".into()));
    server.insert("model".into(), Value::String("voxtral".into()));
    root.insert("server".into(), Value::Table(server));

    let mut audio = toml::map::Map::new();
    audio.insert("device".into(), Value::String("default".into()));
    audio.insert("sample_rate".into(), Value::Integer(16_000));
    audio.insert("channels".into(), Value::Integer(1));
    root.insert("audio".into(), Value::Table(audio));

    let mut transcribe_hooks = toml::map::Map::new();
    transcribe_hooks.insert(
        "start".into(),
        Value::Array(vec![Value::String("show_ui".into())]),
    );
    transcribe_hooks.insert(
        "stop".into(),
        Value::Array(vec![
            Value::String("hide_ui".into()),
            Value::String("copy".into()),
        ]),
    );
    transcribe_hooks.insert(
        "stop_insert".into(),
        Value::Array(vec![
            Value::String("hide_ui".into()),
            Value::String("stash".into()),
            Value::String("copy".into()),
            Value::String("paste".into()),
            Value::String("sleep 120".into()),
            Value::String("unstash".into()),
        ]),
    );

    let mut hooks = toml::map::Map::new();
    hooks.insert("transcribe".into(), Value::Table(transcribe_hooks));

    let mut event = toml::map::Map::new();
    event.insert("hooks".into(), Value::Table(hooks));
    root.insert("event".into(), Value::Table(event));

    Value::Table(root)
}

pub fn load_config() -> Result<Value> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(default_config());
    }

    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed reading config file: {}", path.display()))?;

    toml::from_str::<Value>(&text)
        .with_context(|| format!("failed parsing TOML config: {}", path.display()))
}

pub fn save_config(config: &Value) -> Result<PathBuf> {
    let path = config_path()?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed creating config directory: {}", parent.display()))?;
    }

    let text = toml::to_string_pretty(config).context("failed serializing TOML config")?;
    fs::write(&path, text)
        .with_context(|| format!("failed writing config file: {}", path.display()))?;

    Ok(path)
}

pub fn get_value_by_key<'a>(config: &'a Value, key: &str) -> Option<&'a Value> {
    let Ok(parts) = key_parts(key) else {
        return None;
    };

    let mut cursor = config;
    for part in parts {
        match cursor {
            Value::Table(table) => cursor = table.get(part)?,
            _ => return None,
        }
    }

    Some(cursor)
}

pub fn set_value_by_key(config: &mut Value, key: &str, value: Value) -> Result<()> {
    let parts = key_parts(key)?;
    let mut cursor = config;

    for part in &parts[..parts.len() - 1] {
        let table = ensure_table(cursor);
        cursor = table
            .entry((*part).to_string())
            .or_insert_with(|| Value::Table(toml::map::Map::new()));
    }

    let table = ensure_table(cursor);
    table.insert(parts[parts.len() - 1].to_string(), value);

    Ok(())
}

pub fn unset_value_by_key(config: &mut Value, key: &str) -> Result<bool> {
    let parts = key_parts(key)?;
    let mut cursor = config;

    for part in &parts[..parts.len() - 1] {
        match cursor {
            Value::Table(table) => {
                let Some(next) = table.get_mut(*part) else {
                    return Ok(false);
                };
                cursor = next;
            }
            _ => return Ok(false),
        }
    }

    match cursor {
        Value::Table(table) => Ok(table.remove(parts[parts.len() - 1]).is_some()),
        _ => Ok(false),
    }
}

pub fn parse_set_value(raw: &str, json_value: bool) -> Result<Value> {
    if !json_value {
        return Ok(Value::String(raw.to_string()));
    }

    let json = serde_json::from_str::<serde_json::Value>(raw)
        .with_context(|| format!("invalid JSON for --json-value: {raw}"))?;

    json_to_toml_value(json)
}

fn key_parts(key: &str) -> Result<Vec<&str>> {
    let parts: Vec<&str> = key
        .split('.')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        bail!("config key cannot be empty");
    }

    Ok(parts)
}

fn ensure_table(value: &mut Value) -> &mut toml::map::Map<String, Value> {
    if !matches!(value, Value::Table(_)) {
        *value = Value::Table(toml::map::Map::new());
    }

    match value {
        Value::Table(table) => table,
        _ => unreachable!(),
    }
}

fn json_to_toml_value(json: serde_json::Value) -> Result<Value> {
    match json {
        serde_json::Value::Null => bail!("null is not representable in TOML"),
        serde_json::Value::Bool(v) => Ok(Value::Boolean(v)),
        serde_json::Value::Number(v) => {
            if let Some(i) = v.as_i64() {
                return Ok(Value::Integer(i));
            }
            if let Some(f) = v.as_f64() {
                return Ok(Value::Float(f));
            }
            bail!("unsupported number value: {v}")
        }
        serde_json::Value::String(v) => Ok(Value::String(v)),
        serde_json::Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                out.push(json_to_toml_value(item)?);
            }
            Ok(Value::Array(out))
        }
        serde_json::Value::Object(map) => {
            let mut out = toml::map::Map::new();
            for (k, v) in map {
                out.insert(k, json_to_toml_value(v)?);
            }
            Ok(Value::Table(out))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn set_get_unset_works_with_dot_keys() {
        let mut config = default_config();

        set_value_by_key(
            &mut config,
            "event.hooks.transcribe.stop_copy",
            Value::Array(vec![Value::String("copy".into())]),
        )
        .expect("set should work");

        let value = get_value_by_key(&config, "event.hooks.transcribe.stop_copy")
            .expect("value should exist");
        assert!(matches!(value, Value::Array(_)));

        let removed = unset_value_by_key(&mut config, "event.hooks.transcribe.stop_copy")
            .expect("unset should succeed");
        assert!(removed);

        assert!(get_value_by_key(&config, "event.hooks.transcribe.stop_copy").is_none());
    }

    #[test]
    fn parse_set_value_json_value_parses_arrays() {
        let value =
            parse_set_value("[\"hide_ui\",\"copy\"]", true).expect("json parse should work");
        match value {
            Value::Array(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].as_str(), Some("hide_ui"));
                assert_eq!(items[1].as_str(), Some("copy"));
            }
            _ => panic!("expected array value"),
        }
    }

    #[test]
    fn parse_set_value_string_is_literal_without_json_flag() {
        let value = parse_set_value("true", false).expect("literal set should work");
        assert_eq!(value.as_str(), Some("true"));
    }

    #[test]
    fn saved_config_can_be_loaded_back() {
        let _guard = ENV_LOCK.lock().expect("env lock should not be poisoned");

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("omni-config-test-{nonce}.toml"));

        unsafe {
            std::env::set_var("OMNI_CONFIG_FILE", &path);
        }

        let mut config = default_config();
        set_value_by_key(
            &mut config,
            "server.baseUrl",
            Value::String("http://100.101.223.8:1237/v1".into()),
        )
        .expect("set should work");

        save_config(&config).expect("save should work");
        let loaded = load_config().expect("load should work");

        assert_eq!(
            get_value_by_key(&loaded, "server.baseUrl").and_then(|v| v.as_str()),
            Some("http://100.101.223.8:1237/v1")
        );

        let _ = std::fs::remove_file(&path);
        unsafe {
            std::env::remove_var("OMNI_CONFIG_FILE");
        }
    }
}
