//! `/api/cards/:id/terminal` — read-side helpers for terminal cards.
//!
//! The companion write path used to live here (`POST /api/cards/:id/terminal`,
//! the second leg of the 3-step terminal-card recipe) but #13's atomic
//! endpoint replaced it. The single remaining route is the GET that
//! `useTodayTerminal` uses to validate a cached `card_id` from
//! `localStorage` before attempting a WS attach.
//!
//! `spawn_daemon_for` stays public because two other call sites still need
//! it: the new atomic-create handler in `routes::terminal_cards`, the codex
//! route's PTY spawn (`routes::codex`), and the WS attach path's
//! auto-revive (`ws::terminal`).

use crate::db::RouteRepo;
use crate::error::{CalmError, ErrorBody, Result};
use crate::model::Terminal;
use crate::state::{AppState, DaemonClient};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::unix::AsyncFd;

const DAEMON_READY_BACKSTOP: Duration = Duration::from_secs(10);
const DAEMON_READY_SIGNAL: &[u8] = b"ready\n";
const DAEMON_READY_MAX_BYTES: usize = 64;

enum DaemonReadiness {
    Ready,
    Failed {
        error: CalmError,
        child_already_reaped: bool,
    },
}

fn daemon_not_ready(terminal_id: &str, reason: impl std::fmt::Display) -> CalmError {
    CalmError::Internal(format!(
        "daemon for terminal {terminal_id} did not become ready ({reason})"
    ))
}

fn set_fd_nonblocking(fd: i32) -> io::Result<()> {
    // SAFETY: fcntl is called with a live file descriptor and commands
    // that do not require pointer arguments.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: see F_GETFL call above.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn set_fd_cloexec(fd: i32, cloexec: bool) -> io::Result<()> {
    // SAFETY: fcntl is called with a live file descriptor and commands
    // that do not require pointer arguments.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    let next = if cloexec {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    // SAFETY: see F_GETFD call above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, next) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn create_cloexec_pipe() -> io::Result<[OwnedFd; 2]> {
    let mut fds = [0; 2];

    #[cfg(target_os = "linux")]
    {
        // SAFETY: `fds` points at two valid i32 slots for pipe2(2) to fill.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // SAFETY: `fds` points at two valid i32 slots for pipe(2) to fill.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }

    // SAFETY: pipe(2)/pipe2(2) returned two owned descriptors on success.
    let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    // SAFETY: pipe(2)/pipe2(2) returned two owned descriptors on success.
    let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };

    #[cfg(all(unix, not(target_os = "linux")))]
    {
        set_fd_cloexec(read_fd.as_raw_fd(), true)?;
        set_fd_cloexec(write_fd.as_raw_fd(), true)?;
    }

    Ok([read_fd, write_fd])
}

fn ready_pipe() -> io::Result<(AsyncFd<OwnedFd>, OwnedFd)> {
    let [read_fd, write_fd] = create_cloexec_pipe()?;

    // Linux uses atomic pipe2(O_CLOEXEC); other Unix targets use
    // pipe(2)+fcntl(FD_CLOEXEC) on both ends as the portable fallback.
    // Both branches make the descriptors CLOEXEC before spawn setup
    // continues, keeping the cross-spawn inheritance window closed on
    // Linux and as narrow as possible elsewhere. Only the intended child
    // clears CLOEXEC on the write end in `pre_exec`, immediately before exec.
    set_fd_nonblocking(read_fd.as_raw_fd())?;

    Ok((AsyncFd::new(read_fd)?, write_fd))
}

struct ReadySignalScanner {
    buf: Vec<u8>,
}

impl ReadySignalScanner {
    fn new() -> Self {
        Self {
            buf: Vec::with_capacity(16),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> io::Result<bool> {
        let scan_from = self
            .buf
            .len()
            .saturating_sub(DAEMON_READY_SIGNAL.len().saturating_sub(1));
        self.buf.extend_from_slice(bytes);
        if self.buf[scan_from..]
            .windows(DAEMON_READY_SIGNAL.len())
            .any(|w| w == DAEMON_READY_SIGNAL)
        {
            return Ok(true);
        }
        if self.buf.len() > DAEMON_READY_MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ready fd did not contain ready signal",
            ));
        }
        Ok(false)
    }
}

fn with_ready_scanner<T>(
    scanner: &Mutex<ReadySignalScanner>,
    f: impl FnOnce(&mut ReadySignalScanner) -> io::Result<T>,
) -> io::Result<T> {
    let mut scanner = scanner
        .lock()
        .map_err(|_| io::Error::other("ready scanner mutex poisoned"))?;
    f(&mut scanner)
}

fn read_ready_chunk(fd: i32, chunk: &mut [u8]) -> io::Result<usize> {
    loop {
        // SAFETY: `chunk` is a valid writable byte slice and `fd` is a
        // nonblocking pipe read end owned by the AsyncFd wrapper.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

async fn read_ready_signal(
    reader: &AsyncFd<OwnedFd>,
    scanner: &Mutex<ReadySignalScanner>,
) -> io::Result<()> {
    let mut chunk = [0_u8; 16];
    loop {
        let mut guard = reader.readable().await?;
        let n =
            match guard.try_io(|inner| read_ready_chunk(inner.get_ref().as_raw_fd(), &mut chunk)) {
                Ok(result) => result?,
                Err(_would_block) => continue,
            };
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "ready fd closed before ready signal",
            ));
        }
        if with_ready_scanner(scanner, |scanner| scanner.push(&chunk[..n]))? {
            return Ok(());
        }
    }
}

fn drain_ready_signal_now(
    reader: &AsyncFd<OwnedFd>,
    scanner: &Mutex<ReadySignalScanner>,
) -> io::Result<bool> {
    let mut chunk = [0_u8; 16];
    loop {
        match read_ready_chunk(reader.get_ref().as_raw_fd(), &mut chunk) {
            Ok(0) => return Ok(false),
            Ok(n) => {
                if with_ready_scanner(scanner, |scanner| scanner.push(&chunk[..n]))? {
                    return Ok(true);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(e) => return Err(e),
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/cards/{card_id}/terminal", get(get_terminal_for_card))
}

/// Look up the Terminal row a card owns. Returns 404 if the card has no
/// terminal (yet). The UI uses this to validate a card_id cached in
/// localStorage before attempting a WS attach to its terminal.
#[utoipa::path(
    get,
    path = "/api/cards/{card_id}/terminal",
    tag = "terminals",
    params(("card_id" = String, Path, description = "Card id (must be a terminal card)")),
    responses(
        (status = 200, description = "Terminal row for this card", body = Terminal),
        (status = 404, description = "Card has no terminal yet", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_terminal_for_card(
    State(s): State<AppState>,
    Path(card_id): Path<String>,
) -> Result<Json<Terminal>> {
    let term = s
        .repo
        .terminal_get_by_card(&card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("terminal for card {card_id}")))?;
    Ok(Json(term))
}

/// Spawn a `calm-session-daemon` for the given terminal row, wait for its
/// deterministic ready signal, and persist the socket path as the row's
/// `daemon_handle`. Used by `routes::terminal_cards::create_terminal_card`
/// (the atomic-create endpoint), the codex route's PTY spawn, and (when a
/// previously-spawned daemon has died) by the WS handler's auto-revive path.
pub(crate) async fn spawn_daemon_for(
    s: &AppState,
    term: &Terminal,
    program: &str,
    cwd: &str,
    env: &serde_json::Value,
) -> Result<()> {
    spawn_daemon_with_parts(s.daemon.as_ref(), s.repo.as_ref(), term, program, cwd, env).await
}

/// PR6 (#136) — lower-level seam over `spawn_daemon_for` that takes the
/// constituent `DaemonClient` + `&dyn RouteRepo` instead of the full
/// `AppState`. Used by the dispatcher (which doesn't own an `AppState` —
/// it's a kernel-internal worker that ships before AppState exists in
/// the boot order). Identical semantics to `spawn_daemon_for`; the
/// latter is now a one-line forwarder.
pub(crate) async fn spawn_daemon_with_parts(
    daemon: &DaemonClient,
    repo: &dyn RouteRepo,
    term: &Terminal,
    program: &str,
    cwd: &str,
    env: &serde_json::Value,
) -> Result<()> {
    let sock = daemon.sock_path(&term.id);
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CalmError::Internal(format!("mkdir sock parent: {e}")))?;
    }
    // Stale leftover socket file from a previous daemon — must remove or
    // bind() refuses.
    if sock.exists() {
        let _ = std::fs::remove_file(&sock);
    }
    // #306 — GC the previous daemon's `.exit` sidecar at the same
    // moment we unlink its socket: a stale sidecar would otherwise
    // make `resolve_live_sock` mistakenly re-persist the *old* exit
    // info onto a freshly-spawned daemon's row the first time the new
    // daemon's socket goes unreachable. Best-effort; ENOENT is the
    // common case (no prior daemon for this row).
    let _ = std::fs::remove_file(crate::ws::terminal::exit_sidecar_path(&sock));
    let sock_str = sock.to_string_lossy().to_string();
    let (ready_reader, ready_writer) =
        ready_pipe().map_err(|e| CalmError::Internal(format!("create daemon ready pipe: {e}")))?;
    let ready_fd = ready_writer.as_raw_fd();
    let ready_fd_arg = ready_fd.to_string();

    // #177 PR2 — `term.theme_fg/_bg` are the single source of truth
    // for daemon OSC 10/11 reply colors (write-once at row create,
    // NOT NULL post-migration 0017). Thread them onto every daemon
    // spawn so the model can answer codex's startup probe with the
    // host theme's RGB before the first PTY chunk lands. The daemon
    // (PR2 commit 3) bails out fast if either flag is missing — the
    // NOT NULL row invariant should make that unreachable but the
    // belt-and-braces check protects against future kernel-side
    // regressions that forget to thread through this helper.
    let mut cmd = tokio::process::Command::new(&daemon.session_daemon_bin);
    cmd.args(["--id", &term.id])
        .args(["--sock", &sock_str])
        .args(["--terminal-fg", &term.theme_fg])
        .args(["--terminal-bg", &term.theme_bg])
        .args(["--cwd", cwd])
        .args(["--ready-fd", &ready_fd_arg])
        .arg("--")
        .args(["/bin/sh", "-c", program]);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    if let Some(map) = env.as_object() {
        for (k, v) in map {
            if let Some(val) = v.as_str() {
                cmd.env(k, val);
            }
        }
    }
    // SAFETY: this closure runs after fork and before exec in the child.
    // It only calls async-signal-safe `fcntl(2)` operations on the
    // inherited ready pipe fd, so it does not allocate or touch locks.
    unsafe {
        cmd.pre_exec(move || {
            let flags = libc::fcntl(ready_fd, libc::F_GETFD);
            if flags == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(ready_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(false);

    let mut child = cmd
        .spawn()
        .map_err(|e| CalmError::Internal(format!("spawn calm-session-daemon: {e}")))?;
    // Parent must not keep a write end open: if the daemon exits or closes
    // its inherited ready fd without writing `ready\n`, the read end's EOF is
    // the deterministic "never ready" signal.
    drop(ready_writer);
    let pid = child.id();
    tracing::info!(pid = ?pid, terminal_id = %term.id, "spawned calm-session-daemon");
    // Persist the pid so the orphan-terminal sweeper has a SIGTERM fallback
    // target when its graceful `ClientMsg::Kill` path doesn't take. Best-
    // effort: a failed write here is a degraded-cleanup signal but must
    // not abort the spawn (the daemon is running fine — we just lose the
    // SIGTERM lever for that row until the next respawn).
    if let Err(e) = repo.terminal_set_pid(&term.id, pid).await {
        tracing::warn!(
            terminal_id = %term.id,
            pid = ?pid,
            error = %e,
            "failed to persist terminal pid; sweeper will fall back to socket-Kill only"
        );
    }
    // Persist the daemon_handle eagerly — BEFORE awaiting readiness.
    // A one-shot child (e.g. `printf done`) can exit + the daemon can
    // unlink its socket before any attach observes it, so a ready-first
    // persist would leave the row's handle permanently None and the next
    // WS attach would see "no daemon_handle" and 500. With the handle
    // written here, `resolve_live_sock` sees `Some(sock)`, probes, fails
    // (socket gone), and falls into the existing `LiveSock::ChildExited`
    // branch → Close(1000, "child-exited"). The handle stays set on the
    // readiness-error path too: it matches reality (a daemon WAS spawned),
    // and a stuck-daemon hang still surfaces as ChildExited on the next
    // attach, giving the user a Restart button.
    repo.terminal_set_handle(&term.id, Some(&sock_str)).await?;

    let readiness = {
        let ready_scanner = Mutex::new(ReadySignalScanner::new());
        let wait = child.wait();
        let ready = read_ready_signal(&ready_reader, &ready_scanner);
        tokio::pin!(wait);
        tokio::pin!(ready);
        tokio::select! {
            ready_res = &mut ready => {
                match ready_res {
                    Ok(()) => DaemonReadiness::Ready,
                    Err(e) => DaemonReadiness::Failed {
                        error: daemon_not_ready(&term.id, e),
                        child_already_reaped: false,
                    },
                }
            }
            wait_res = &mut wait => {
                match drain_ready_signal_now(&ready_reader, &ready_scanner) {
                    Ok(true) => DaemonReadiness::Ready,
                    Ok(false) => match wait_res {
                        Ok(status) => DaemonReadiness::Failed {
                            error: daemon_not_ready(
                                &term.id,
                                format_args!("exited before ready: {status}"),
                            ),
                            child_already_reaped: true,
                        },
                        Err(e) => DaemonReadiness::Failed {
                            error: daemon_not_ready(
                                &term.id,
                                format_args!("failed to observe child exit: {e}"),
                            ),
                            child_already_reaped: true,
                        },
                    },
                    Err(e) => DaemonReadiness::Failed {
                        error: daemon_not_ready(
                            &term.id,
                            format_args!("read ready fd after child exit: {e}"),
                        ),
                        child_already_reaped: true,
                    },
                }
            }
            _ = tokio::time::sleep(DAEMON_READY_BACKSTOP) => {
                // This is deliberately a backstop, not the readiness judge:
                // normal success is `ready\n`, normal failure is child exit or
                // ready-fd EOF. Only a pathological alive process that never
                // writes ready and never exits reaches this branch. Ten seconds
                // keeps rollback/reap tests and real requests bounded without
                // acting as the normal readiness decision.
                DaemonReadiness::Failed {
                    error: daemon_not_ready(
                        &term.id,
                        format_args!("ready-fd backstop after {DAEMON_READY_BACKSTOP:?}"),
                    ),
                    child_already_reaped: false,
                }
            }
        }
    };
    if let DaemonReadiness::Failed {
        error,
        child_already_reaped,
    } = readiness
    {
        if !child_already_reaped {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
        }
        return Err(error);
    }

    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    //! #177 PR2 — exercise the spawn-time theme plumbing.
    //!
    //! `spawn_daemon_with_parts` is `pub(crate)` so external integration
    //! tests can't reach it directly; we drive it from a unit-test
    //! module here. The `argv-recorder-daemon` fixture (see
    //! `crates/calm-server/tests/fixtures/argv-recorder-daemon/main.rs`)
    //! stands in for the real `calm-session-daemon`: writes the full
    //! argv it received to `<sock>.argv`, binds the socket, then writes
    //! `ready\n` to the inherited ready fd. Asserts the bytes that leave the
    //! spawn site without paying the cost of a real daemon.
    //!
    //! Recorder protocol:
    //!   * Kernel passes `--sock <path>` like the real daemon.
    //!   * Recorder writes `<path>.argv` (one line per argv element),
    //!     binds the unix socket, then writes `ready\n` to `--ready-fd`
    //!     so `spawn_daemon_with_parts` returns Ok.

    use super::*;
    use crate::db::prelude::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::model::{Card, NewCard, NewCove, NewTerminal, NewWave};
    use crate::state::DaemonClient;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// `CARGO_BIN_EXE_argv-recorder-daemon` is only populated for the
    /// integration-test crate (target = test, kind = test). Unit tests
    /// inside `src/` don't get that env var — fall back to the same
    /// relative location Cargo emits the test bin to (`target/debug/`
    /// or `target/release/`). Resolves via `current_exe()`'s parent
    /// chain: the unit-test binary lives at
    /// `target/<profile>/deps/<name>-<hash>`; the recorder binary
    /// lives at `target/<profile>/argv-recorder-daemon`.
    fn locate_recorder_bin() -> PathBuf {
        locate_fixture_bin("argv-recorder-daemon")
    }

    fn locate_ready_exit_bin() -> PathBuf {
        locate_fixture_bin("ready-exit-daemon")
    }

    fn locate_fixture_bin(name: &str) -> PathBuf {
        let env_key = format!("CARGO_BIN_EXE_{name}");
        if let Ok(p) = std::env::var(env_key) {
            return PathBuf::from(p);
        }
        let me = std::env::current_exe().expect("current_exe");
        let target_profile = me
            .parent()
            .and_then(|p| p.parent())
            .expect("test bin parent");
        let candidate = target_profile.join(name);
        if candidate.exists() {
            return candidate;
        }
        panic!(
            "{name} binary not found at {candidate:?}; \
             build the workspace first (`cargo build --tests -p calm-server`)"
        );
    }

    /// Read the recorder's sidecar file written next to `<sock>.argv`.
    /// Recorder writes argv before binding, but we still poll briefly
    /// because some filesystems flush writes lazily under load.
    fn read_argv_lines(sock: &str) -> Vec<String> {
        let argv_path = format!("{sock}.argv");
        let start = Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(&argv_path)
                && !text.is_empty()
            {
                return text.lines().map(String::from).collect();
            }
            if start.elapsed() > Duration::from_secs(3) {
                panic!("argv file {argv_path:?} never appeared / stayed empty");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Mint the minimum DB surface needed to drive
    /// `spawn_daemon_with_parts`: in-memory sqlite + DaemonClient
    /// pointing at the recorder fixture + a card → terminal row pair
    /// with theme.
    async fn boot() -> (Arc<dyn Repo>, Arc<DaemonClient>, tempfile::TempDir, String) {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let repo: Arc<dyn Repo> = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let cove = repo
            .cove_create(NewCove {
                name: "spawn-argv-test".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "spawn-argv-test".into(),
                sort: None,
                cwd: String::new(),
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card: Card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();
        let term = repo
            .terminal_create(NewTerminal {
                card_id: card.id,
                program: "codex".into(),
                cwd: "/".into(),
                env: serde_json::json!({}),
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let daemon = Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            session_daemon_bin: locate_recorder_bin(),
        });
        (repo, daemon, tmp, term.id)
    }

    /// Canonical test: a row created with `default_dark` produces
    /// daemon argv carrying matching `--terminal-fg` / `--terminal-bg`
    /// flags. Pinning the single read path means the dispatcher /
    /// atomic-create / WS auto-revive spawn sites all inherit the
    /// same coverage — they all go through `spawn_daemon_with_parts`.
    #[tokio::test]
    async fn spawn_threads_theme_from_row_onto_daemon_argv() {
        let (repo, daemon, _tmp, term_id) = boot().await;

        let term = repo.terminal_get(&term_id).await.unwrap().expect("row");
        assert_eq!(term.theme_fg, "216,219,226");
        assert_eq!(term.theme_bg, "15,20,24");

        spawn_daemon_with_parts(
            daemon.as_ref(),
            repo.as_ref(),
            &term,
            "codex",
            "/",
            &serde_json::json!({}),
        )
        .await
        .expect("spawn must succeed");

        let sock_path = daemon.sock_path(&term_id);
        let argv = read_argv_lines(&sock_path.to_string_lossy());
        // Pair up consecutive elements so the assertion sees
        // `(flag, value)` — argv-recorder writes one element per line,
        // which matches how `tokio::process::Command` spaces
        // `.args(["--flag", "value"])`.
        let pairs: Vec<(String, String)> = argv
            .windows(2)
            .map(|w| (w[0].clone(), w[1].clone()))
            .collect();
        assert!(
            pairs
                .iter()
                .any(|(k, v)| k == "--terminal-fg" && v == "216,219,226"),
            "spawn must carry --terminal-fg from row; got argv: {argv:?}"
        );
        assert!(
            pairs
                .iter()
                .any(|(k, v)| k == "--terminal-bg" && v == "15,20,24"),
            "spawn must carry --terminal-bg from row; got argv: {argv:?}"
        );
    }

    #[tokio::test]
    async fn read_ready_signal_finds_ready_split_across_chunks() {
        let (reader, writer) = ready_pipe().expect("ready pipe");
        {
            let mut writer = std::fs::File::from(writer);
            writer
                .write_all(b"0123456789abcrea")
                .expect("write first chunk");
            writer.write_all(b"dy\n").expect("write second chunk");
        }

        let scanner = Mutex::new(ReadySignalScanner::new());
        read_ready_signal(&reader, &scanner)
            .await
            .expect("split ready signal must be detected");
    }

    #[tokio::test]
    async fn read_ready_signal_rejects_over_64_bytes_without_ready() {
        let (reader, writer) = ready_pipe().expect("ready pipe");
        {
            let mut writer = std::fs::File::from(writer);
            writer.write_all(&[b'x'; 65]).expect("write garbage");
        }

        let scanner = Mutex::new(ReadySignalScanner::new());
        let err = read_ready_signal(&reader, &scanner)
            .await
            .expect_err("oversized non-ready payload must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn read_ready_signal_reports_eof_before_ready() {
        let (reader, writer) = ready_pipe().expect("ready pipe");
        drop(writer);

        let scanner = Mutex::new(ReadySignalScanner::new());
        let err = read_ready_signal(&reader, &scanner)
            .await
            .expect_err("EOF before ready must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn spawn_treats_ready_then_fast_exit_as_ready() {
        let (repo, daemon, _tmp, term_id) = boot().await;
        let daemon = DaemonClient {
            data_dir: daemon.data_dir.clone(),
            session_daemon_bin: locate_ready_exit_bin(),
        };
        let term = repo.terminal_get(&term_id).await.unwrap().expect("row");

        spawn_daemon_with_parts(
            &daemon,
            repo.as_ref(),
            &term,
            "true",
            "/",
            &serde_json::json!({}),
        )
        .await
        .expect("ready signal must win over immediate child exit");
    }

    /// Fast-exit race regression: when the spawned binary exits
    /// immediately without binding its socket (the user-visible
    /// `printf 'done\n'` reproducer in #267's followup), the readiness
    /// race inside `spawn_daemon_with_parts` returns Err — but the row's
    /// `daemon_handle` MUST already be set so that a subsequent WS
    /// attach can resolve to `LiveSock::ChildExited` and emit
    /// `Close(1000, "child-exited")` instead of falling into the
    /// "no daemon_handle" surfacing a 1006 to the browser.
    ///
    /// Drives the helper with `/bin/true` as `session_daemon_bin`:
    /// `cmd.spawn()` succeeds, the binary ignores all args + exits 0
    /// without binding the socket or writing ready, so the child-exit
    /// arm returns the "did not become ready" error. The
    /// invariant under test is that **the handle is persisted before
    /// the readiness check** — verifying it directly from the row
    /// after the failure returns.
    #[tokio::test]
    async fn spawn_persists_handle_even_when_daemon_exits_before_ready() {
        use crate::db::prelude::*;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let repo: Arc<dyn Repo> = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let cove = repo
            .cove_create(NewCove {
                name: "fast-exit".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "fast-exit".into(),
                sort: None,
                cwd: String::new(),
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card: Card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "terminal".into(),
                sort: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();
        let term = repo
            .terminal_create(NewTerminal {
                card_id: card.id,
                program: "true".into(),
                cwd: "/".into(),
                env: serde_json::json!({}),
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        // `/bin/true` (or `/usr/bin/true`) — accepts any args, exits 0,
        // never binds or writes ready. Drives the child-exit path.
        let true_bin = if std::path::Path::new("/bin/true").exists() {
            PathBuf::from("/bin/true")
        } else {
            PathBuf::from("/usr/bin/true")
        };
        let daemon = Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            session_daemon_bin: true_bin,
        });

        // Drive the spawn — must return Err (child exit before ready).
        let res = spawn_daemon_with_parts(
            daemon.as_ref(),
            repo.as_ref(),
            &term,
            "true",
            "/",
            &serde_json::json!({}),
        )
        .await;
        assert!(
            res.is_err(),
            "expected spawn to fail readiness, but got {res:?}",
        );

        // Invariant: handle was persisted before the readiness race, so even
        // after the error path the row carries it. Without this the
        // next attach falls into the "no daemon_handle" branch and
        // surfaces 1006 to the browser.
        let row = repo.terminal_get(&term.id).await.unwrap().expect("row");
        let expected_sock = daemon.sock_path(&term.id).to_string_lossy().to_string();
        assert_eq!(
            row.daemon_handle.as_deref(),
            Some(expected_sock.as_str()),
            "spawn must persist daemon_handle before the readiness race, \
             so a fast-exit daemon still leaves the row resolvable as \
             ChildExited (not as 'no handle' 1006). row.daemon_handle: \
             {:?}",
            row.daemon_handle,
        );
    }

    /// Sanity: `--id`, `--sock`, the theme flags, `--cwd`, and
    /// `--ready-fd` all
    /// appear on the same argv. Catches a `.args(...)` call that
    /// accidentally lands in a `cmd.env(...)` block.
    #[tokio::test]
    async fn spawn_argv_includes_required_kernel_flags() {
        let (repo, daemon, _tmp, term_id) = boot().await;
        let term = repo.terminal_get(&term_id).await.unwrap().expect("row");

        spawn_daemon_with_parts(
            daemon.as_ref(),
            repo.as_ref(),
            &term,
            "codex",
            "/tmp",
            &serde_json::json!({}),
        )
        .await
        .expect("spawn must succeed");

        let sock_path = daemon.sock_path(&term_id);
        let argv = read_argv_lines(&sock_path.to_string_lossy());
        for expected in &[
            "--id",
            "--sock",
            "--terminal-fg",
            "--terminal-bg",
            "--cwd",
            "--ready-fd",
        ] {
            assert!(
                argv.iter().any(|a| a == expected),
                "argv must carry {expected}; got argv: {argv:?}",
            );
        }
    }
}
