use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use calm_session::control::execution::ExecutionRegistration;
use calm_session::control::execution::{ExecutionReceipt, ExecutionState, PrepareExecution};
use serde::{Deserialize, Serialize};
use workspace::execution::ExecutionId;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal {
    pub version: u32,
    pub request: PrepareExecution,
    pub phase: Phase,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum Phase {
    Prepared,
    Revoked,
    Forking {
        slot_version: u64,
    },
    Spawned(ExecutionReceipt),
    Exited {
        receipt: ExecutionReceipt,
        status: Option<i32>,
    },
    Drained {
        receipt: ExecutionReceipt,
        status: Option<i32>,
        proof: workspace::execution::ScopeDrainProof,
    },
    NoChild {
        slot_version: u64,
        reason: String,
    },
    Unknown {
        slot_version: u64,
        receipt: Option<ExecutionReceipt>,
        reason: String,
    },
}

impl Phase {
    pub fn state(&self) -> ExecutionState {
        match self {
            Self::Prepared => ExecutionState::Prepared,
            Self::Revoked => ExecutionState::Revoked,
            Self::Forking { .. } => ExecutionState::Forking,
            Self::Spawned(_) => ExecutionState::Spawned,
            Self::Exited { status, .. } => ExecutionState::Exited { status: *status },
            Self::Drained { status, .. } => ExecutionState::Drained { status: *status },
            Self::NoChild { reason, .. } => ExecutionState::NoChild {
                reason: reason.clone(),
            },
            Self::Unknown { reason, .. } => ExecutionState::Unknown {
                reason: reason.clone(),
            },
        }
    }
    pub fn receipt(&self) -> Option<&ExecutionReceipt> {
        match self {
            Self::Spawned(receipt)
            | Self::Exited { receipt, .. }
            | Self::Drained { receipt, .. } => Some(receipt),
            Self::Unknown { receipt, .. } => receipt.as_ref(),
            _ => None,
        }
    }
    pub fn slot_version(&self) -> Option<u64> {
        match self {
            Self::Forking { slot_version }
            | Self::NoChild { slot_version, .. }
            | Self::Unknown { slot_version, .. } => Some(*slot_version),
            _ => self.receipt().map(|receipt| receipt.slot_version),
        }
    }
}

pub(super) struct Store {
    root: PathBuf,
    _lock: File,
}

impl Store {
    pub fn open(root: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&root)?;
        anyhow::ensure!(
            !std::fs::symlink_metadata(&root)?.file_type().is_symlink(),
            "journal root is a symlink"
        );
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root.join("executor.lock"))?;
        // SAFETY: the lock file remains owned for the store's full lifetime.
        anyhow::ensure!(
            unsafe {
                libc::flock(
                    std::os::fd::AsRawFd::as_raw_fd(&lock),
                    libc::LOCK_EX | libc::LOCK_NB,
                )
            } == 0,
            "executor journal already owned"
        );
        File::open(&root)?.sync_all()?;
        Ok(Self { root, _lock: lock })
    }
    pub fn load(&self) -> anyhow::Result<Vec<Journal>> {
        self.load_extension("execution")
    }
    pub fn registrations(&self) -> anyhow::Result<Vec<ExecutionRegistration>> {
        self.load_extension("registration")
    }
    fn load_extension<T: serde::de::DeserializeOwned>(
        &self,
        extension: &str,
    ) -> anyhow::Result<Vec<T>> {
        let mut records = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path.extension().is_some_and(|actual| actual == extension) {
                let mut bytes = Vec::new();
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(path)?
                    .take(16 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)?;
                anyhow::ensure!(
                    bytes.len() <= 16 * 1024 * 1024,
                    "oversized execution journal"
                );
                records.push(serde_json::from_slice(&bytes)?);
            }
        }
        Ok(records)
    }
    pub fn save(&self, record: &Journal) -> anyhow::Result<()> {
        self.write(
            &format!(
                "{}.execution",
                record.request.identity.execution_id.as_str()
            ),
            record,
        )
    }
    pub fn write(&self, name: &str, value: &impl Serialize) -> anyhow::Result<()> {
        let temporary = self
            .root
            .join(format!("{}.staging", ExecutionId::fresh()?.as_str()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        serde_json::to_writer(&mut file, value)?;
        file.flush()?;
        file.sync_all()?;
        std::fs::rename(&temporary, self.root.join(name))?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    pub fn path(&self) -> &Path {
        &self.root
    }
}
