use std::io::Write;
use std::process::{Command, Stdio};

#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::{Result, bail};

#[derive(Debug, Default)]
pub struct ClipboardSession {
    stashed: Option<String>,
}

impl ClipboardSession {
    pub fn execute_builtin(&mut self, action: &str, transcript: &str) -> Result<()> {
        match action {
            "stash" => self.stash(),
            "copy" => self.copy(transcript),
            "paste" => self.paste(),
            "unstash" => self.unstash(),
            other => bail!("unsupported clipboard builtin: {other}"),
        }
    }

    fn stash(&mut self) -> Result<()> {
        self.stashed = Some(read_clipboard()?);
        Ok(())
    }

    fn copy(&mut self, transcript: &str) -> Result<()> {
        write_clipboard(transcript)
    }

    fn paste(&mut self) -> Result<()> {
        paste_clipboard()
    }

    fn unstash(&mut self) -> Result<()> {
        let Some(previous) = self.stashed.take() else {
            return Ok(());
        };

        write_clipboard(&previous)
    }
}

fn read_clipboard() -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("pbpaste")
            .output()
            .context("failed running pbpaste")?;
        if !output.status.success() {
            bail!("pbpaste failed with status {}", output.status);
        }

        return String::from_utf8(output.stdout).context("clipboard contains non-utf8 data");
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return read_clipboard_unix();
    }

    #[cfg(not(unix))]
    {
        bail!("clipboard stash is currently only implemented on macOS and Linux/WSL")
    }
}

fn write_clipboard(value: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut child = Command::new("pbcopy")
            .stdin(Stdio::piped())
            .spawn()
            .context("failed launching pbcopy")?;

        {
            let Some(stdin) = child.stdin.as_mut() else {
                bail!("failed opening stdin for pbcopy");
            };
            stdin
                .write_all(value.as_bytes())
                .context("failed writing clipboard payload")?;
        }

        let status = child.wait().context("failed waiting for pbcopy")?;
        if !status.success() {
            bail!("pbcopy failed with status {status}");
        }

        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return write_clipboard_unix(value);
    }

    #[cfg(not(unix))]
    {
        let _ = value;
        bail!("clipboard copy/unstash is currently only implemented on macOS and Linux/WSL")
    }
}

fn paste_clipboard() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("osascript")
            .arg("-e")
            .arg("tell application \"System Events\" to keystroke \"v\" using command down")
            .status()
            .context("failed launching osascript for paste")?;

        if !status.success() {
            bail!("osascript paste failed with status {status}");
        }

        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return paste_clipboard_unix();
    }

    #[cfg(not(unix))]
    {
        bail!("clipboard paste is currently only implemented on macOS and Linux/WSL")
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
#[derive(Debug)]
enum CommandAttemptError {
    NotFound,
    Failed(String),
}

#[cfg(all(unix, not(target_os = "macos")))]
fn read_clipboard_unix() -> Result<String> {
    let candidates: [(&str, &[&str]); 2] = [
        ("wl-paste", &["--no-newline"]),
        ("xclip", &["-selection", "clipboard", "-o"]),
    ];

    let mut failures: Vec<String> = Vec::new();

    for (program, args) in candidates {
        match run_capture(program, args) {
            Ok(value) => return Ok(value),
            Err(CommandAttemptError::NotFound) => continue,
            Err(CommandAttemptError::Failed(message)) => {
                failures.push(format!("{program}: {message}"));
            }
        }
    }

    if failures.is_empty() {
        bail!("clipboard stash requires wl-paste or xclip in PATH")
    }

    bail!(
        "failed reading clipboard via Linux providers: {}",
        failures.join("; ")
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn write_clipboard_unix(value: &str) -> Result<()> {
    let candidates: [(&str, &[&str]); 2] =
        [("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])];

    let mut failures: Vec<String> = Vec::new();

    for (program, args) in candidates {
        match run_with_stdin(program, args, value) {
            Ok(()) => return Ok(()),
            Err(CommandAttemptError::NotFound) => continue,
            Err(CommandAttemptError::Failed(message)) => {
                failures.push(format!("{program}: {message}"));
            }
        }
    }

    if failures.is_empty() {
        bail!("clipboard copy/unstash requires wl-copy or xclip in PATH")
    }

    bail!(
        "failed writing clipboard via Linux providers: {}",
        failures.join("; ")
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn paste_clipboard_unix() -> Result<()> {
    // We emit a standard Ctrl+V paste keystroke into the active window.
    // - Wayland: wtype
    // - X11: xdotool
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = std::env::var_os("DISPLAY").is_some();

    let mut candidates: Vec<(&str, &[&str])> = Vec::new();
    if wayland {
        candidates.push(("wtype", &["-M", "ctrl", "-k", "v", "-m", "ctrl"]));
        candidates.push(("xdotool", &["key", "--clearmodifiers", "ctrl+v"]));
    } else if x11 {
        candidates.push(("xdotool", &["key", "--clearmodifiers", "ctrl+v"]));
        candidates.push(("wtype", &["-M", "ctrl", "-k", "v", "-m", "ctrl"]));
    } else {
        candidates.push(("wtype", &["-M", "ctrl", "-k", "v", "-m", "ctrl"]));
        candidates.push(("xdotool", &["key", "--clearmodifiers", "ctrl+v"]));
    }

    let mut failures: Vec<String> = Vec::new();

    for (program, args) in candidates {
        match run_status(program, args) {
            Ok(()) => return Ok(()),
            Err(CommandAttemptError::NotFound) => continue,
            Err(CommandAttemptError::Failed(message)) => {
                failures.push(format!("{program}: {message}"));
            }
        }
    }

    if failures.is_empty() {
        bail!("clipboard paste requires wtype (Wayland) or xdotool (X11) in PATH")
    }

    bail!(
        "failed issuing Linux paste keystroke: {}",
        failures.join("; ")
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn run_capture(program: &str, args: &[&str]) -> std::result::Result<String, CommandAttemptError> {
    let output = Command::new(program).args(args).output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CommandAttemptError::NotFound
        } else {
            CommandAttemptError::Failed(format!("spawn failed: {error}"))
        }
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CommandAttemptError::Failed(format!(
            "status {} ({})",
            output.status,
            stderr.trim()
        )));
    }

    String::from_utf8(output.stdout)
        .map_err(|error| CommandAttemptError::Failed(format!("non-utf8 output: {error}")))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn run_with_stdin(
    program: &str,
    args: &[&str],
    input: &str,
) -> std::result::Result<(), CommandAttemptError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CommandAttemptError::NotFound
            } else {
                CommandAttemptError::Failed(format!("spawn failed: {error}"))
            }
        })?;

    {
        let Some(stdin) = child.stdin.as_mut() else {
            return Err(CommandAttemptError::Failed(
                "failed opening stdin".to_string(),
            ));
        };

        stdin
            .write_all(input.as_bytes())
            .map_err(|error| CommandAttemptError::Failed(format!("stdin write failed: {error}")))?;
    }

    let status = child
        .wait()
        .map_err(|error| CommandAttemptError::Failed(format!("wait failed: {error}")))?;

    if status.success() {
        Ok(())
    } else {
        Err(CommandAttemptError::Failed(format!("status {status}")))
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn run_status(program: &str, args: &[&str]) -> std::result::Result<(), CommandAttemptError> {
    let status = Command::new(program).args(args).status().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CommandAttemptError::NotFound
        } else {
            CommandAttemptError::Failed(format!("spawn failed: {error}"))
        }
    })?;

    if status.success() {
        Ok(())
    } else {
        Err(CommandAttemptError::Failed(format!("status {status}")))
    }
}
