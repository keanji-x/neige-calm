//! Private RMUX host and explicit launch configuration for Neige terminals.
//!
//! This package hosts the upstream daemon in its own process. It does not own
//! Neige card/task identity or interpret pane exits as business completion.
//! Dropping an SDK connection never means shutting down the runtime.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod readiness;

/// Configuration supplied by the owning application, never by a model tool.
/// The socket's parent must already be a private directory owned by this user.
pub struct RuntimeLaunch {
    pub executable: PathBuf,
    pub socket: PathBuf,
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub executable_path: OsString,
    pub locale: OsString,
}

impl RuntimeLaunch {
    /// Build the actual child command with an explicit environment allowlist.
    /// The owner decides when to spawn, supervise, and reap this process.
    pub fn command(&self) -> io::Result<Command> {
        validate_socket_parent(&self.socket)?;
        for path in [&self.executable, &self.cwd, &self.home] {
            if !path.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "runtime paths must be absolute",
                ));
            }
        }
        let mut command = Command::new(&self.executable);
        command
            .arg("--socket")
            .arg(&self.socket)
            .current_dir(&self.cwd);
        command
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.executable_path)
            .env("LANG", &self.locale)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor");
        Ok(command)
    }
}

/// Connect to exactly this runtime. Never discovers or starts a global daemon.
pub async fn connect(socket: &Path, timeout: Duration) -> anyhow::Result<rmux_sdk::Rmux> {
    validate_socket_parent(socket)?;
    tokio::time::timeout(timeout, async {
        let endpoint = socket.to_owned();
        // SDK connect() does not wait for server configuration loading.
        // Use the upstream typed client to observe readiness without invoking
        // SDK connect_or_start(), which could create a replacement process.
        tokio::task::spawn_blocking(move || readiness::wait(&endpoint, timeout)).await??;
        Ok(rmux_sdk::Rmux::builder()
            .unix_socket(socket)
            .default_timeout(timeout)
            .connect()
            .await?)
    })
    .await?
}

/// Run a private daemon with user/global rmux and tmux configuration disabled.
/// The process owner controls the host environment through `RuntimeLaunch`.
#[cfg(unix)]
pub async fn serve(socket: PathBuf) -> io::Result<()> {
    use rustix::fs::{FlockOperation, OFlags, flock};
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    validate_socket_parent(&socket)?;
    let mut lock_path = socket.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .open(lock_path)?;
    // The file stays in place: unlinking a locked file would let a concurrent
    // host lock a different inode. The kernel releases the lease on exit.
    flock(&lock, FlockOperation::NonBlockingLockExclusive)?;
    match std::fs::symlink_metadata(&socket) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "runtime endpoint already exists; refusing to replace it",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // Only this host-owned policy is loaded; no user/global configuration.
    // An empty runtime must stay available for the next Terminal card, and
    // dead panes retain output/exit evidence until Neige explicitly disposes them.
    let mut policy = tempfile::Builder::new()
        .prefix(".neige-rmux-")
        .tempfile_in(socket.parent().unwrap())?;
    policy.write_all(b"set-option -s exit-empty off\nset-option -g remain-on-exit on\n")?;
    policy.flush()?;
    let config = rmux_server::DaemonConfig::new(socket).with_config_files(
        vec![policy.path().to_owned()],
        false,
        None,
    );
    let daemon = rmux_server::ServerDaemon::new(config);
    let result = daemon.bind().await?.wait().await;
    drop(lock);
    result
}

#[cfg(not(unix))]
pub async fn serve(_socket: PathBuf) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Neige terminal runtime currently requires Unix",
    ))
}

#[cfg(unix)]
fn validate_socket_parent(socket: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if !socket.is_absolute() || socket.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime socket must be an absolute file path",
        ));
    }
    let parent = socket
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing socket parent"))?;
    let metadata = std::fs::symlink_metadata(parent)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "runtime socket parent must be a private owned directory, not a symlink",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_socket_parent(_socket: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Neige terminal runtime currently requires Unix",
    ))
}
