use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use calm_session::control::execution::ObjectRegistrationReceipt;
use calm_session::control::execution::{ExecutionReceipt, ExecutionState, PrepareExecution};
use serde::{Deserialize, Serialize};
use workspace::execution::ExecutionId;

// Pre-E1a opt-in journals are intentionally refused; required facts are never backfilled.
pub(super) const JOURNAL_VERSION: u32 = 4;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal {
    pub version: u32,
    pub host_boot_id: String,
    pub output_end: calm_session::control::execution::OutputEnd,
    pub request: PrepareExecution,
    pub phase: Phase,
    pub interactive_revoked: bool,
    /// Volatile IO bookkeeping. Loaded journal facts are already durable; never a wire field.
    #[serde(skip)]
    pub pending_persistence: bool,
}

impl Journal {
    pub fn settle_unspawned_output(&mut self) {
        if self.phase.receipt().is_none()
            && self.phase.state().permits_handoff()
            && self.output_end == calm_session::control::execution::OutputEnd::Open
        {
            self.output_end =
                calm_session::control::execution::OutputEnd::Complete { final_cursor: 0 };
        }
    }
    pub fn durably_settled(&self) -> bool {
        !self.pending_persistence && self.phase.state().permits_handoff()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum Phase {
    Prepared,
    Reserved {
        slot_version: u64,
    },
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
        slot_version: Option<u64>,
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
            Self::Prepared | Self::Reserved { .. } => ExecutionState::Prepared,
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
            Self::Reserved { slot_version }
            | Self::Forking { slot_version }
            | Self::Unknown { slot_version, .. } => Some(*slot_version),
            Self::NoChild { slot_version, .. } => *slot_version,
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
        Self::open_with_barrier(root, sync_ancestry)
    }
    fn open_with_barrier(
        root: PathBuf,
        barrier: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> anyhow::Result<Self> {
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
        barrier(&root)?;
        Ok(Self { root, _lock: lock })
    }
    pub fn load(&self) -> anyhow::Result<Vec<Journal>> {
        self.load_extension("execution")
    }
    pub fn registrations(&self) -> anyhow::Result<Vec<ObjectRegistrationReceipt>> {
        self.load_extension("registration")
    }
    pub fn retirements(&self) -> anyhow::Result<Vec<ObjectRegistrationReceipt>> {
        self.load_extension("retired")
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
                if extension == "execution" {
                    #[derive(Deserialize)]
                    struct Version {
                        version: u32,
                    }
                    let probe: Version = serde_json::from_slice(&bytes)?;
                    anyhow::ensure!(
                        probe.version == JOURNAL_VERSION,
                        "unsupported execution journal version {}; expected {}; pre-E1a journals require a fresh state directory",
                        probe.version,
                        JOURNAL_VERSION
                    );
                }
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
    pub fn output_path(&self, id: &ExecutionId) -> PathBuf {
        self.root.join(format!("{}.output", id.as_str()))
    }
    pub fn path(&self) -> &Path {
        &self.root
    }
}

// Repeat the entire barrier on open: a previous attempt may have created names then failed fsync.
fn sync_ancestry(root: &Path) -> std::io::Result<()> {
    sync_ancestry_with(root, |directory| File::open(directory)?.sync_all())
}
fn sync_ancestry_with(
    root: &Path,
    mut sync: impl FnMut(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    for directory in root.ancestors() {
        let directory = if directory.as_os_str().is_empty() {
            Path::new(".")
        } else {
            directory
        };
        sync(directory)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_e1a_journal_is_explicitly_refused() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("journal")).unwrap();
        // The old format deliberately lacks the new required fields.
        std::fs::write(store.path().join("old.execution"), br#"{"version":3}"#).unwrap();
        let error = store.load().unwrap_err().to_string();
        assert_eq!(
            error,
            "unsupported execution journal version 3; expected 4; pre-E1a journals require a fresh state directory"
        );
        std::fs::write(store.path().join("old.execution"), br#"{"version":4}"#).unwrap();
        assert!(
            store
                .load()
                .unwrap_err()
                .to_string()
                .contains("missing field"),
            "required facts must not default"
        );
    }

    #[test]
    fn execution_journal_bootstrap_retries_every_containing_entry_barrier() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("new-parent/new-store");
        let parent = root.parent().unwrap();
        let mut first = Vec::new();
        let failed = Store::open_with_barrier(root.clone(), |path| {
            sync_ancestry_with(path, |directory| {
                first.push(directory.to_owned());
                if directory == parent {
                    return Err(std::io::Error::from_raw_os_error(libc::EIO));
                }
                File::open(directory)?.sync_all()
            })
        });
        assert!(
            failed.is_err(),
            "store acknowledged before containing entry was durable"
        );
        assert!(root.exists());
        assert_eq!(first, [root.clone(), parent.to_owned()]);
        let mut retried = Vec::new();
        let _store = Store::open_with_barrier(root.clone(), |path| {
            sync_ancestry_with(path, |directory| {
                retried.push(directory.to_owned());
                File::open(directory)?.sync_all()
            })
        })
        .unwrap();
        assert_eq!(
            retried,
            root.ancestors().map(Path::to_owned).collect::<Vec<_>>()
        );
    }
}
