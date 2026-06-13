use async_trait::async_trait;
use sqlx::{Sqlite, Transaction};
use tokio::sync::Mutex;

use crate::error::Result;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::model::CardRole;
use crate::worker::{WorkerSession, WorkerSessionId};
use std::sync::Arc;

pub type WorkerSessionRow = WorkerSession;

mod sealed {
    pub trait Sealed {}
}

/// Transaction capability accepted by [`DecisionGate`].
///
/// This intentionally hides the concrete SQL transaction type from the gate
/// signature while still letting truth-layer impls run in the caller's write
/// transaction. PR2 only provides the sqlite implementation; later gates can
/// add more truth-owned transaction adapters without changing conformance
/// call sites.
///
/// This is the substrate PR7b's Principal gate will use.
#[async_trait]
pub trait WriteTx: sealed::Sealed + Send {
    async fn read_wave_root_session_id(&mut self, wave: &WaveId)
    -> Result<Option<WorkerSessionId>>;

    async fn read_worker_session(
        &mut self,
        id: &WorkerSessionId,
    ) -> Result<Option<WorkerSessionRow>>;

    async fn read_card_role(&mut self, card: &CardId) -> Result<Option<CardRole>>;

    async fn read_wave_cove(&mut self, wave: &WaveId) -> Result<Option<CoveId>>;
}

impl<'a> sealed::Sealed for Transaction<'a, Sqlite> {}

#[async_trait]
impl<'a> WriteTx for Transaction<'a, Sqlite> {
    async fn read_wave_root_session_id(
        &mut self,
        wave: &WaveId,
    ) -> Result<Option<WorkerSessionId>> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT root_session_id FROM waves WHERE id = ?1")
                .bind(wave.as_str())
                .fetch_optional(&mut **self)
                .await?;
        Ok(row.and_then(|(id,)| id.map(WorkerSessionId::from)))
    }

    async fn read_worker_session(
        &mut self,
        id: &WorkerSessionId,
    ) -> Result<Option<WorkerSessionRow>> {
        crate::db::sqlite::session_get_tx(self, id).await
    }

    async fn read_card_role(&mut self, card: &CardId) -> Result<Option<CardRole>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT role FROM cards WHERE id = ?1")
            .bind(card.as_str())
            .fetch_optional(&mut **self)
            .await?;
        row.map(|(role,)| {
            CardRole::try_from(role)
                .map_err(|e| crate::error::TruthError::Internal(format!("cards.role decode: {e}")))
        })
        .transpose()
    }

    async fn read_wave_cove(&mut self, wave: &WaveId) -> Result<Option<CoveId>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT cove_id FROM waves WHERE id = ?1")
            .bind(wave.as_str())
            .fetch_optional(&mut **self)
            .await?;
        Ok(row.map(|(id,)| CoveId::from(id)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    Allow,
    Deny(String),
}

impl GateDecision {
    pub fn into_result(self) -> Result<()> {
        match self {
            GateDecision::Allow => Ok(()),
            GateDecision::Deny(message) => Err(crate::error::TruthError::Forbidden(message)),
        }
    }
}

#[async_trait]
pub trait DecisionGate: Send + Sync {
    async fn decide<T>(
        &self,
        tx: &mut T,
        actor: &ActorId,
        scope: &EventScope,
        event: &Event,
    ) -> Result<GateDecision>
    where
        T: WriteTx + ?Sized + Send;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PermissiveGate;

#[async_trait]
impl DecisionGate for PermissiveGate {
    async fn decide<T>(
        &self,
        _tx: &mut T,
        _actor: &ActorId,
        _scope: &EventScope,
        _event: &Event,
    ) -> Result<GateDecision>
    where
        T: WriteTx + ?Sized + Send,
    {
        Ok(GateDecision::Allow)
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn commit_decision<R, G, F>(
    repo: &dyn crate::db::RepoEventWrite,
    gate: Arc<G>,
    actor: ActorId,
    scope: EventScope,
    correlation: Option<&str>,
    bus: &crate::event::EventBus,
    write: &crate::state::WriteContext,
    event: Event,
    f: F,
) -> Result<(R, i64)>
where
    R: Send + 'static,
    G: DecisionGate + 'static,
    F: for<'tx> FnOnce(
            &'tx mut Transaction<'_, Sqlite>,
        ) -> futures::future::BoxFuture<'tx, Result<R>>
        + Send
        + 'static,
{
    let captured: Arc<Mutex<Option<R>>> = Arc::new(Mutex::new(None));
    let captured_inner = Arc::clone(&captured);
    let decision_actor = actor.clone();
    let decision_scope = scope.clone();

    let boxed: crate::db::WriteWithEventFn<'_> = Box::new(move |tx| {
        Box::pin(async move {
            gate.decide(tx, &decision_actor, &decision_scope, &event)
                .await?
                .into_result()?;
            let row = f(tx).await?;
            *captured_inner.lock().await = Some(row);
            Ok(event)
        })
    });

    let event_id = repo
        .write_with_event(actor, scope, correlation, bus, write, boxed)
        .await?;
    let row = Arc::try_unwrap(captured)
        .map_err(|_| {
            crate::error::TruthError::Internal(
                "commit_decision: outstanding reference to captured row".into(),
            )
        })?
        .into_inner()
        .ok_or_else(|| {
            crate::error::TruthError::Internal("commit_decision: closure did not set row".into())
        })?;
    Ok((row, event_id))
}
