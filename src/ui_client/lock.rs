use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};

use anyhow::{Context, Result, anyhow};

const UI_INSTANCE_LOCK_FILE: &str = "transcribe-ui.pid";

#[derive(Debug)]
pub struct InstanceLock {
    path: PathBuf,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn acquire_instance_lock(runtime_dir: &Path) -> Result<Option<InstanceLock>> {
    fs::create_dir_all(runtime_dir).with_context(|| {
        format!(
            "failed creating runtime directory for ui lock {}",
            runtime_dir.display()
        )
    })?;

    let lock_path = runtime_dir.join(UI_INSTANCE_LOCK_FILE);

    loop {
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(mut file) => {
                writeln!(file, "{}", process::id()).context("failed writing ui instance lock")?;
                return Ok(Some(InstanceLock { path: lock_path }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if let Some(existing_pid) = read_lock_pid(&lock_path)
                    && is_active_ui_instance(existing_pid)
                {
                    return Ok(None);
                }

                let _ = fs::remove_file(&lock_path);
            }
            Err(error) => {
                return Err(anyhow!(
                    "failed acquiring ui instance lock {}: {error}",
                    lock_path.display()
                ));
            }
        }
    }
}

fn read_lock_pid(path: &Path) -> Option<u32> {
    let raw = fs::read_to_string(path).ok()?;
    raw.trim().parse::<u32>().ok()
}

fn is_active_ui_instance(pid: u32) -> bool {
    let pid_text = pid.to_string();

    // Prefer `ps` so we can treat zombie (`Z`) as dead and verify command name.
    if let Ok(output) = Command::new("ps")
        .args(["-o", "stat=", "-o", "command=", "-p", pid_text.as_str()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        if output.status.success() {
            let row = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if row.is_empty() {
                return false;
            }

            let mut parts = row.split_whitespace();
            let state = parts.next().unwrap_or_default();
            let command = parts.collect::<Vec<_>>().join(" ");

            if state.contains('Z') {
                return false;
            }

            // Ignore unrelated process if PID got reused.
            if !command.contains("omni-transcribe-ui") {
                return false;
            }

            return true;
        }
    }

    Command::new("kill")
        .arg("-0")
        .arg(pid_text)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parse_lock_pid_reads_valid_numbers() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).expect("temp dir should be creatable");
        let lock_file = dir.join("pid.lock");

        fs::write(&lock_file, "12345\n").expect("lock file should be writable");
        assert_eq!(read_lock_pid(&lock_file), Some(12345));

        fs::write(&lock_file, "not-a-pid").expect("lock file should be writable");
        assert_eq!(read_lock_pid(&lock_file), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_lock_is_reclaimed_and_removed_on_drop() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).expect("temp dir should be creatable");
        let lock_file = dir.join(UI_INSTANCE_LOCK_FILE);

        // Use a very large PID to force stale-lock recovery.
        fs::write(&lock_file, "999999\n").expect("stale lock should be writable");

        let lock = acquire_instance_lock(&dir)
            .expect("lock acquisition should succeed")
            .expect("stale lock should be reclaimed");
        assert!(lock_file.exists(), "lock file should exist while held");

        drop(lock);
        assert!(
            !lock_file.exists(),
            "lock file should be removed when guard is dropped"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    fn unique_temp_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();

        std::env::temp_dir().join(format!(
            "omni-ui-lock-test-{}-{}",
            std::process::id(),
            nonce
        ))
    }
}
