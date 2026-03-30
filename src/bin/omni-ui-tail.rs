use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "omni-ui-tail",
    version,
    about = "Tail omni UI JSONL events from ui.sock"
)]
struct Cli {
    /// Override ui.sock path.
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Wait and reconnect when socket is unavailable or disconnected.
    #[arg(long)]
    wait: bool,

    /// Retry backoff in milliseconds when --wait is enabled.
    #[arg(long, default_value_t = 250)]
    retry_ms: u64,

    /// Pretty-print each JSON event.
    #[arg(long)]
    pretty: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    let socket_path = resolve_ui_socket_path(cli.socket.as_deref())?;
    let retry = Duration::from_millis(cli.retry_ms.max(10));

    loop {
        let stream = connect_to_socket(&socket_path, cli.wait, retry)?;
        let mut reader = BufReader::new(stream);

        loop {
            let mut line = String::new();
            let bytes = reader
                .read_line(&mut line)
                .with_context(|| format!("failed reading from {}", socket_path.display()))?;

            if bytes == 0 {
                if cli.wait {
                    eprintln!("ui socket disconnected; reconnecting...");
                    break;
                }
                return Ok(());
            }

            emit_line(&line, cli.pretty)?;
        }

        if !cli.wait {
            return Ok(());
        }

        thread::sleep(retry);
    }
}

fn connect_to_socket(path: &Path, wait: bool, retry: Duration) -> Result<UnixStream> {
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(error) if wait => {
                eprintln!("waiting for ui socket {} ({error})...", path.display());
                thread::sleep(retry);
            }
            Err(error) => {
                return Err(anyhow!(
                    "failed connecting ui socket {}: {error}",
                    path.display()
                ));
            }
        }
    }
}

fn emit_line(line: &str, pretty: bool) -> Result<()> {
    let trimmed = line.trim_end();
    if trimmed.is_empty() {
        return Ok(());
    }

    if pretty {
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(value) => println!("{}", serde_json::to_string_pretty(&value)?),
            Err(_) => println!("{trimmed}"),
        }
    } else {
        println!("{trimmed}");
    }

    std::io::stdout().flush().context("failed flushing stdout")
}

fn resolve_ui_socket_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    Ok(resolve_runtime_dir()?.join("ui.sock"))
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

    let home = dirs::home_dir().ok_or_else(|| anyhow!("failed to resolve home directory"))?;
    Ok(home.join(".local").join("state").join("omni"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_ui_socket_path_prefers_explicit_path() {
        let explicit = PathBuf::from("/tmp/omni-ui.sock");
        let resolved = resolve_ui_socket_path(Some(&explicit)).expect("path should resolve");
        assert_eq!(resolved, explicit);
    }
}
