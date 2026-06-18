use async_trait::async_trait;
use sqlx::{Sqlite, Transaction};
use tokio::sync::Mutex;

use crate::card_role_cache::CardRoleCache;
use crate::error::Result;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::model::CardRole;
use crate::role_gate::{RoleViolation, enforce_role};
use crate::wave_cove_cache::WaveCoveCache;
use crate::worker::{Principal, WorkerSession, WorkerSessionId};
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

/// Resolve session-keyed actors through the live `worker_sessions` row, then
/// reuse the sync role gate for the final containment decision.
///
/// This is HP1-a-2's option (b) seam: session→card is a live DB read at gate
/// time, not the option-(a) session→card cache, so deleted, unknown, or
/// never-committed sessions deny by construction. Once a session resolves to a
/// bound card, card→{role,wave,cove} still comes from the existing
/// `CardRoleCache` and `WaveCoveCache` through [`enforce_role`]. That keeps a
/// session actor's decision identical to the equivalent card-keyed actor's
/// decision, with no duplicate containment logic. All ambiguous states deny
/// closed, and cardless authority remains denied until PR11 lands.
pub async fn enforce_role_resolving_session<T: WriteTx + ?Sized + Send>(
    tx: &mut T,
    actor: &ActorId,
    event: &Event,
    scope: &EventScope,
    cache: &CardRoleCache,
    wave_cove_cache: &WaveCoveCache,
) -> std::result::Result<(), RoleViolation> {
    let session_id = match actor {
        ActorId::AiSpecSession(session)
        | ActorId::AiCodexSession(session)
        | ActorId::AiClaudeSession(session) => session.clone(),
        _ => return enforce_role(actor, event, scope, cache, wave_cove_cache),
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

    let card_id = session
        .card_id
        .ok_or_else(|| RoleViolation::CardlessSessionDenied {
            session: session_id.clone(),
        })?;

    let synthetic = match actor {
        ActorId::AiSpecSession(_) => {
            // Live read gave ground-truth card_id; the AiSpec path in enforce_role
            // does not re-check role/scope for ordinary events, so verify the card
            // is actually Spec-roled before granting spec authority. Fail-closed on
            // non-Spec or unknown card. Worker variants below stay delegated;
            // enforce_role's self-scope/UnknownCard arms already cover every role.
            if cache.get(&card_id) != Some(CardRole::Spec) {
                return Err(RoleViolation::SessionSpecRoleMismatch {
                    session: session_id,
                    card: card_id,
                });
            }
            ActorId::AiSpec(card_id)
        }
        ActorId::AiCodexSession(_) => ActorId::AiCodex(card_id),
        ActorId::AiClaudeSession(_) => ActorId::AiClaude(card_id),
        _ => unreachable!("session actor match above guarantees session variant"),
    };

    enforce_role(&synthetic, event, scope, cache, wave_cove_cache)
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

#[derive(Debug, Clone)]
pub struct PrincipalDecisionGate {
    principal: Principal,
}

impl PrincipalDecisionGate {
    pub fn new(principal: Principal) -> Self {
        Self { principal }
    }

    pub async fn decide_recorder<T>(&self, tx: &mut T, wave: &WaveId) -> Result<GateDecision>
    where
        T: WriteTx + ?Sized + Send,
    {
        let Principal::Agent { session_id, .. } = &self.principal else {
            return Ok(GateDecision::Deny(
                "principal is not an agent session".into(),
            ));
        };
        let root = tx.read_wave_root_session_id(wave).await?;
        if root.as_ref() == Some(session_id) {
            Ok(GateDecision::Allow)
        } else {
            Ok(GateDecision::Deny(format!(
                "session {session_id} is not wave root"
            )))
        }
    }

    pub async fn recorder_grant<T>(&self, tx: &mut T, wave: &WaveId) -> Result<bool>
    where
        T: WriteTx + ?Sized + Send,
    {
        Ok(matches!(
            self.decide_recorder(tx, wave).await?,
            GateDecision::Allow
        ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cove, CoveKind, Wave, WaveLifecycle};
    use crate::worker::{
        LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSessionState,
    };

    struct FakeWriteTx {
        root_session_id: Option<WorkerSessionId>,
        worker_session: Option<WorkerSessionRow>,
        worker_session_reads: usize,
        worker_session_read_error: bool,
    }

    impl FakeWriteTx {
        fn new() -> Self {
            Self {
                root_session_id: None,
                worker_session: None,
                worker_session_reads: 0,
                worker_session_read_error: false,
            }
        }

        fn with_worker_session(worker_session: WorkerSessionRow) -> Self {
            Self {
                worker_session: Some(worker_session),
                ..Self::new()
            }
        }

        fn with_worker_session_read_error() -> Self {
            Self {
                worker_session_read_error: true,
                ..Self::new()
            }
        }
    }

    impl sealed::Sealed for FakeWriteTx {}

    #[async_trait]
    impl WriteTx for FakeWriteTx {
        async fn read_wave_root_session_id(
            &mut self,
            _wave: &WaveId,
        ) -> Result<Option<WorkerSessionId>> {
            Ok(self.root_session_id.clone())
        }

        async fn read_worker_session(
            &mut self,
            id: &WorkerSessionId,
        ) -> Result<Option<WorkerSessionRow>> {
            self.worker_session_reads += 1;
            if self.worker_session_read_error {
                return Err(crate::error::TruthError::Internal(
                    "worker session read failed".into(),
                ));
            }
            Ok(self
                .worker_session
                .as_ref()
                .filter(|session| &session.id == id)
                .cloned())
        }

        async fn read_card_role(&mut self, _card: &CardId) -> Result<Option<CardRole>> {
            Ok(None)
        }

        async fn read_wave_cove(&mut self, _wave: &WaveId) -> Result<Option<CoveId>> {
            Ok(None)
        }
    }

    fn agent(session_id: &str) -> Principal {
        Principal::Agent {
            session_id: WorkerSessionId::from(session_id),
            wave_id: WaveId::from("wave-1"),
            cove_id: CoveId::from("cove-1"),
        }
    }

    fn worker_session(session_id: &str, card_id: Option<CardId>) -> WorkerSession {
        WorkerSession {
            id: WorkerSessionId::from(session_id),
            wave_id: WaveId::from("w"),
            provider: WorkerProviderKind::Codex,
            mode: SessionMode::Resumable,
            contract: WorkerContract::Executor,
            parent_session_id: None,
            requester_session_id: None,
            state: WorkerSessionState::Running,
            mcp_token_hash: None,
            thread_id: None,
            agent_session_id: None,
            active_turn_id: None,
            terminal_run_id: None,
            card_id,
            handle_state_json: None,
            liveness: LivenessTag::Unknown,
            liveness_probed_at_ms: None,
            exit_code: None,
            exit_interpretation: None,
            spawn_op_id: None,
            last_activity_ms: None,
            last_thread_status: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            completed_at_ms: None,
        }
    }

    fn wave(id: &str, cove: &str) -> Wave {
        Wave {
            id: WaveId::from(id),
            cove_id: CoveId::from(cove),
            title: "t".into(),
            sort: 1.0,
            archived_at: None,
            pinned_at: None,
            lifecycle: WaveLifecycle::Draft,
            cwd: String::new(),
            terminal_at: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn card_scope(card: &str, wave: &str, cove: &str) -> EventScope {
        EventScope::Card {
            card: CardId::from(card),
            wave: WaveId::from(wave),
            cove: CoveId::from(cove),
        }
    }

    fn wave_scope(wave: &str, cove: &str) -> EventScope {
        EventScope::Wave {
            wave: WaveId::from(wave),
            cove: CoveId::from(cove),
        }
    }

    fn wave_updated() -> Event {
        Event::WaveUpdated(crate::event::WaveUpdatedPayload::new(wave("w", "c"), None))
    }

    fn cove_updated() -> Event {
        Event::CoveUpdated(Cove {
            id: CoveId::from("c"),
            name: "n".into(),
            color: "#fff".into(),
            sort: 1.0,
            kind: CoveKind::User,
            created_at: 0,
            updated_at: 0,
        })
    }

    fn seeded_caches(card: &CardId, role: CardRole) -> (CardRoleCache, WaveCoveCache) {
        let cache = CardRoleCache::new();
        cache.insert(card.clone(), role, WaveId::from("w"));
        let wcc = WaveCoveCache::new();
        wcc.insert(WaveId::from("w"), CoveId::from("c"));
        (cache, wcc)
    }

    #[tokio::test]
    async fn principal_decision_gate_computes_root_grant() {
        let wave = WaveId::from("wave-1");
        let mut tx = FakeWriteTx {
            root_session_id: Some(WorkerSessionId::from("root-session")),
            ..FakeWriteTx::new()
        };

        let root = PrincipalDecisionGate::new(agent("root-session"))
            .recorder_grant(&mut tx, &wave)
            .await
            .expect("root grant");
        assert!(root);

        let non_root = PrincipalDecisionGate::new(agent("other-session"))
            .recorder_grant(&mut tx, &wave)
            .await
            .expect("non-root grant");
        assert!(!non_root);
    }

    #[tokio::test]
    async fn principal_decision_gate_treats_missing_root_as_not_root() {
        let wave = WaveId::from("wave-1");
        let mut tx = FakeWriteTx::new();

        let grant = PrincipalDecisionGate::new(agent("root-session"))
            .recorder_grant(&mut tx, &wave)
            .await
            .expect("missing root grant");
        assert!(!grant);
    }

    #[tokio::test]
    async fn session_resolver_allows_worker_self_scope() {
        let worker_card = CardId::from("worker-card");
        let (cache, wcc) = seeded_caches(&worker_card, CardRole::Worker);
        let mut tx =
            FakeWriteTx::with_worker_session(worker_session("s1", Some(worker_card.clone())));

        let res = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("s1")),
            &cove_updated(),
            &card_scope(worker_card.as_str(), "w", "c"),
            &cache,
            &wcc,
        )
        .await;

        assert!(res.is_ok(), "worker session in own scope: {res:?}");
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_allows_spec_wave_updated() {
        let spec_card = CardId::from("spec-card");
        let (cache, wcc) = seeded_caches(&spec_card, CardRole::Spec);
        let mut tx =
            FakeWriteTx::with_worker_session(worker_session("s2", Some(spec_card.clone())));

        let res = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiSpecSession(WorkerSessionId::from("s2")),
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
            &wcc,
        )
        .await;

        assert!(res.is_ok(), "spec session should update wave: {res:?}");
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_denies_spec_session_bound_to_worker_card_on_ordinary_event() {
        let worker_card = CardId::from("worker-card");
        let (cache, wcc) = seeded_caches(&worker_card, CardRole::Worker);
        let mut tx = FakeWriteTx::with_worker_session(worker_session(
            "s-spec-worker",
            Some(worker_card.clone()),
        ));

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiSpecSession(WorkerSessionId::from("s-spec-worker")),
            &cove_updated(),
            &card_scope(worker_card.as_str(), "w", "c"),
            &cache,
            &wcc,
        )
        .await
        .expect_err("spec session bound to worker card must deny");

        match err {
            RoleViolation::SessionSpecRoleMismatch { session, card } => {
                assert_eq!(session, WorkerSessionId::from("s-spec-worker"));
                assert_eq!(card, worker_card);
            }
            other => panic!("unexpected violation: {other:?}"),
        }
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_denies_spec_session_bound_to_unknown_card_on_ordinary_event() {
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let ghost = CardId::from("ghost");
        let mut tx =
            FakeWriteTx::with_worker_session(worker_session("s-spec-ghost", Some(ghost.clone())));

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiSpecSession(WorkerSessionId::from("s-spec-ghost")),
            &cove_updated(),
            &card_scope(ghost.as_str(), "w", "c"),
            &cache,
            &wcc,
        )
        .await
        .expect_err("spec session bound to unknown card must deny");

        match err {
            RoleViolation::SessionSpecRoleMismatch { session, card } => {
                assert_eq!(session, WorkerSessionId::from("s-spec-ghost"));
                assert_eq!(card, ghost);
            }
            other => panic!("unexpected violation: {other:?}"),
        }
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_allows_spec_session_bound_to_spec_card_on_ordinary_event() {
        let spec_card = CardId::from("spec-card");
        let (cache, wcc) = seeded_caches(&spec_card, CardRole::Spec);
        let mut tx = FakeWriteTx::with_worker_session(worker_session(
            "s-spec-ordinary",
            Some(spec_card.clone()),
        ));

        let res = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiSpecSession(WorkerSessionId::from("s-spec-ordinary")),
            &cove_updated(),
            &card_scope(spec_card.as_str(), "w", "c"),
            &cache,
            &wcc,
        )
        .await;

        assert!(
            res.is_ok(),
            "spec session bound to spec card should pass ordinary event: {res:?}"
        );
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_denies_empty_card_id_via_worker_variant_sync_gate() {
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let mut tx = FakeWriteTx::with_worker_session(worker_session(
            "s-empty-card",
            Some(CardId::from("")),
        ));

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("s-empty-card")),
            &cove_updated(),
            &EventScope::System,
            &cache,
            &wcc,
        )
        .await
        .expect_err("worker session resolving to empty card id must deny");

        assert!(matches!(err, RoleViolation::EmptyAiCardId));
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_denies_missing_row() {
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let mut tx = FakeWriteTx::new();

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("missing")),
            &cove_updated(),
            &EventScope::System,
            &cache,
            &wcc,
        )
        .await
        .expect_err("missing session row must deny");

        assert!(matches!(err, RoleViolation::SessionRowMissing { .. }));
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_denies_cardless_row() {
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let mut tx = FakeWriteTx::with_worker_session(worker_session("cardless", None));

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("cardless")),
            &cove_updated(),
            &EventScope::System,
            &cache,
            &wcc,
        )
        .await
        .expect_err("cardless session must deny");

        assert!(matches!(err, RoleViolation::CardlessSessionDenied { .. }));
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_denies_read_error() {
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let mut tx = FakeWriteTx::with_worker_session_read_error();

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("s-error")),
            &cove_updated(),
            &EventScope::System,
            &cache,
            &wcc,
        )
        .await
        .expect_err("worker_sessions read error must deny");

        assert!(matches!(err, RoleViolation::SessionResolutionError { .. }));
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_denies_empty_session_id() {
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let mut tx = FakeWriteTx::new();

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("")),
            &cove_updated(),
            &EventScope::System,
            &cache,
            &wcc,
        )
        .await
        .expect_err("empty session id must deny");

        assert!(matches!(err, RoleViolation::SessionRowMissing { .. }));
        assert_eq!(tx.worker_session_reads, 0);
    }

    #[tokio::test]
    async fn session_resolver_denies_unknown_card_via_sync_gate() {
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let ghost = CardId::from("ghost");
        let mut tx =
            FakeWriteTx::with_worker_session(worker_session("s-ghost", Some(ghost.clone())));

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("s-ghost")),
            &cove_updated(),
            &card_scope(ghost.as_str(), "w", "c"),
            &cache,
            &wcc,
        )
        .await
        .expect_err("unknown resolved card must deny");

        assert!(matches!(err, RoleViolation::UnknownCard { .. }));
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_denies_out_of_scope_worker_via_sync_gate() {
        let worker_card = CardId::from("worker-card");
        let (cache, wcc) = seeded_caches(&worker_card, CardRole::Worker);
        let mut tx =
            FakeWriteTx::with_worker_session(worker_session("s-out-of-scope", Some(worker_card)));

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("s-out-of-scope")),
            &cove_updated(),
            &card_scope("other-card", "w", "c"),
            &cache,
            &wcc,
        )
        .await
        .expect_err("worker session outside own card scope must deny");

        assert!(matches!(err, RoleViolation::WorkerOutOfScope { .. }));
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_denies_worker_wave_updated_via_sync_gate() {
        let worker_card = CardId::from("worker-card");
        let (cache, wcc) = seeded_caches(&worker_card, CardRole::Worker);
        let mut tx =
            FakeWriteTx::with_worker_session(worker_session("s-worker-wave", Some(worker_card)));

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("s-worker-wave")),
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
            &wcc,
        )
        .await
        .expect_err("worker session must not update wave");

        assert!(matches!(err, RoleViolation::NotSpecForWave { .. }));
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_passthrough_keeps_card_actor_on_sync_path() {
        let worker_card = CardId::from("worker-card");
        let (cache, wcc) = seeded_caches(&worker_card, CardRole::Worker);
        let actor = ActorId::AiCodex(worker_card.clone());
        let event = cove_updated();
        let scope = card_scope(worker_card.as_str(), "w", "c");
        let direct = enforce_role(&actor, &event, &scope, &cache, &wcc);
        let mut tx = FakeWriteTx::new();

        let routed =
            enforce_role_resolving_session(&mut tx, &actor, &event, &scope, &cache, &wcc).await;

        assert!(direct.is_ok(), "direct sync gate should allow: {direct:?}");
        assert!(routed.is_ok(), "async passthrough should allow: {routed:?}");
        assert_eq!(tx.worker_session_reads, 0);
    }
}
