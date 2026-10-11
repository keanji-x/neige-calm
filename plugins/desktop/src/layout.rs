//! The plugin's files under its working directory `<plugins_data_dir>/<id>/`
//! (the kernel creates it): every directory 0700, `http.sock` 0600.
//!
//! - `run/<process>/`: this process's Wayland socket, a directory per
//!   `desktop` process so a restart never meets an old socket;
//! - `http.sock`: the socket the kernel's WebSocket route dials;
//! - `profiles/chrome/`: Chrome's persistent profile (logins);
//! - `home/`: Chrome's `HOME` (crash database, NSS state).

use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::net::UnixListener;

/// The manifest's `http_socket`.
pub const HTTP_SOCKET: &str = "http.sock";
/// How often, and how far apart, binding retries while an older `desktop`
/// still answers on `http.sock`: a restart overlapping the old process's
/// kill, or an old process stopping Chrome in order, which keeps answering
/// until Chrome is gone. Together longer than that stop (`STOP_WAIT`).
pub const BIND_ATTEMPTS: u32 = 48;
pub const BIND_BACKOFF: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub struct Layout {
    pub work: PathBuf,
    pub run: PathBuf,
    pub http_socket: PathBuf,
    pub profile: PathBuf,
    pub home: PathBuf,
}

impl Layout {
    pub fn new(work: &Path) -> Self {
        let tag = format!("{}-{:08x}", std::process::id(), nonce());
        Self {
            work: work.to_path_buf(),
            run: work.join("run").join(tag),
            http_socket: work.join(HTTP_SOCKET),
            profile: work.join("profiles").join("chrome"),
            home: work.join("home"),
        }
    }

    /// Creates the directories, private even when they already existed, and
    /// removes run directories whose Wayland socket no longer answers.
    /// Chrome's profile and home are made private again by every launch.
    pub fn prepare(&self) -> io::Result<()> {
        let runs = self.run.parent().expect("run dir has a parent");
        for dir in [&self.work, &runs.to_path_buf(), &self.run, &self.home] {
            private_dir(dir)?;
        }
        private_dir(self.profile.parent().expect("profile has a parent"))?;
        private_dir(&self.profile)?;
        remove_dead_runs(runs, &self.run);
        Ok(())
    }
}

/// A time-derived tag that keeps two processes with one pid apart; short,
/// because the socket path must fit `sun_path`.
fn nonce() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ d.as_secs() as u32)
        .unwrap_or(0)
}

/// Creates `dir` if needed and sets it to 0700 either way.
pub fn private_dir(dir: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Removes earlier processes' run directories whose Wayland socket refuses a
/// connect. A live one (an old `desktop` still being killed) is left alone.
fn remove_dead_runs(runs: &Path, ours: &Path) {
    let Ok(entries) = std::fs::read_dir(runs) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if dir == ours || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let socket = dir.join(compositor::SOCKET_NAME);
        if UnixStream::connect(&socket).is_err()
            && let Err(e) = std::fs::remove_dir_all(&dir)
        {
            tracing::debug!(dir = %dir.display(), error = %e, "removing a dead run dir failed");
        }
    }
}

/// Binds `path`, mode 0600. An existing socket file is unlinked only after a
/// connect to it fails; while it still answers (an older `desktop` during a
/// restart), binding is retried `attempts` times, `backoff` apart.
pub async fn bind_http_socket(
    path: &Path,
    attempts: u32,
    backoff: Duration,
) -> io::Result<UnixListener> {
    for attempt in 1..=attempts {
        match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
            Ok(meta) if !meta.file_type().is_socket() => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} exists and is not a socket", path.display()),
                ));
            }
            Ok(_) => {
                if UnixStream::connect(path).is_ok() {
                    if attempt == attempts {
                        break;
                    }
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                std::fs::remove_file(path)?;
            }
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        return Ok(listener);
    }
    Err(io::Error::new(
        io::ErrorKind::AddrInUse,
        format!(
            "{} still answers after {attempts} attempts: another desktop process serves it",
            path.display()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().mode() & 0o777
    }

    /// A new desktop keeps waiting through an old one's whole orderly stop.
    #[test]
    fn binding_outwaits_an_orderly_stop() {
        assert!(BIND_BACKOFF * BIND_ATTEMPTS > crate::STOP_WAIT);
    }

    #[test]
    fn prepare_makes_every_directory_private_even_when_it_existed() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("desktop");
        for dir in [
            work.join("run"),
            work.join("home"),
            work.join("profiles/chrome"),
        ] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o755)).unwrap();
        let layout = Layout::new(&work);
        layout.prepare().unwrap();
        for dir in [
            &work,
            &work.join("run"),
            &layout.run,
            &work.join("home"),
            &work.join("profiles"),
            &work.join("profiles/chrome"),
        ] {
            assert_eq!(mode(dir), 0o700, "{}", dir.display());
        }
    }

    #[test]
    fn a_dead_run_dir_is_removed_and_a_live_one_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("w");
        let dead = work.join("run/1-dead");
        let live = work.join("run/2-live");
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::create_dir_all(&live).unwrap();
        drop(std::os::unix::net::UnixListener::bind(dead.join(compositor::SOCKET_NAME)).unwrap());
        let _held =
            std::os::unix::net::UnixListener::bind(live.join(compositor::SOCKET_NAME)).unwrap();
        Layout::new(&work).prepare().unwrap();
        assert!(!dead.exists());
        assert!(live.exists());
    }

    #[tokio::test]
    async fn the_socket_is_0600_and_a_stale_one_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(HTTP_SOCKET);
        // A socket file whose listener is gone: a connect fails.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        let listener = bind_http_socket(&path, 3, Duration::from_millis(1))
            .await
            .unwrap();
        assert_eq!(mode(&path), 0o600);
        let client = tokio::net::UnixStream::connect(&path);
        let (accepted, connected) = tokio::join!(listener.accept(), client);
        accepted.unwrap();
        connected.unwrap();
    }

    #[tokio::test]
    async fn a_live_socket_is_never_unlinked() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(HTTP_SOCKET);
        let live = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let started = std::time::Instant::now();
        let err = bind_http_socket(&path, 3, Duration::from_millis(20))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        assert!(started.elapsed() >= Duration::from_millis(40));
        // The old process's listener still owns the path.
        let _client = UnixStream::connect(&path).unwrap();
        live.accept().unwrap();
    }

    #[test]
    fn a_non_socket_file_is_refused_not_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(HTTP_SOCKET);
        std::fs::write(&path, b"x").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = runtime
            .block_on(bind_http_socket(&path, 2, Duration::from_millis(1)))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(path.exists());
    }
}
