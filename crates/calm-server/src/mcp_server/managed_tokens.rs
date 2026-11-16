//! Process-lifetime plaintext for managed providers. DB state remains the authority.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use calm_truth::db::{RepoEventWrite, write_in_tx_typed};
use calm_types::worker::WorkerSessionId;
use sqlx::{Sqlite, Transaction};

use super::auth::hash_token;
use super::wiring::{mint_card_mcp_token_pair, persist_card_mcp_token_hash};
use crate::error::{CalmError, Result};
use crate::model::CardId;

#[derive(Clone)]
struct Candidate {
    card: CardId,
    raw: String,
}

/// Explicitly shared by the ACP and Claude hosts of one AppState/DB instance.
/// Dropping a harness does not revoke or discard a still-valid credential.
#[derive(Clone, Default)]
pub struct ManagedSessionTokens(Arc<Mutex<HashMap<WorkerSessionId, Candidate>>>);

impl std::fmt::Debug for ManagedSessionTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedSessionTokens")
            .field("entries", &self.0.lock().expect("managed tokens").len())
            .finish()
    }
}

/// One validator for issuance, explicit test rotation and reclamation. Missing/inactive or
/// non-current rows are invalid; storage errors propagate and never imply invalidity.
pub(super) async fn carrier_hash(
    tx: &mut Transaction<'_, Sqlite>,
    card: &CardId,
    worker: &WorkerSessionId,
) -> Result<Option<Option<String>>> {
    let Some(row) = crate::db::sqlite::session_get_tx(tx, worker).await? else {
        return Ok(None);
    };
    if !row.state.is_active_authority() || row.card_id.as_ref() != Some(card) {
        return Ok(None);
    }
    let current: Option<String> = sqlx::query_scalar("SELECT session_id FROM cards WHERE id=?1")
        .bind(card.as_str())
        .fetch_optional(&mut **tx)
        .await?
        .flatten();
    if current.as_deref() != Some(worker.as_str()) {
        return Ok(None);
    }
    Ok(Some(row.mcp_token_hash))
}

impl ManagedSessionTokens {
    /// Own the dedicated BEGIN IMMEDIATE transaction. Callers cannot revoke inside an outer
    /// transaction and then roll back a replacement of an otherwise valid cached candidate.
    pub(crate) async fn ensure(
        &self,
        repo: &dyn RepoEventWrite,
        card: &CardId,
        worker: &WorkerSessionId,
    ) -> Result<String> {
        let (store, card, worker) = (self.clone(), card.clone(), worker.clone());
        write_in_tx_typed(repo, move |tx| {
            Box::pin(async move {
                let stored = carrier_hash(tx, &card, &worker).await?.ok_or_else(|| {
                    CalmError::Conflict("Managed credential carrier changed".into())
                })?;
                let cached = store
                    .0
                    .lock()
                    .expect("managed tokens")
                    .get(&worker)
                    .cloned();
                if let Some(cached) = cached
                    && cached.card == card
                    && stored.as_deref() == Some(hash_token(&cached.raw).as_str())
                {
                    return Ok(cached.raw);
                }
                store.prune(tx).await?;
                let (raw, hash) = mint_card_mcp_token_pair();
                persist_card_mcp_token_hash(tx, card.as_str(), &hash).await?;
                crate::db::sqlite::session_mcp_token_set_if_active_tx(tx, worker.as_str(), &hash)
                    .await?;
                // A speculative candidate, not an authorization decision: only the successful
                // commit returns raw to a provider. Keeping it here also recovers a committed token
                // if the caller is cancelled before receiving the result. A rollback's candidate
                // cannot match the next transaction's committed DB hash and will be replaced.
                store.0.lock().expect("managed tokens").insert(
                    worker,
                    Candidate {
                        card,
                        raw: raw.clone(),
                    },
                );
                Ok(raw)
            })
        })
        .await
    }

    async fn prune(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
        let snapshot = self.0.lock().expect("managed tokens").clone();
        let mut invalid = Vec::new();
        for (worker, candidate) in snapshot {
            let valid = carrier_hash(tx, &candidate.card, &worker)
                .await?
                .flatten()
                .is_some_and(|stored| stored == hash_token(&candidate.raw));
            if !valid {
                invalid.push((worker, candidate));
            }
        }
        // Finish all reads before deleting anything: an error must not discard valid entries.
        let mut entries = self.0.lock().expect("managed tokens");
        for (worker, candidate) in invalid {
            if entries
                .get(&worker)
                .is_some_and(|now| now.card == candidate.card && now.raw == candidate.raw)
            {
                entries.remove(&worker);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
