//! Physical process evidence; business authorization remains with the caller.
use crate::{BoundaryHandle, Error, LaunchConfig, QuiescenceProof, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Record {
    pub version: u32,
    pub run_id: String,
    pub launch_config: LaunchConfig,
    pub config_digest: String,
    pub helper: std::path::PathBuf,
    pub bwrap: std::path::PathBuf,
    pub token: String,
    pub phase: Phase,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum Phase {
    Reserved,
    Prepared {
        handle: BoundaryHandle,
        launcher_pid: i32,
    },
    Running {
        handle: BoundaryHandle,
        launcher_pid: i32,
    },
    Closing {
        handle: BoundaryHandle,
        launcher_pid: i32,
    },
    Closed(QuiescenceProof),
}

impl Record {
    pub fn handle(&self) -> Option<&BoundaryHandle> {
        match &self.phase {
            Phase::Reserved => None,
            Phase::Prepared { handle, .. }
            | Phase::Running { handle, .. }
            | Phase::Closing { handle, .. } => Some(handle),
            Phase::Closed(proof) => Some(&proof.handle),
        }
    }
    pub fn check_handle(&self, handle: &BoundaryHandle) -> Result<()> {
        if self.version != 1
            || self.handle() != Some(handle)
            || handle.run_id != self.run_id
            || handle.attempt_id != self.launch_config.attempt_id
            || handle.config_digest != self.config_digest
        {
            return Err(Error::Evidence(
                "handle does not match durable identity".into(),
            ));
        }
        Ok(())
    }
}

/// A separate stable inode: atomic record replacement never replaces this lock.
pub(crate) struct Lock(File);
impl Lock {
    pub fn acquire(directory: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.join("lock"))?;
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(libc::EWOULDBLOCK | libc::EINTR)) {
                return Err(error.into());
            }
            if std::time::Instant::now() >= until {
                return Err(Error::Evidence("process evidence lock is busy".into()));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok(Self(file))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub(crate) fn read(directory: &Path) -> Result<Record> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("record.json"))?;
    let mut bytes = Vec::new();
    file.take(1_048_577).read_to_end(&mut bytes)?;
    if bytes.len() > 1_048_576 {
        return Err(Error::Evidence("oversized process record".into()));
    }
    let record: Record = serde_json::from_slice(&bytes)?;
    if record.version != 1 {
        return Err(Error::Evidence("unknown record version".into()));
    }
    Ok(record)
}

pub(crate) fn write(directory: &Path, record: &Record) -> Result<()> {
    let temporary = directory.join(format!("record-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec(record)?)?;
    file.sync_all()?;
    std::fs::rename(&temporary, directory.join("record.json"))?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
