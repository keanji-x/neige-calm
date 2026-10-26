//! Immutable host credential scopes. Validation and additions commit atomically.
use super::{SqliteDb, begin_immediate_tx};

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct BackendIdentity {
    pub provider: String,
    pub backend_id: String,
    pub protocol: String,
    pub configured_home: Vec<u8>,
    pub canonical_home: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("backend registration removed: {0}/{1}")]
    Removed(String, String),
    #[error("backend credential scope changed: {0}/{1}")]
    Changed(String, String),
    #[error("backend credential scope reused")]
    Reused,
    #[error("default backend differs from the pre-upgrade credential scope")]
    LegacyDefault,
    #[error(transparent)]
    Storage(#[from] crate::error::TruthError),
}

impl From<sqlx::Error> for RegistryError {
    fn from(error: sqlx::Error) -> Self {
        Self::Storage(error.into())
    }
}

/// `legacy_home` is resolved by the filesystem owner from the migrated runtime
/// record (or the original implicit default when that record had no home).
pub async fn reconcile(
    db: &SqliteDb,
    configured: &[BackendIdentity],
    legacy_home: &[u8],
) -> Result<(), RegistryError> {
    let mut tx = begin_immediate_tx(db).await?;
    let stored = sqlx::query_as::<_, BackendIdentity>("SELECT * FROM agent_backend_registry")
        .fetch_all(&mut **tx)
        .await?;
    for old in &stored {
        let Some(new) = configured
            .iter()
            .find(|new| new.provider == old.provider && new.backend_id == old.backend_id)
        else {
            return Err(RegistryError::Removed(
                old.provider.clone(),
                old.backend_id.clone(),
            ));
        };
        if old != new {
            return Err(RegistryError::Changed(
                old.provider.clone(),
                old.backend_id.clone(),
            ));
        }
    }
    if !stored
        .iter()
        .any(|old| old.provider == "codex" && old.backend_id == "default")
        && !configured.iter().any(|new| {
            new.provider == "codex"
                && new.backend_id == "default"
                && new.canonical_home == legacy_home
        })
    {
        return Err(RegistryError::LegacyDefault);
    }
    for new in configured {
        if stored
            .iter()
            .any(|old| old.provider == new.provider && old.backend_id == new.backend_id)
        {
            continue;
        }
        if stored.iter().chain(configured.iter()).any(|other| {
            (other.provider != new.provider || other.backend_id != new.backend_id)
                && other.canonical_home == new.canonical_home
        }) {
            return Err(RegistryError::Reused);
        }
        sqlx::query("INSERT INTO agent_backend_registry (provider, backend_id, protocol, configured_home, canonical_home) VALUES (?, ?, ?, ?, ?)")
            .bind(&new.provider).bind(&new.backend_id).bind(&new.protocol)
            .bind(&new.configured_home).bind(&new.canonical_home)
            .execute(&mut **tx).await?;
        sqlx::query("INSERT OR IGNORE INTO codex_backend_daemons (backend_id, state, updated_at, paths_are_legacy_display) VALUES (?, 'idle', 0, ?)")
            .bind(&new.backend_id).bind(false).execute(&mut **tx).await?;
    }
    tx.commit().await?;
    Ok(())
}
