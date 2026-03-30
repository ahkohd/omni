use std::fs;
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn ui_tail_binary_reads_event_line_from_socket() {
    let runtime_dir = unique_temp_path("out");
    fs::create_dir_all(&runtime_dir).expect("runtime dir should be creatable");
    let socket_path = runtime_dir.join("ui.sock");

    let server = thread::spawn({
        let socket_path = socket_path.clone();
        move || {
            if socket_path.exists() {
                let _ = fs::remove_file(&socket_path);
            }

            let listener =
                UnixListener::bind(&socket_path).expect("server should bind ui socket path");
            let (mut stream, _) = listener.accept().expect("server should accept client");

            let line = "{\"v\":1,\"seq\":1,\"at_ms\":1,\"type\":\"ui.show\",\"payload\":{}}\n";
            stream
                .write_all(line.as_bytes())
                .expect("server should write event line");
            stream.flush().expect("server should flush event line");

            thread::sleep(Duration::from_millis(30));
            let _ = fs::remove_file(&socket_path);
        }
    });

    let bin = ui_tail_bin_path();
    let child = Command::new(&bin)
        .arg("--socket")
        .arg(&socket_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("failed spawning {}: {error}", bin.display()));

    let output = wait_with_timeout(child, Duration::from_secs(3));

    server.join().expect("server thread should join");

    assert!(
        output.status.success(),
        "ui-tail exited non-zero\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"type\":\"ui.show\""), "stdout={stdout}");
}

fn ui_tail_bin_path() -> PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_omni-ui-tail") {
        return PathBuf::from(path);
    }

    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join("omni-ui-tail")
}

fn wait_with_timeout(mut child: Child, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;

    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .expect("failed collecting ui-tail output");
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("ui-tail did not exit within {:?}", timeout);
                }
                thread::sleep(Duration::from_millis(15));
            }
            Err(error) => panic!("failed waiting for ui-tail process: {error}"),
        }
    }
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
