//! Out-of-process kernel harness (#840 e1/e2/e3): spawn the shipped
//! `calm-server` binary against an isolated tempdir, wait until it is fully
//! booted, and guarantee cleanup via a SIGKILL-on-drop guard.
//!
//! Extracted behavior-preserving from `tests/kernel_reboot_harness.rs` (e1)
//! so the crash-window slices (e2/e3) can reuse the same spawn/ready-wait/
//! guard machinery. The only extension is the `extra_env` hook: pairs are
//! applied AFTER the e1 baseline allowlist, so a caller can override `PATH`
//! (gh-shim prepend) or add fixture vars (`NEIGE_TRUSTED_FORGE_PLUGINS`,
//! `CALM_TEST_CRASH_AT`) without weakening the env_clear isolation story.
//! e1 passes `&[]` and is byte-equivalent to its pre-extraction behavior.
//!
//! ## Safety
//! This spawns REAL `calm-server` processes, so it is hard-guarded to never
//! touch prod: callers put the DB in a throwaway `tempfile::tempdir()`, the
//! port is a freshly-discovered ephemeral port (asserted `!= 4040`), and
//! codex/claude/supervisor binaries are pointed at non-existent paths so no
//! real agent or shared app-server is ever launched. The child environment is
//! **cleared and rebuilt from a minimal allowlist** (`spawn_kernel`), so no
//! inherited `CALM_*` / `NEIGE_*` / `RECORD_SESSION` var can bleed in and no
//! write can escape the tempdir (`HOME`/`TMPDIR` are redirected into it).
//! Children are killed via a `Drop` guard even on panic. It is CI-safe: no
//! external deps, and callers self-skip if the sandbox denies a loopback bind
//! (`free_port_or_skip` / `launch_kernel` returning `None`).

use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const CALM_SERVER_BIN: &str = env!("CARGO_BIN_EXE_calm-server");

pub const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Kills the spawned child on drop so a panic mid-test never leaks a real
/// `calm-server` process. Explicit `sigkill_and_reap()` is idempotent with
/// this.
pub struct ChildGuard {
    pub child: Child,
    pub port: u16,
}

impl ChildGuard {
    pub fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    /// SIGKILL the kernel and reap it. Mirrors the fault-injection shape: the
    /// out-of-process harness kills at an arbitrary instant while durable state
    /// is live.
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

/// Grab a currently-free ephemeral loopback port by binding `:0` and reading
/// the assigned port back. Returns `None` if the sandbox denies loopback bind
/// (CI-safe skip). The listener is dropped immediately; the port may briefly
/// sit in TIME_WAIT, which is why `launch_kernel` retries on an early bind
/// failure with a fresh port (design fix #4 — driver-side rebind handling,
/// production bind code untouched).
pub fn free_port_or_skip(what: &str) -> Option<u16> {
    match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => Some(listener.local_addr().unwrap().port()),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            eprintln!("skipping kernel reboot harness ({what}): sandbox denied loopback bind: {e}");
            None
        }
        Err(e) => panic!("bind 127.0.0.1:0 for {what}: {e}"),
    }
}

/// Spawn the shipped `calm-server` binary against `tmp` on `port`, with every
/// external dependency (codex/claude/supervisor/plugins/hook-fallback) pointed
/// at throwaway paths inside `tmp` so no real agent, app-server, or prod
/// artifact is ever touched.
///
/// The child environment is **cleared first** (`env_clear`) and rebuilt from a
/// minimal allowlist, so no inherited `CALM_*` / `NEIGE_*` / `RECORD_SESSION`
/// var from the developer's or CI's shell can bleed in — e.g. a stray
/// `RECORD_SESSION` would append outside the tempdir (breaking isolation), and a
/// malformed `CALM_SHARED_CODEX_APPSERVER_RESTART_*` could wedge boot. `HOME`
/// and `TMPDIR` are redirected into `tmp` too, so any path the kernel derives
/// from them also stays inside the throwaway dir.
///
/// `extra_env` pairs are applied LAST, after the baseline allowlist, so
/// callers can override baseline values (e.g. prepend a gh-shim dir to `PATH`)
/// or add fixture vars. e1 passes `&[]`.
pub fn spawn_kernel(
    tmp: &Path,
    db_path: &Path,
    port: u16,
    extra_env: &[(&str, OsString)],
) -> Child {
    // Bare essentials the binary/runtime need. `PATH` is passed through (fallen
    // back to a sane default) so any incidental PATH lookup still resolves;
    // everything else is an explicit fixture value.
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());

    let mut cmd = Command::new(CALM_SERVER_BIN);
    cmd.env_clear()
        // ---- runtime essentials (allowlist) -------------------------------
        .env("PATH", path)
        .env("HOME", tmp)
        .env("TMPDIR", tmp)
        .env("RUST_LOG", "warn,calm_server=info")
        .env("RUST_BACKTRACE", "1")
        // ---- fixture config: DB + listener + all state under `tmp` --------
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
        // Point the agent binaries at non-existent paths: the shared codex
        // app-server start fails fast, `boot_harnesses` swallows it and skips
        // harness recovery — NO real codex is ever launched (prod-safety).
        .env("CALM_CODEX_BIN", tmp.join("no-codex-binary"))
        .env("CALM_CLAUDE_BIN", tmp.join("no-claude-binary"))
        // Dev autologin so boot doesn't panic requiring an owner password; the
        // `/api/version` readiness probe is public regardless.
        .env("CALM_DEV_AUTOLOGIN", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd.spawn().expect("spawn calm-server binary")
}

/// Poll `GET /api/version` until it returns `200` with a `kernelVersion` body,
/// which — because the listener binds only after ALL boot recovery completes —
/// is a strictly-better ready signal than a bare port-open (there is no
/// `/health` route on calm-server). Returns:
///   * `Ok(())` when ready,
///   * `Err(EarlyExit)` if the child died before becoming ready (so the caller
///     can retry on a fresh port for a transient EADDRINUSE),
///   * panics on a hard timeout.
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

/// One-shot `GET /api/version` over raw TCP (no HTTP client dep). Returns true
/// only on a `200` whose body mentions `kernelVersion`.
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

/// Spawn + wait-ready with a driver-side bind-retry loop: pick a fresh free
/// port each attempt so a SIGKILL'd socket sitting in TIME_WAIT (tokio's
/// `TcpListener::bind` sets no `SO_REUSEADDR`) can't wedge the relaunch. This
/// keeps production bind code untouched (design fix #4).
pub fn launch_kernel(
    tmp: &Path,
    db_path: &Path,
    what: &str,
    extra_env: &[(&str, OsString)],
) -> Option<ChildGuard> {
    for attempt in 0..5 {
        let port = free_port_or_skip(what)?;
        assert_ne!(port, 4040, "must never bind the prod calm-server port");
        // Wrap the child in its `ChildGuard` IMMEDIATELY, before waiting on it.
        // `wait_ready` can panic on a hard 30s timeout; because `guard` is a
        // live local, unwinding drops it and `ChildGuard::drop` SIGKILLs+reaps
        // the (possibly hung) kernel — so a wedged boot never leaks a real
        // calm-server holding the tempdir. A bare `Child` would NOT be killed
        // by its own `Drop`, which is the leak this guards against.
        let mut guard = ChildGuard {
            child: spawn_kernel(tmp, db_path, port, extra_env),
            port,
        };
        match wait_ready(&mut guard.child, port) {
            Ok(()) => return Some(guard),
            Err(WaitErr::EarlyExit) => {
                // `guard` drops here and reaps the already-exited child (its
                // `try_wait` sees `Some(status)`, so no redundant kill).
                eprintln!(
                    "{what}: relaunch attempt {attempt} on port {port} exited early; retrying on a fresh port"
                );
            }
        }
    }
    panic!("{what}: calm-server failed to become ready after 5 fresh-port attempts");
}
