use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const READY_BANNER: &str = "calm-server (replay mode) listening on http://";

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn replay_survives_stdout_pipe_close() {
    let port = ephemeral_port();
    let port_arg = port.to_string();
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();

    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_replay"))
            .current_dir(workspace_root)
            .args([
                "--serve",
                "--file",
                "crates/calm-server/tests/fixtures/events/wave-grid-layout-trace.events.json",
                "--port",
                port_arg.as_str(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn replay binary"),
    );

    let stdout = child.0.stdout.take().expect("child stdout is piped");
    let (ready_tx, ready_rx) = mpsc::channel();
    let reader = thread::spawn(move || read_until_ready(stdout, ready_tx));

    match ready_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!("{e}"),
        Err(_) => panic!("timed out waiting for replay readiness"),
    }
    reader
        .join()
        .expect("stdout reader thread should not panic");

    thread::sleep(Duration::from_millis(50));
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if version_responds(port).unwrap_or(false) {
            return;
        }
        if let Some(status) = child.0.try_wait().expect("poll child status") {
            panic!("replay exited after stdout pipe close: {status}");
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("replay did not answer /api/version after stdout pipe close");
}

fn ephemeral_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind ephemeral port")
        .local_addr()
        .expect("read listener address")
        .port()
}

fn read_until_ready(stdout: std::process::ChildStdout, ready_tx: mpsc::Sender<Result<(), String>>) {
    for line in BufReader::new(stdout).lines() {
        match line {
            Ok(line) if line.contains(READY_BANNER) => {
                let _ = ready_tx.send(Ok(()));
                return;
            }
            Ok(_) => {}
            Err(e) => {
                let _ = ready_tx.send(Err(format!("failed reading stdout: {e}")));
                return;
            }
        }
    }
    let _ = ready_tx.send(Err("stdout closed before ready banner".to_string()));
}

fn version_responds(port: u16) -> std::io::Result<bool> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    stream
        .write_all(b"GET /api/version HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200"))
}
