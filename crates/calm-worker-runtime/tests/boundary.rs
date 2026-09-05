#![cfg(target_os = "linux")]
use calm_worker_runtime::{
    BoundaryHandle, BoundaryState, LaunchConfig, Mount, NetworkPolicy, Runtime, RuntimeConfig,
};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Fixture {
    _root: tempfile::TempDir,
    runtime: Runtime,
    config: LaunchConfig,
    handle: Option<BoundaryHandle>,
    init_pin: Option<OwnedFd>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("n1501-boundary-")
            .tempdir()
            .unwrap();
        let workspace = root.path().join("work");
        std::fs::create_dir(&workspace).unwrap();
        let runtime = Runtime::new(RuntimeConfig {
            state_root: root.path().join("state"),
            helper: PathBuf::from(env!("CARGO_BIN_EXE_calm-worker-boundary")),
            bwrap: PathBuf::from("/usr/bin/bwrap"),
            timeout: Duration::from_secs(4),
        })
        .unwrap();
        let config = LaunchConfig {
            attempt_id: "attempt-b".into(),
            network: NetworkPolicy::Provider,
            workspace,
            program: "/provider".into(),
            args: vec!["writer".into()],
            environment: BTreeMap::new(),
            mounts: vec![Mount {
                source: env!("CARGO_BIN_EXE_boundary-test-provider").into(),
                destination: "/provider".into(),
                writable: false,
            }],
        };
        Self {
            _root: root,
            runtime,
            config,
            handle: None,
            init_pin: None,
        }
    }
    fn prepare(&mut self) -> BoundaryHandle {
        let handle = self
            .runtime
            .prepare("run-b", &self.config)
            .unwrap_or_else(|error| {
                let directory = self._root.path().join("state/run-b");
                panic!(
                    "{error}; launcher={:?}; provider={:?}",
                    std::fs::read_to_string(directory.join("launcher.log")),
                    std::fs::read_to_string(directory.join("provider.stderr"))
                );
            });
        self.handle = Some(handle.clone());
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, handle.init.pid, 0) };
        assert!(fd >= 0);
        self.init_pin = Some(unsafe { OwnedFd::from_raw_fd(fd as i32) });
        handle
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            let _ = self.runtime.stop(handle, Duration::from_secs(3));
        }
        // Retain cleanup ownership even when a test deliberately removes metadata.
        if let Some(fd) = &self.init_pin {
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
    }
}
fn wait_file(path: &Path) {
    let until = Instant::now() + Duration::from_secs(3);
    while !path.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(path.exists(), "missing {}", path.display());
}
fn size(path: &Path) -> u64 {
    std::fs::metadata(path).unwrap().len()
}

#[test]
fn boundary_prepare_requires_positive_start_and_preserves_stdio() {
    let mut f = Fixture::new();
    let h = f.prepare();
    assert_eq!(f.runtime.probe(&h).unwrap(), BoundaryState::Prepared);
    std::thread::sleep(Duration::from_millis(120));
    assert!(!f.config.workspace.join("started").exists());
    let mut stream = f.runtime.connect_stdio(&h).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    f.runtime.start(&h).unwrap();
    stream.write_all(b"hello\n").unwrap();
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).unwrap();
    assert_eq!(reply, "hello\n");
    assert!(matches!(
        f.runtime.stop(&h, Duration::from_secs(3)).unwrap(),
        BoundaryState::Quiesced(_)
    ));
}

#[test]
fn boundary_stop_reaps_setsid_descendants_without_stopping_sibling() {
    let mut a = Fixture::new();
    let mut b = Fixture::new();
    let ah = a.prepare();
    let bh = b.prepare();
    a.runtime.start(&ah).unwrap();
    b.runtime.start(&bh).unwrap();
    let ap = a.config.workspace.join("beats");
    let bp = b.config.workspace.join("beats");
    wait_file(&ap);
    wait_file(&bp);
    let a_before = size(&ap);
    assert!(matches!(
        b.runtime.stop(&bh, Duration::from_secs(3)).unwrap(),
        BoundaryState::Quiesced(_)
    ));
    let b_after = size(&bp);
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(size(&bp), b_after);
    assert!(size(&ap) > a_before);
    assert!(!Path::new(&format!("/proc/{}", bh.init.pid)).exists());
}

#[test]
fn boundary_closed_run_never_restarts_on_prepare_or_start_replay() {
    let mut f = Fixture::new();
    let h = f.prepare();
    f.runtime.start(&h).unwrap();
    wait_file(&f.config.workspace.join("started"));
    assert!(matches!(
        f.runtime.stop(&h, Duration::from_secs(3)).unwrap(),
        BoundaryState::Quiesced(_)
    ));
    assert!(f.runtime.start(&h).is_err());
    let replay = f.runtime.prepare("run-b", &f.config).unwrap();
    assert_eq!(replay, h);
    assert!(matches!(
        f.runtime.probe(&replay).unwrap(),
        BoundaryState::Quiesced(_)
    ));
}

#[path = "cases/lifecycle.rs"]
mod lifecycle;

#[path = "cases/network.rs"]
mod network;

#[path = "cases/transport.rs"]
mod transport;
