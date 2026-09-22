//! Out-of-process kernel harness: spawn the shipped `calm-server` binary against an isolated
//! tempdir, wait until it is fully booted, and guarantee cleanup via a SIGKILL-on-drop guard.

use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub const CALM_SERVER_BIN: &str = env!("CARGO_BIN_EXE_calm-server");

pub const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Kills the spawned child on drop so a panic mid-test never leaks a real `calm-server` process.
pub struct ChildGuard {
    pub child: Child,
    pub port: u16,
}

impl ChildGuard {
    pub fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    pub fn sigkill_and_reap(&mut self) {
        // SAFETY: plain libc kill on our own child pid.
        unsafe {
            libc::kill(self.pid(), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Best-effort: if the test already reaped it this is a no-op.
        if let Ok(None) = self.child.try_wait() {
            unsafe {
                libc::kill(self.pid(), libc::SIGKILL);
            }
            let _ = self.child.wait();
        }
    }
}

/// Grab a currently-free ephemeral loopback port; `None` if the sandbox denies loopback bind.
/// The listener is dropped immediately, so the port may briefly sit in TIME_WAIT.
pub fn free_port_or_skip(what: &str) -> Option<u16> {
    match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => Some(listener.local_addr().unwrap().port()),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            // The test then returns green having verified nothing: a hosted CI runner's sandbox
            // takes this branch, the self-hosted runner never does.
            eprintln!("SKIP: kernel reboot harness ({what}) (loopback bind denied): {e}");
            None
        }
        Err(e) => panic!("bind 127.0.0.1:0 for {what}: {e}"),
    }
}

/// Spawn the shipped `calm-server` binary against `tmp` on `port`. The child environment is cleared
/// and rebuilt from a minimal allowlist so nothing inherited bleeds in; `extra_env` is applied last.
pub fn spawn_kernel(
    tmp: &Path,
    db_path: &Path,
    port: u16,
    extra_env: &[(&str, OsString)],
) -> Child {
    spawn_kernel_to(tmp, db_path, port, extra_env, None)
}

/// A handle on `log` (appending) for one of the kernel's output streams.
fn log_stdio(log: &Path) -> Stdio {
    Stdio::from(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .expect("open the kernel log file"),
    )
}

/// [`spawn_kernel`] with the kernel's stdout and stderr appended to `log` when given, for a
/// test that asserts on the kernel's own lines: tracing writes to stdout (the boot recovery
/// plan), the crash seam's abort notice goes to stderr.
pub fn spawn_kernel_to(
    tmp: &Path,
    db_path: &Path,
    port: u16,
    extra_env: &[(&str, OsString)],
    log: Option<&Path>,
) -> Child {
    let (stdout, stderr) = match log {
        Some(log) => (log_stdio(log), log_stdio(log)),
        None => (Stdio::inherit(), Stdio::inherit()),
    };
    // `PATH` is passed through so incidental lookups still resolve; everything else is an explicit fixture value.
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());

    let mut cmd = Command::new(CALM_SERVER_BIN);
    cmd.env_clear()
        .env("PATH", path)
        .env("HOME", tmp)
        .env("TMPDIR", tmp)
        .env("RUST_LOG", "warn,calm_server=info")
        .env("RUST_BACKTRACE", "1")
        .env(
            "CALM_DB_URL",
            format!("sqlite://{}?mode=rwc", db_path.display()),
        )
        .env("CALM_LISTEN", format!("127.0.0.1:{port}"))
        .env("CALM_DATA_DIR", tmp.join("data"))
        .env("CALM_PLUGINS_DIR", tmp.join("plugins"))
        .env("CALM_PLUGINS_DATA_DIR", tmp.join("plugins-data"))
        .env("CALM_PROC_SUPERVISOR_SOCK", tmp.join("no-supervisor.sock"))
        .env(
            "CALM_SHARED_CODEX_APPSERVER_LOG_DIR",
            tmp.join("codex-logs"),
        )
        .env("NEIGE_HOOK_FALLBACK_DIR", tmp.join("hook-fallback"))
        // Non-existent agent binaries: the shared codex app-server start fails fast, so NO real codex is ever launched.
        .env("CALM_CODEX_BIN", tmp.join("no-codex-binary"))
        .env("CALM_CLAUDE_BIN", tmp.join("no-claude-binary"))
        // Dev autologin so boot doesn't panic requiring an owner password; the
        // `/api/version` readiness probe is public regardless.
        .env("CALM_DEV_AUTOLOGIN", "true")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    // RLIMIT_CORE=0: e2/e3 deliberately abort the kernel (SIGABRT); on hosts
    // with a nonzero core limit that would otherwise dump a core file per run.
    // SAFETY: setrlimit is async-signal-safe; nothing else runs pre-exec.
    unsafe {
        cmd.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_CORE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().expect("spawn calm-server binary")
}

/// Block until the kernel exits on its own, reap it, and return the `ExitStatus`; panics on `timeout`.
pub fn wait_exit_with_timeout(guard: &mut ChildGuard, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        match guard.child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {}
            Err(e) => panic!("wait for kernel exit: {e}"),
        }
        if Instant::now() >= deadline {
            panic!("kernel did not exit within {timeout:?} (expected a fixture-induced crash)");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Poll `GET /api/version` until `200` with a `kernelVersion` body: the listener binds only after
/// ALL boot recovery completes, so this is a stricter ready signal than a bare port-open.
pub enum WaitErr {
    EarlyExit,
}

pub fn wait_ready(child: &mut Child, port: u16) -> Result<(), WaitErr> {
    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            eprintln!("calm-server exited before ready: {status:?}");
            return Err(WaitErr::EarlyExit);
        }
        if probe_version_ok(port) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("calm-server never became ready on port {port} within {READY_TIMEOUT:?}");
}

/// One-shot `GET /api/version` over raw TCP; true only on a `200` whose body mentions `kernelVersion`.
pub fn probe_version_ok(port: u16) -> bool {
    let addr = format!("127.0.0.1:{port}");
    let Ok(mut stream) = TcpStream::connect(&addr) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let req = "GET /api/version HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = String::new();
    if stream.read_to_string(&mut buf).is_err() {
        return false;
    }
    let status_line = buf.lines().next().unwrap_or_default();
    status_line.contains("200") && buf.contains("kernelVersion")
}

/// Spawn + wait-ready with a driver-side bind-retry loop: a fresh free port each attempt, since a
/// SIGKILL'd socket in TIME_WAIT (tokio's bind sets no `SO_REUSEADDR`) can wedge the relaunch.
pub fn launch_kernel(
    tmp: &Path,
    db_path: &Path,
    what: &str,
    extra_env: &[(&str, OsString)],
) -> Option<ChildGuard> {
    launch_kernel_to(tmp, db_path, what, extra_env, None)
}

/// [`launch_kernel`] with the kernel's stdout and stderr appended to `log` when given (every
/// relaunch attempt appends to the same file).
pub fn launch_kernel_to(
    tmp: &Path,
    db_path: &Path,
    what: &str,
    extra_env: &[(&str, OsString)],
    log: Option<&Path>,
) -> Option<ChildGuard> {
    for attempt in 0..5 {
        let port = free_port_or_skip(what)?;
        assert_ne!(port, 4040, "must never bind the prod calm-server port");
        // Wrap the child in its `ChildGuard` IMMEDIATELY: `wait_ready` can panic, and only the guard's
        // `Drop` SIGKILLs the (possibly hung) kernel — a bare `Child` would leak it.
        let mut guard = ChildGuard {
            child: spawn_kernel_to(tmp, db_path, port, extra_env, log),
            port,
        };
        match wait_ready(&mut guard.child, port) {
            Ok(()) => return Some(guard),
            Err(WaitErr::EarlyExit) => {
                eprintln!(
                    "{what}: relaunch attempt {attempt} on port {port} exited early; retrying on a fresh port"
                );
            }
        }
    }
    panic!("{what}: calm-server failed to become ready after 5 fresh-port attempts");
}
