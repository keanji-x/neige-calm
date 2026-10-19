use async_trait::async_trait;
use sqlx::{Sqlite, Transaction};

use crate::card_role_cache::CardRoleCache;
use crate::error::Result;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, AreaId, CardId, TrackId};
use crate::model::CardRole;
use crate::role_gate::{RoleViolation, enforce_role};
use crate::track_area_cache::TrackAreaCache;
use crate::worker::{Principal, WorkerSession, WorkerSessionId};
#[cfg(any(test, feature = "test-helpers"))]
use std::sync::Arc;

pub type WorkerSessionRow = WorkerSession;

mod sealed {
    pub trait Sealed {}
}

/// Transaction capability accepted by [`DecisionGate`]; hides the concrete SQL
/// transaction type while still running in the caller's write transaction.
#[async_trait]
pub trait WriteTx: sealed::Sealed + Send {
    async fn read_track_root_session_id(
        &mut self,
        track: &TrackId,
    ) -> Result<Option<WorkerSessionId>>;

    async fn read_worker_session(
        &mut self,
        id: &WorkerSessionId,
    ) -> Result<Option<WorkerSessionRow>>;

    async fn read_card_role(&mut self, card: &CardId) -> Result<Option<CardRole>>;

    /// The card's home track: a live `cards` read in the caller's write
    /// transaction, `None` for a missing row (deny, never assume).
    async fn read_card_track(&mut self, card: &CardId) -> Result<Option<TrackId>>;

    /// Persisted binding and current pointer, read inside the event write transaction.
    async fn read_card_binding(
        &mut self,
        card: &CardId,
    ) -> Result<Option<(String, serde_json::Value, Option<String>)>>;

    async fn read_track_area(&mut self, track: &TrackId) -> Result<Option<AreaId>>;
}

/// Resolve session-keyed actors through the live `worker_sessions` row, then
/// reuse [`enforce_role`] for the containment decision. Session→card is a live
/// DB read, so deleted, unknown or never-committed sessions deny by construction.
pub async fn enforce_role_resolving_session<T: WriteTx + ?Sized + Send>(
    tx: &mut T,
    actor: &ActorId,
    event: &Event,
    scope: &EventScope,
    cache: &CardRoleCache,
    track_area_cache: &TrackAreaCache,
) -> std::result::Result<(), RoleViolation> {
    let session_id = match actor {
        ActorId::AiPlannerSession(session)
        | ActorId::AiCodexSession(session)
        | ActorId::AiClaudeSession(session)
        | ActorId::AiManagedSession(session) => session.clone(),
        _ => return enforce_role(actor, event, scope, cache, track_area_cache),
    };

    if session_id.as_str().is_empty() {
        return Err(RoleViolation::SessionRowMissing {
            session: session_id,
        });
    }

    let session = tx
        .read_worker_session(&session_id)
        .await
        .map_err(|_| RoleViolation::SessionResolutionError {
            session: session_id.clone(),
        })?
        .ok_or_else(|| RoleViolation::SessionRowMissing {
            session: session_id.clone(),
        })?;

    if !session.state.is_active_authority() {
        return Err(RoleViolation::SessionNotActive {
            session: session_id,
        });
    }

    // The new managed contract has one live authorization entry point. Historical
    // executor actors keep their semantics, but cannot impersonate a managed Assistant.
    if session.contract == calm_types::worker::WorkerContract::Assistant
        && !matches!(actor, ActorId::AiManagedSession(_))
    {
        return Err(RoleViolation::SessionActorUnresolved {
            session: session_id,
        });
    }

    let card_id = session
        .card_id
        .ok_or_else(|| RoleViolation::CardlessSessionDenied {
            session: session_id.clone(),
        })?;

    let synthetic = match actor {
        ActorId::AiPlannerSession(_) => {
            // A cached Planner role is not a grant: revalidate the durable current
            // binding in the same write transaction as the eventual event/mutation.
            let role = tx.read_card_role(&card_id).await.map_err(|_| {
                RoleViolation::SessionResolutionError {
                    session: session_id.clone(),
                }
            })?;
            let binding = tx.read_card_binding(&card_id).await.map_err(|_| {
                RoleViolation::SessionResolutionError {
                    session: session_id.clone(),
                }
            })?;
            let track = tx.read_card_track(&card_id).await.map_err(|_| {
                RoleViolation::SessionResolutionError {
                    session: session_id.clone(),
                }
            })?;
            if role != Some(CardRole::Planner)
                || !binding
                    .is_some_and(|(_, _, current)| current.as_deref() == Some(session_id.as_str()))
                || track.as_ref() != Some(&session.track_id)
                || cache.get(&card_id) != Some(CardRole::Planner)
            {
                return Err(RoleViolation::SessionPlannerRoleMismatch {
                    session: session_id,
                    card: card_id,
                });
            }
            ActorId::AiPlanner(card_id)
        }
        ActorId::AiCodexSession(_) => ActorId::AiCodex(card_id),
        ActorId::AiClaudeSession(_) => ActorId::AiClaude(card_id),
        ActorId::AiManagedSession(_) => {
            use calm_types::worker::{SessionMode, WorkerContract, WorkerProviderKind};
            let live = tx.read_card_binding(&card_id).await.map_err(|_| {
                RoleViolation::SessionResolutionError {
                    session: session_id.clone(),
                }
            })?;
            let role = tx.read_card_role(&card_id).await.map_err(|_| {
                RoleViolation::SessionResolutionError {
                    session: session_id.clone(),
                }
            })?;
            let track = tx.read_card_track(&card_id).await.map_err(|_| {
                RoleViolation::SessionResolutionError {
                    session: session_id.clone(),
                }
            })?;
            if session.provider == WorkerProviderKind::Terminal
                || session.contract != WorkerContract::Assistant
                || session.mode != SessionMode::Resumable
                || role != Some(CardRole::Assistant)
                || track.as_ref() != Some(&session.track_id)
                || !live.is_some_and(|(kind, payload, current)| {
                    kind == "codex"
                        && current.as_deref() == Some(session_id.as_str())
                        && payload
                            .get("harness_profile")
                            .and_then(serde_json::Value::as_str)
                            == Some("assistant")
                        && payload
                            .get(crate::validation::ASSISTANT_PROVIDER_PAYLOAD_KEY)
                            .and_then(|value| {
                                serde_json::from_value::<calm_types::runtime::AgentProvider>(
                                    value.clone(),
                                )
                                .ok()
                            })
                            .map(|provider| WorkerProviderKind::from(&provider))
                            == Some(session.provider)
                })
            {
                return Err(RoleViolation::SessionPlannerRoleMismatch {
                    session: session_id,
                    card: card_id,
                });
            }
            ActorId::AiManagedCard(card_id)
        }
        _ => unreachable!("session actor match above guarantees session variant"),
    };

    enforce_role(&synthetic, event, scope, cache, track_area_cache)
}

/// Same policy as [`enforce_role_resolving_session`], with `card → {role, home
/// track}` and `track → area` read live from the caller's write transaction.
/// Hydrates two throwaway caches and calls the original, reading only keys
/// reachable from the actor and scope inside the write lock.
pub async fn enforce_role_resolving_session_from_tx<T: WriteTx + ?Sized + Send>(
    tx: &mut T,
    actor: &ActorId,
    event: &Event,
    scope: &EventScope,
) -> std::result::Result<(), RoleViolation> {
    // A session actor's card is outside the syntactic closure of `(actor, scope)`,
    // so look it up as an extra key. Errors are not interpreted here: the
    // authoritative session read below owns every deny reason.
    let session_card = match actor {
        ActorId::AiPlannerSession(session)
        | ActorId::AiCodexSession(session)
        | ActorId::AiClaudeSession(session)
        | ActorId::AiManagedSession(session)
            if !session.as_str().is_empty() =>
        {
            tx.read_worker_session(session)
                .await
                .ok()
                .flatten()
                .and_then(|row| row.card_id)
        }
        _ => None,
    };
    let (cache, track_area_cache) =
        hydrate_role_caches_from_tx(tx, actor, scope, session_card.as_ref()).await?;
    enforce_role_resolving_session(tx, actor, event, scope, &cache, &track_area_cache).await
}

/// Session variants carry a `WorkerSessionId`, not a card; their card is
/// resolved from `worker_sessions` and passed as `extra_card`.
fn actor_card_id(actor: &ActorId) -> Option<&CardId> {
    match actor {
        ActorId::AiCodex(card)
        | ActorId::AiClaude(card)
        | ActorId::AiManagedCard(card)
        | ActorId::AiPlanner(card) => Some(card),
        ActorId::User
        | ActorId::Kernel
        | ActorId::KernelDispatcher
        | ActorId::Plugin(_)
        | ActorId::AiPlannerSession(_)
        | ActorId::AiCodexSession(_)
        | ActorId::AiClaudeSession(_)
        | ActorId::AiManagedSession(_) => None,
    }
}

/// Fill throwaway caches from the caller's transaction with every row
/// [`enforce_role`] can key on: the gate only keys on identifiers reachable from
/// `actor ∪ scope ∪ {session card}` under `card → home track → area`. Safe
/// because the verdict and the `events` insert share one transaction; the
/// write-through caches can drift from the DB in either direction after a rollback.
async fn hydrate_role_caches_from_tx<T: WriteTx + ?Sized + Send>(
    tx: &mut T,
    actor: &ActorId,
    scope: &EventScope,
    extra_card: Option<&CardId>,
) -> std::result::Result<(CardRoleCache, TrackAreaCache), RoleViolation> {
    let cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();

    let mut cards: Vec<CardId> = Vec::new();
    for card in actor_card_id(actor)
        .into_iter()
        .chain(scope.card_id())
        .chain(extra_card)
    {
        if card.as_str().is_empty() || cards.contains(card) {
            continue;
        }
        cards.push(card.clone());
    }

    let mut tracks: Vec<TrackId> = scope.track_id().cloned().into_iter().collect();
    for card in &cards {
        let role = tx
            .read_card_role(card)
            .await
            .map_err(|_| RoleViolation::RoleLookupFailed {
                subject: format!("cards.role({card})"),
            })?;
        let home_track =
            tx.read_card_track(card)
                .await
                .map_err(|_| RoleViolation::RoleLookupFailed {
                    subject: format!("cards.track_id({card})"),
                })?;
        // Both halves or neither: the write-through cache stores role and home
        // track as one entry, so a half-populated read is not a state the
        // cached gate can observe. Fail closed by leaving the card unknown.
        let (Some(role), Some(home_track)) = (role, home_track) else {
            continue;
        };
        if !tracks.contains(&home_track) {
            tracks.push(home_track.clone());
        }
        cache.insert(card.clone(), role, home_track);
    }

    for track in tracks {
        let area =
            tx.read_track_area(&track)
                .await
                .map_err(|_| RoleViolation::RoleLookupFailed {
                    subject: format!("tracks.area_id({track})"),
                })?;
        if let Some(area) = area {
            track_area_cache.insert(track, area);
        }
    }

    Ok((cache, track_area_cache))
}

impl<'a> sealed::Sealed for Transaction<'a, Sqlite> {}

#[async_trait]
impl<'a> WriteTx for Transaction<'a, Sqlite> {
    async fn read_track_root_session_id(
        &mut self,
        track: &TrackId,
    ) -> Result<Option<WorkerSessionId>> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT root_session_id FROM tracks WHERE id = ?1")
                .bind(track.as_str())
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

    async fn read_card_track(&mut self, card: &CardId) -> Result<Option<TrackId>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT track_id FROM cards WHERE id = ?1")
            .bind(card.as_str())
            .fetch_optional(&mut **self)
            .await?;
        Ok(row.map(|(track,)| TrackId::from(track)))
    }

    async fn read_card_binding(
        &mut self,
        card: &CardId,
    ) -> Result<Option<(String, serde_json::Value, Option<String>)>> {
        let row: Option<(String, String, Option<String>)> =
            sqlx::query_as("SELECT kind, payload, session_id FROM cards WHERE id=?1")
                .bind(card.as_str())
                .fetch_optional(&mut **self)
                .await?;
        row.map(|(kind, payload, session)| Ok((kind, serde_json::from_str(&payload)?, session)))
            .transpose()
    }

    async fn read_track_area(&mut self, track: &TrackId) -> Result<Option<AreaId>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT area_id FROM tracks WHERE id = ?1")
            .bind(track.as_str())
            .fetch_optional(&mut **self)
            .await?;
        Ok(row.map(|(id,)| AreaId::from(id)))
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

/// Pluggable "should this write be allowed" policy. **Test-only**: kept behind
/// `cfg(test-helpers)` so a permissive stub cannot reach a production write path.
#[cfg(any(test, feature = "test-helpers"))]
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

/// Allow-everything [`DecisionGate`]. Test scaffolding only.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Debug, Default, Clone, Copy)]
pub struct PermissiveGate;

#[cfg(any(test, feature = "test-helpers"))]
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

#[derive(Debug, Clone)]
pub struct PrincipalDecisionGate {
    principal: Principal,
}

impl PrincipalDecisionGate {
    pub fn new(principal: Principal) -> Self {
        Self { principal }
    }

    /// May this agent session record into `track`'s report? The criterion is the
    /// session's card — `card.track_id == track` ∧ `role ∈ {Planner, Assistant}` —
    /// plus an explicit liveness check: `session_get_tx` has no state filter, so a
    /// `superseded` row still resolves with its `card_id`. Every unresolvable step denies.
    pub async fn decide_recorder<T>(&self, tx: &mut T, track: &TrackId) -> Result<GateDecision>
    where
        T: WriteTx + ?Sized + Send,
    {
        self.decide_card_authority(tx, track, None, &[CardRole::Planner, CardRole::Assistant])
            .await
    }

    /// Re-prove a transported card identity for a write in the same transaction.
    pub async fn decide_card_write<T>(
        &self,
        tx: &mut T,
        track: &TrackId,
        card: &CardId,
        role: CardRole,
    ) -> Result<GateDecision>
    where
        T: WriteTx + ?Sized + Send,
    {
        self.decide_card_authority(tx, track, Some(card), &[role])
            .await
    }

    async fn decide_card_authority<T>(
        &self,
        tx: &mut T,
        track: &TrackId,
        expected_card: Option<&CardId>,
        roles: &[CardRole],
    ) -> Result<GateDecision>
    where
        T: WriteTx + ?Sized + Send,
    {
        let Principal::Agent { session_id, .. } = &self.principal else {
            return Ok(GateDecision::Deny(
                "principal is not an agent session".into(),
            ));
        };
        let Some(session) = tx.read_worker_session(session_id).await? else {
            return Ok(GateDecision::Deny(format!(
                "session {session_id} has no session row"
            )));
        };
        if !session.state.is_active_authority() {
            let state = session.state;
            return Ok(GateDecision::Deny(format!(
                "session {session_id} is no longer an active authority (state {state:?})"
            )));
        }
        let Some(card_id) = session.card_id else {
            return Ok(GateDecision::Deny(format!(
                "session {session_id} is not bound to a card"
            )));
        };
        if expected_card.is_some_and(|expected| expected != &card_id) {
            return Ok(GateDecision::Deny(format!(
                "session {session_id} card binding changed"
            )));
        }
        let Some(role) = tx.read_card_role(&card_id).await? else {
            return Ok(GateDecision::Deny(format!(
                "session {session_id} is bound to unknown card {card_id}"
            )));
        };
        if !roles.contains(&role) {
            return Ok(GateDecision::Deny(format!(
                "session {session_id} card {card_id} has role {role:?}, which may not record"
            )));
        }
        let Some(card_track) = tx.read_card_track(&card_id).await? else {
            return Ok(GateDecision::Deny(format!(
                "session {session_id} card {card_id} has no resolvable track"
            )));
        };
        if &card_track != track {
            return Ok(GateDecision::Deny(format!(
                "session {session_id} card {card_id} lives on track {card_track}, not {track}"
            )));
        }
        Ok(GateDecision::Allow)
    }

    pub async fn recorder_grant<T>(&self, tx: &mut T, track: &TrackId) -> Result<bool>
    where
        T: WriteTx + ?Sized + Send,
    {
        Ok(matches!(
            self.decide_recorder(tx, track).await?,
            GateDecision::Allow
        ))
    }
}

/// Run `f` inside one write transaction behind a [`DecisionGate`]. Test-only.
#[cfg(any(test, feature = "test-helpers"))]
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
    use tokio::sync::Mutex;

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

#[cfg(test)]
mod tests;
