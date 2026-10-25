//! C5 location admission: one nonwaiting fence shared by terminal and probe owners.
use super::{Operation, PreparedTx, PreparedTxResult, ProviderAdapter, Tx};
use crate::{
    error::{CalmError, Result},
    per_card_lock::{KeyedLockGuard, KeyedLocks, try_lock_key},
};
use calm_truth::db::sqlite::path_location::{PathLocation, paths_overlap};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub struct LocationAdmission {
    pub managed_root: PathBuf,
    pub objective_checkout_root: PathBuf,
    pub teardown_targets: workspace_runtime::sweep::TeardownTargets,
    pub area_locks: KeyedLocks,
    pub track_locks: KeyedLocks,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Owner {
    Area(String),
    Track(String),
}

impl LocationAdmission {
    /// Internal resources never admit a caller-selected execution root. Other tracks'
    /// directories remain valid cwd choices when their lifecycle locks are free.
    fn location<'a>(&self, cwd: &'a Path) -> Result<PathLocation<'a>> {
        let location = PathLocation::allow_missing(cwd)?;
        for root in [
            self.managed_root.join(workspace::recycle::TRASH_DIR_NAME),
            self.objective_checkout_root.clone(),
        ] {
            if paths_overlap(&location, &root, false)? {
                return Err(CalmError::BadRequest(format!(
                    "explicit cwd {} overlaps Neige-owned location {}",
                    cwd.display(),
                    root.display()
                )));
            }
        }
        Ok(location)
    }

    async fn owners(
        &self,
        conn: &mut sqlx::SqliteConnection,
        cwd: &Path,
    ) -> Result<BTreeSet<Owner>> {
        let location = self.location(cwd)?;
        let mut owners = BTreeSet::new();
        let managed_locations = if paths_overlap(&location, &self.managed_root, false)? {
            location.relative_to(&self.managed_root)?
        } else {
            Vec::new()
        };
        for relative in managed_locations {
            let mut components = relative.iter();
            if let Some(area) = components.next() {
                let area = area.to_str().ok_or_else(|| {
                    CalmError::BadRequest("managed area identity is not UTF-8".into())
                })?;
                owners.insert(Owner::Area(area.into()));
                if let Some(track) = components.next() {
                    let track = track.to_str().ok_or_else(|| {
                        CalmError::BadRequest("managed track identity is not UTF-8".into())
                    })?;
                    owners.insert(Owner::Track(track.into()));
                }
            }
        }
        for owned in calm_truth::db::sqlite::owned_workspace_locations(conn)
            .await?
            .into_iter()
            .chain(
                self.teardown_targets
                    .snapshot_with_owners()
                    .into_iter()
                    .map(
                        |(track_id, path)| calm_truth::db::sqlite::OwnedWorkspaceLocation {
                            track_id,
                            path,
                            removing: false,
                        },
                    ),
            )
        {
            if paths_overlap(&location, &owned.path, false)? {
                if owned.removing {
                    return Err(CalmError::WorkspaceBusy(owned.track_id));
                }
                owners.insert(Owner::Track(owned.track_id));
            }
        }
        Ok(owners)
    }

    /// The caller may already hold its own track fence. Every other lock is tried,
    /// never awaited; a Busy return drops all guards acquired by this invocation.
    pub(crate) async fn try_fence(
        &self,
        conn: &mut sqlx::SqliteConnection,
        cwd: &Path,
        held_track: Option<&str>,
    ) -> Result<LocationFence> {
        let owners = self.owners(conn, cwd).await?;
        let mut guards = Vec::new();
        for owner in &owners {
            let (locks, key) = match owner {
                Owner::Track(track) if held_track == Some(track.as_str()) => continue,
                Owner::Track(track) => (&self.track_locks, track),
                Owner::Area(area) => (&self.area_locks, area),
            };
            guards.push(
                try_lock_key(locks, key).ok_or_else(|| CalmError::WorkspaceBusy(key.clone()))?,
            );
        }
        Ok(LocationFence {
            config: self.clone(),
            cwd: cwd.into(),
            owners,
            _guards: guards,
        })
    }
}

pub(crate) struct LocationFence {
    config: LocationAdmission,
    cwd: PathBuf,
    owners: BTreeSet<Owner>,
    _guards: Vec<KeyedLockGuard>,
}
impl LocationFence {
    pub(crate) async fn recheck(&self, tx: &mut Tx<'_>) -> Result<()> {
        if self.config.owners(tx, &self.cwd).await? != self.owners {
            return Err(CalmError::WorkspaceBusy(self.cwd.display().to_string()));
        }
        Ok(())
    }
}

/// The driver retains another Arc through commit; consuming PreparedTx cannot
/// release the lifecycle guards before the repository writer commits.
pub(crate) struct FencedPreparation {
    pub fence: Arc<LocationFence>,
    pub prepared: Option<Box<dyn PreparedTx>>,
    pub adapter: Arc<dyn ProviderAdapter>,
}
#[async_trait::async_trait]
impl PreparedTx for FencedPreparation {
    async fn apply(self: Box<Self>, tx: &mut Tx<'_>, op: &Operation) -> Result<PreparedTxResult> {
        self.fence.recheck(tx).await?;
        match self.prepared {
            Some(prepared) => prepared.apply(tx, op).await,
            None => self
                .adapter
                .prepare_tx(tx, &op.payload, op)
                .await
                .map(PreparedTxResult::Ready),
        }
    }
}
