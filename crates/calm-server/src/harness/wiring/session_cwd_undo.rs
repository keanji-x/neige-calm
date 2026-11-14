//! A replacement's undo journal owns the predecessor's native identity and locator together.
use super::*;
use crate::error::{CalmError, Result};
use calm_types::worker::WorkerSession;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize)]
pub(crate) struct SessionCwdUndo {
    binding: BoundSessionCwd,
    replacement: String,
    #[serde(deserialize_with = "Option::deserialize")]
    thread_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    agent_session_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    last_thread_id: Option<Value>,
}

impl SessionCwdUndo {
    pub(crate) async fn capture_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        card: &str,
        predecessor: &str,
        replacement: &str,
    ) -> Result<Self> {
        let session = crate::db::sqlite::session_get_tx(tx, &predecessor.into())
            .await?
            .ok_or_else(changed)?;
        let track = crate::db::sqlite::track_get_tx(tx, &session.track_id).await?;
        let cwd = session_cwd_tx(tx, card, &session, &track).await?;
        Ok(Self::from_session(&session, replacement, cwd))
    }

    fn from_session(session: &WorkerSession, replacement: &str, cwd: String) -> Self {
        Self {
            binding: BoundSessionCwd {
                worker_session_id: session.id.to_string(),
                cwd,
            },
            replacement: replacement.into(),
            thread_id: session.thread_id.clone(),
            agent_session_id: session.agent_session_id.clone(),
            last_thread_id: last_thread(session),
        }
    }

    /// Historical compensation v1 did not require a cwd journal. Its predecessor row is
    /// still the native identity authority, but only an exact retained binding proves cwd.
    /// `result.payload` is usable before checkpoint; checkpoint overwrites it with the
    /// successor. Neither successor launch cwd nor a changed Track locates an old native.
    pub(crate) async fn recover_legacy_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        predecessor: &str,
        replacement: &str,
        saved_payload: &Value,
    ) -> Result<Self> {
        let session = crate::db::sqlite::session_get_tx(tx, &predecessor.into())
            .await?
            .ok_or_else(changed)?;
        let card = session.card_id.as_ref().ok_or_else(changed)?;
        let track = crate::db::sqlite::track_get_tx(tx, &session.track_id).await?;
        let live = crate::db::sqlite::card_execution_shape_tx(tx, card.as_str()).await?;
        let mut retained_cwd = None;
        for payload in [&live.payload, saved_payload] {
            if let Some(binding) =
                payload.get(calm_truth::validation::BOUND_SESSION_CWD_PAYLOAD_KEY)
            {
                let binding: BoundSessionCwd = serde_json::from_value(binding.clone())?;
                if binding.worker_session_id == predecessor {
                    if let Some(thread) = payload.get("codex_thread_id").and_then(Value::as_str)
                        && session.thread_id.as_deref() != Some(thread)
                    {
                        return Err(changed());
                    }
                    let cwd = selected_session_cwd(payload, card.as_str(), &session, &track)?;
                    if retained_cwd
                        .as_ref()
                        .is_some_and(|previous| previous != &cwd)
                    {
                        return Err(changed());
                    }
                    retained_cwd = Some(cwd);
                }
            }
        }
        if let Some(cwd) = retained_cwd {
            tracing::info!(
                predecessor,
                replacement,
                "restoring historical session cwd from retained binding facts"
            );
            return Ok(Self::from_session(&session, replacement, cwd));
        }
        if !has_native_identity(&session) {
            // No native was ever bound: use the existing new-runtime selection contract.
            tracing::info!(
                predecessor,
                replacement,
                "restoring historical unbound session cwd"
            );
            let cwd = selected_session_cwd(&live.payload, card.as_str(), &session, &track)?;
            return Ok(Self::from_session(&session, replacement, cwd));
        }
        Err(CalmError::Conflict(format!(
            "historical session cwd recovery requires verified native identity and absolute cwd \
             for predecessor {predecessor}; supply its session_cwd undo journal and replay compensation"
        )))
    }

    /// Check before restoring authority. A later carrier or foreign locator cannot be undone.
    pub(crate) async fn require_owner_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        predecessor: &str,
    ) -> Result<String> {
        if self.binding.worker_session_id != predecessor || self.replacement == predecessor {
            return Err(changed());
        }
        let session = crate::db::sqlite::session_get_tx(tx, &predecessor.into())
            .await?
            .ok_or_else(changed)?;
        if session.thread_id != self.thread_id
            || session.agent_session_id != self.agent_session_id
            || last_thread(&session) != self.last_thread_id
        {
            return Err(changed());
        }
        let card = session.card_id.as_ref().ok_or_else(changed)?;
        let eligible: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM cards c JOIN worker_sessions old ON old.id=?1 \
             JOIN worker_sessions next ON next.id=?2 WHERE c.id=?3 \
             AND old.card_id=c.id AND next.card_id=c.id AND old.track_id=c.track_id \
             AND next.track_id=c.track_id AND next.provider=old.provider \
             AND next.contract=old.contract AND next.mode=old.mode \
             AND c.session_id IN (old.id,next.id) AND next.state='failed')",
        )
        .bind(predecessor)
        .bind(&self.replacement)
        .bind(card.as_str())
        .fetch_one(&mut **tx)
        .await?;
        if !eligible {
            return Err(changed());
        }
        let shape = crate::db::sqlite::card_execution_shape_tx(tx, card.as_str()).await?;
        if let Some(saved) = shape
            .payload
            .get(calm_truth::validation::BOUND_SESSION_CWD_PAYLOAD_KEY)
        {
            let saved: BoundSessionCwd = serde_json::from_value(saved.clone())?;
            if saved.worker_session_id != predecessor && saved.worker_session_id != self.replacement
            {
                return Err(changed());
            }
            if saved.worker_session_id == predecessor && saved.cwd != self.binding.cwd {
                return Err(changed());
            }
        }
        Ok(card.to_string())
    }

    /// Called in the same transaction that restores the exact predecessor runtime.
    pub(crate) async fn restore_tx(
        self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        card: &str,
    ) -> Result<()> {
        record_bound_session_cwd_tx(tx, card, &self.binding.worker_session_id, &self.binding.cwd)
            .await
    }
}

fn last_thread(session: &WorkerSession) -> Option<Value> {
    session
        .handle_state_json
        .as_ref()?
        .get("last_thread_id")
        // The selector treats both absence and explicit JSON null as no native identity.
        .filter(|thread| !thread.is_null())
        .cloned()
}

fn changed() -> CalmError {
    CalmError::Conflict("session cwd compensation owner changed".into())
}
