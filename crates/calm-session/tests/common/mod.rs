#![allow(dead_code)] // not every test uses every helper

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

pub struct SupervisorHandle {
    pub child: Child,
    pub sock: PathBuf,
    pub _temp: TempDir,
}

fn locate_bin(name: &str) -> PathBuf {
    let env_key = format!("CARGO_BIN_EXE_{name}");
    if let Ok(path) = std::env::var(env_key) {
        return PathBuf::from(path);
    }
    let me = std::env::current_exe().expect("current_exe");
    let target_profile = me.parent().and_then(Path::parent).expect("test bin parent");
    let candidate = target_profile.join(name);
    if candidate.exists() {
        return candidate;
    }
    panic!("{name} binary not found at {}", candidate.display());
}

/// Spawn `calm-proc-supervisor` as a subprocess and poll until its
/// control socket is reachable. Mirrors the pattern in
/// `calm-proc-supervisor/tests/server_restart_survives.rs`.
pub fn spawn_proc_supervisor() -> SupervisorHandle {
    let temp = tempfile::tempdir().expect("tempdir");
    let sock = temp.path().join("proc-supervisor.sock");
    let mut child = Command::new(locate_bin("calm-proc-supervisor"))
        .args(["--control-sock"])
        .arg(&sock)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn calm-proc-supervisor");
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("try_wait calm-proc-supervisor") {
            panic!("calm-proc-supervisor exited before listening: {status}");
        }
        if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
            return SupervisorHandle {
                child,
                sock,
                _temp: temp,
            };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!(
        "calm-proc-supervisor sock {} never became reachable",
        sock.display()
    );
}

impl Drop for SupervisorHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
