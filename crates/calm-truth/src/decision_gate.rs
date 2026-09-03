use async_trait::async_trait;
use sqlx::{Sqlite, Transaction};
use tokio::sync::Mutex;

use crate::card_role_cache::CardRoleCache;
use crate::error::Result;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, AreaId, CardId, WaveId};
use crate::model::CardRole;
use crate::role_gate::{RoleViolation, enforce_role};
use crate::wave_area_cache::WaveAreaCache;
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

    /// #1189 §3.6 — the card's home wave, the load-bearing half of the
    /// recorder criterion. Same shape as [`WriteTx::read_card_role`]: a
    /// live `cards` read inside the caller's write transaction, `None`
    /// for a row that isn't there (deny, never assume).
    async fn read_card_wave(&mut self, card: &CardId) -> Result<Option<WaveId>>;

    async fn read_wave_area(&mut self, wave: &WaveId) -> Result<Option<AreaId>>;
}

/// Resolve session-keyed actors through the live `worker_sessions` row, then
/// reuse the sync role gate for the final containment decision.
///
/// This is HP1-a-2's option (b) seam: session→card is a live DB read at gate
/// time, not the option-(a) session→card cache, so deleted, unknown, or
/// never-committed sessions deny by construction. Once a session resolves to a
/// bound card, card→{role,wave,area} still comes from the existing
/// `CardRoleCache` and `WaveAreaCache` through [`enforce_role`]. That keeps a
/// session actor's decision identical to the equivalent card-keyed actor's
/// decision, with no duplicate containment logic. All ambiguous states deny
/// closed, and cardless authority remains denied until PR11 lands.
pub async fn enforce_role_resolving_session<T: WriteTx + ?Sized + Send>(
    tx: &mut T,
    actor: &ActorId,
    event: &Event,
    scope: &EventScope,
    cache: &CardRoleCache,
    wave_area_cache: &WaveAreaCache,
) -> std::result::Result<(), RoleViolation> {
    let session_id = match actor {
        ActorId::AiSpecSession(session)
        | ActorId::AiCodexSession(session)
        | ActorId::AiClaudeSession(session) => session.clone(),
        _ => return enforce_role(actor, event, scope, cache, wave_area_cache),
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

    enforce_role(&synthetic, event, scope, cache, wave_area_cache)
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

    async fn read_card_wave(&mut self, card: &CardId) -> Result<Option<WaveId>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT wave_id FROM cards WHERE id = ?1")
            .bind(card.as_str())
            .fetch_optional(&mut **self)
            .await?;
        Ok(row.map(|(wave,)| WaveId::from(wave)))
    }

    async fn read_wave_area(&mut self, wave: &WaveId) -> Result<Option<AreaId>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT area_id FROM waves WHERE id = ?1")
            .bind(wave.as_str())
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

    /// #1189 §3.6 — may this agent session record into `wave`'s report?
    ///
    /// The criterion is **the session's card**, not the wave's root session:
    /// `card.wave_id == wave` ∧ `role ∈ {Spec, Assistant}`.
    ///
    /// The two halves carry different weight and are worth naming:
    ///
    /// * `role ∈ {Spec, Assistant}` keeps worker-card sessions out (G-A).
    ///   The MCP entry points have a `require_role` of their own, so a
    ///   mutation here is masked on the production path — this half is
    ///   pinned at the gate-unit level only, and that is admitted.
    /// * `card.wave_id == wave` is the load-bearing half (G-A'): once the
    ///   role whitelist admits N assistant cards, nothing else stops one
    ///   wave's assistant from recording into another wave's report.
    ///
    /// **The old `session_id == waves.root_session_id` criterion is gone,
    /// not OR-ed in.** Keeping it as an extra allow-arm would have been a
    /// bypass of the role whitelist rather than a compatibility shim:
    /// `session_mark_wave_root_tx` never checks the root session's card
    /// role or home wave, so a root-marked worker session would sail
    /// straight past the very check G-A exists to make. The root session
    /// of a wave is bound to that wave's Spec card, so it is covered by
    /// the new criterion on its own merits; what it loses is the ability
    /// to be the *only* spec-roled session that may record, which #1189
    /// deliberately gives up (an assistant is by construction not root).
    ///
    /// Liveness is checked explicitly, mirroring
    /// [`enforce_role_resolving_session`]: `session_get_tx` is a plain
    /// `WHERE id = ?1` with no state filter, so a `superseded`/`exited`
    /// row still resolves and still carries its `card_id`. Production reaches
    /// this state on every resume: `session_supersede_active_tx` — reached
    /// through `session_supersede_and_start_tx` — only flips
    /// `worker_sessions.state` to `superseded`. The row stays, bound to the
    /// same card on the same wave, so both halves of the card criterion still
    /// admit the predecessor.
    ///
    /// Moving `waves.root_session_id` is a *separate and conditional* path,
    /// not part of the supersede: `session_repoint_current_links_tx` calls
    /// `session_mark_wave_root_tx` only when the successor is a `Planner` in
    /// an active-authority state. So the old root-session criterion got
    /// liveness for free only on that path (a Planner resume moves the pointer
    /// off the predecessor); the card criterion never gets it, on any path, so
    /// the check has to be written down.
    ///
    /// Every unresolvable step denies: no session row, a session that is no
    /// longer an active authority, a cardless session, an unknown card, an
    /// unknown card wave.
    pub async fn decide_recorder<T>(&self, tx: &mut T, wave: &WaveId) -> Result<GateDecision>
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
        let Some(role) = tx.read_card_role(&card_id).await? else {
            return Ok(GateDecision::Deny(format!(
                "session {session_id} is bound to unknown card {card_id}"
            )));
        };
        if !matches!(role, CardRole::Spec | CardRole::Assistant) {
            return Ok(GateDecision::Deny(format!(
                "session {session_id} card {card_id} has role {role:?}, which may not record"
            )));
        }
        let Some(card_wave) = tx.read_card_wave(&card_id).await? else {
            return Ok(GateDecision::Deny(format!(
                "session {session_id} card {card_id} has no resolvable wave"
            )));
        };
        if &card_wave != wave {
            return Ok(GateDecision::Deny(format!(
                "session {session_id} card {card_id} lives on wave {card_wave}, not {wave}"
            )));
        }
        Ok(GateDecision::Allow)
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
    use crate::model::{Area, AreaKind, Wave, WaveLifecycle};
    use crate::worker::{
        LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSessionState,
    };

    struct FakeWriteTx {
        root_session_id: Option<WorkerSessionId>,
        worker_session: Option<WorkerSessionRow>,
        worker_session_reads: usize,
        worker_session_read_error: bool,
        /// `cards` rows this fake knows about: id → (role, home wave).
        cards: Vec<(CardId, CardRole, WaveId)>,
    }

    impl FakeWriteTx {
        fn new() -> Self {
            Self {
                root_session_id: None,
                worker_session: None,
                worker_session_reads: 0,
                worker_session_read_error: false,
                cards: Vec::new(),
            }
        }

        fn with_card(mut self, card: &str, role: CardRole, wave: &str) -> Self {
            self.cards
                .push((CardId::from(card), role, WaveId::from(wave)));
            self
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

        async fn read_card_role(&mut self, card: &CardId) -> Result<Option<CardRole>> {
            Ok(self
                .cards
                .iter()
                .find(|(id, _, _)| id == card)
                .map(|(_, role, _)| *role))
        }

        async fn read_card_wave(&mut self, card: &CardId) -> Result<Option<WaveId>> {
            Ok(self
                .cards
                .iter()
                .find(|(id, _, _)| id == card)
                .map(|(_, _, wave)| wave.clone()))
        }

        async fn read_wave_area(&mut self, _wave: &WaveId) -> Result<Option<AreaId>> {
            Ok(None)
        }
    }

    fn agent(session_id: &str) -> Principal {
        Principal::Agent {
            session_id: WorkerSessionId::from(session_id),
            wave_id: WaveId::from("wave-1"),
            area_id: AreaId::from("area-1"),
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

    fn wave(id: &str, area: &str) -> Wave {
        Wave {
            id: WaveId::from(id),
            area_id: AreaId::from(area),
            title: "t".into(),
            sort: 1.0,
            archived_at: None,
            pinned_at: None,
            lifecycle: WaveLifecycle::Draft,
            cwd_wire_alias: String::new(),
            template_id: None,
            plugin_scope: None,
            purpose: None,
            template_input: None,
            terminal_at: None,
            workspace: Default::default(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn card_scope(card: &str, wave: &str, area: &str) -> EventScope {
        EventScope::Card {
            card: CardId::from(card),
            wave: WaveId::from(wave),
            area: AreaId::from(area),
        }
    }

    fn wave_scope(wave: &str, area: &str) -> EventScope {
        EventScope::Wave {
            wave: WaveId::from(wave),
            area: AreaId::from(area),
        }
    }

    fn wave_updated() -> Event {
        Event::WaveUpdated(crate::event::WaveUpdatedPayload::new(wave("w", "c"), None))
    }

    fn area_updated() -> Event {
        Event::AreaUpdated(Area {
            id: AreaId::from("c"),
            name: "n".into(),
            color: "#fff".into(),
            sort: 1.0,
            kind: AreaKind::User,
            created_at: 0,
            updated_at: 0,
        })
    }

    fn seeded_caches(card: &CardId, role: CardRole) -> (CardRoleCache, WaveAreaCache) {
        let cache = CardRoleCache::new();
        cache.insert(card.clone(), role, WaveId::from("w"));
        let wcc = WaveAreaCache::new();
        wcc.insert(WaveId::from("w"), AreaId::from("c"));
        (cache, wcc)
    }

    /// #1189 §3.6 — one table for the whole recorder criterion, so the two
    /// halves (`role ∈ {Spec, Assistant}` and `card.wave_id == wave`) are
    /// each pinned by a row that flips when that half is removed.
    async fn recorder_grant_for(
        session: &str,
        card: Option<&str>,
        card_role: CardRole,
        card_wave: &str,
        target_wave: &str,
    ) -> bool {
        let mut tx =
            FakeWriteTx::with_worker_session(worker_session(session, card.map(CardId::from)));
        if let Some(card) = card {
            tx = tx.with_card(card, card_role, card_wave);
        }
        PrincipalDecisionGate::new(agent(session))
            .recorder_grant(&mut tx, &WaveId::from(target_wave))
            .await
            .expect("recorder grant computes")
    }

    #[tokio::test]
    async fn recorder_grant_admits_spec_and_assistant_cards_of_the_target_wave() {
        assert!(
            recorder_grant_for(
                "s-spec",
                Some("card-spec"),
                CardRole::Spec,
                "wave-1",
                "wave-1"
            )
            .await,
            "the wave's spec card may record — this is the path the old \
             root-session criterion used to serve"
        );
        assert!(
            recorder_grant_for(
                "s-assistant",
                Some("card-assistant"),
                CardRole::Assistant,
                "wave-1",
                "wave-1"
            )
            .await,
            "#1189: an assistant card on the wave records into the same report"
        );
    }

    /// G-A. Honest about its reach: the MCP entry points carry a
    /// `require_role` of their own, so this half is only observable here.
    #[tokio::test]
    async fn recorder_grant_refuses_a_worker_card_session() {
        assert!(
            !recorder_grant_for(
                "s-worker",
                Some("card-worker"),
                CardRole::Worker,
                "wave-1",
                "wave-1"
            )
            .await
        );
        assert!(
            !recorder_grant_for(
                "s-report",
                Some("card-report"),
                CardRole::ReportCard,
                "wave-1",
                "wave-1"
            )
            .await
        );
    }

    /// G-A' — the load-bearing half. Same role, same everything, different
    /// home wave: drop `card.wave_id == wave` from `decide_recorder` and
    /// this is the assertion that goes red.
    #[tokio::test]
    async fn recorder_grant_refuses_a_card_from_another_wave() {
        assert!(
            !recorder_grant_for(
                "s-foreign-assistant",
                Some("card-foreign-assistant"),
                CardRole::Assistant,
                "wave-2",
                "wave-1"
            )
            .await,
            "another wave's assistant must not record into this wave's report"
        );
        assert!(
            !recorder_grant_for(
                "s-foreign-spec",
                Some("card-foreign-spec"),
                CardRole::Spec,
                "wave-2",
                "wave-1"
            )
            .await,
            "and neither may another wave's spec — the role whitelist alone \
             is not containment"
        );
    }

    #[tokio::test]
    async fn recorder_grant_denies_every_unresolvable_step() {
        // No session row at all (the fake only answers for its own id).
        let mut tx = FakeWriteTx::new();
        assert!(
            !PrincipalDecisionGate::new(agent("ghost"))
                .recorder_grant(&mut tx, &WaveId::from("wave-1"))
                .await
                .expect("missing session row is a denial, not an error")
        );
        // Session row, no card binding.
        assert!(!recorder_grant_for("s-cardless", None, CardRole::Spec, "wave-1", "wave-1").await);
        // Session bound to a card that has no `cards` row.
        let mut tx = FakeWriteTx::with_worker_session(worker_session(
            "s-dangling",
            Some(CardId::from("card-gone")),
        ));
        assert!(
            !PrincipalDecisionGate::new(agent("s-dangling"))
                .recorder_grant(&mut tx, &WaveId::from("wave-1"))
                .await
                .expect("unknown card is a denial, not an error")
        );
    }

    /// #1189 §3.6 liveness — a session row whose card passes *both* halves of
    /// the recorder criterion, parameterised only on `worker_sessions.state`.
    /// `session_get_tx` is `WHERE id = ?1` with no state filter, so every one
    /// of these rows resolves; only `is_active_authority` separates them.
    async fn recorder_decision_for_state(state: WorkerSessionState) -> GateDecision {
        let mut session = worker_session("s-spec", Some(CardId::from("card-spec")));
        session.state = state;
        let mut tx =
            FakeWriteTx::with_worker_session(session).with_card("card-spec", CardRole::Spec, "w-1");
        PrincipalDecisionGate::new(agent("s-spec"))
            .decide_recorder(&mut tx, &WaveId::from("w-1"))
            .await
            .expect("recorder decision computes")
    }

    /// The predecessor row a resume leaves behind must not keep recording
    /// rights. `session_supersede_active_tx` (reached through
    /// `session_supersede_and_start_tx`) only flips the old row's state to
    /// `superseded`; the row keeps its `card_id` on the same wave, so the card
    /// criterion alone still admits it. Moving `waves.root_session_id` off the
    /// predecessor is a different path — `session_repoint_current_links_tx` →
    /// `session_mark_wave_root_tx` — and runs only when the successor is an
    /// active-authority `Planner`. The old root criterion therefore got this
    /// for free only on that path; the card criterion has to check it
    /// explicitly, on every path.
    #[tokio::test]
    async fn recorder_grant_refuses_a_session_that_is_no_longer_an_active_authority() {
        for state in [
            WorkerSessionState::Superseded,
            WorkerSessionState::Exited,
            WorkerSessionState::Failed,
        ] {
            assert!(
                matches!(
                    recorder_decision_for_state(state).await,
                    GateDecision::Deny(_)
                ),
                "a {state:?} session on this wave's spec card must not record"
            );
        }
        for state in [
            WorkerSessionState::Starting,
            WorkerSessionState::Running,
            WorkerSessionState::Idle,
            WorkerSessionState::TurnPending,
        ] {
            assert_eq!(
                recorder_decision_for_state(state).await,
                GateDecision::Allow,
                "a {state:?} session on this wave's spec card still records"
            );
        }
    }

    /// The deny message used to claim "no live session row" for a read that is
    /// not a live query at all. The two failures are now distinct facts and
    /// must stay distinguishable, or the message is decoration again.
    #[tokio::test]
    async fn recorder_deny_distinguishes_a_missing_row_from_a_dead_one() {
        let mut tx = FakeWriteTx::new();
        let missing = PrincipalDecisionGate::new(agent("ghost"))
            .decide_recorder(&mut tx, &WaveId::from("w-1"))
            .await
            .expect("missing row is a denial, not an error");
        let GateDecision::Deny(missing) = missing else {
            panic!("a session with no row must be denied");
        };
        assert!(
            missing.contains("has no session row"),
            "missing-row denial should say the row is absent, got {missing:?}"
        );

        let dead = recorder_decision_for_state(WorkerSessionState::Superseded).await;
        let GateDecision::Deny(dead) = dead else {
            panic!("a superseded session must be denied");
        };
        assert!(
            dead.contains("is no longer an active authority") && dead.contains("Superseded"),
            "dead-row denial should name the state it found, got {dead:?}"
        );
        assert_ne!(
            missing, dead,
            "the two denials must not collapse into one message"
        );
    }

    /// The root-session criterion is *gone*, not OR-ed in. Marking a
    /// worker-card session as the wave root used to be — and must not
    /// become again — a way around the role whitelist: nothing in
    /// `session_mark_wave_root_tx` checks the root card's role.
    #[tokio::test]
    async fn a_root_marked_worker_session_is_still_refused() {
        let mut tx = FakeWriteTx::with_worker_session(worker_session(
            "s-root",
            Some(CardId::from("card-worker")),
        ))
        .with_card("card-worker", CardRole::Worker, "wave-1");
        tx.root_session_id = Some(WorkerSessionId::from("s-root"));

        assert!(
            !PrincipalDecisionGate::new(agent("s-root"))
                .recorder_grant(&mut tx, &WaveId::from("wave-1"))
                .await
                .expect("root grant computes"),
            "being the wave root must not buy a worker card recording rights"
        );
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
            &area_updated(),
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
            &area_updated(),
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
        let wcc = WaveAreaCache::new();
        let ghost = CardId::from("ghost");
        let mut tx =
            FakeWriteTx::with_worker_session(worker_session("s-spec-ghost", Some(ghost.clone())));

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiSpecSession(WorkerSessionId::from("s-spec-ghost")),
            &area_updated(),
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
            &area_updated(),
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
        let wcc = WaveAreaCache::new();
        let mut tx = FakeWriteTx::with_worker_session(worker_session(
            "s-empty-card",
            Some(CardId::from("")),
        ));

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("s-empty-card")),
            &area_updated(),
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
        let wcc = WaveAreaCache::new();
        let mut tx = FakeWriteTx::new();

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("missing")),
            &area_updated(),
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
        let wcc = WaveAreaCache::new();
        let mut tx = FakeWriteTx::with_worker_session(worker_session("cardless", None));

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("cardless")),
            &area_updated(),
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
    async fn session_resolver_denies_terminal_session_rows_before_card_delegation() {
        let worker_card = CardId::from("worker-card");
        let (cache, wcc) = seeded_caches(&worker_card, CardRole::Worker);

        for (session_id, state) in [
            ("s-exited", WorkerSessionState::Exited),
            ("s-failed", WorkerSessionState::Failed),
            ("s-superseded", WorkerSessionState::Superseded),
        ] {
            let mut session = worker_session(session_id, Some(worker_card.clone()));
            session.state = state;
            let mut tx = FakeWriteTx::with_worker_session(session);

            let err = enforce_role_resolving_session(
                &mut tx,
                &ActorId::AiCodexSession(WorkerSessionId::from(session_id)),
                &area_updated(),
                &card_scope(worker_card.as_str(), "w", "c"),
                &cache,
                &wcc,
            )
            .await
            .expect_err("terminal session row must deny before card delegation");

            match err {
                RoleViolation::SessionNotActive { session } => {
                    assert_eq!(session, WorkerSessionId::from(session_id));
                }
                other => panic!("unexpected violation for {state:?}: {other:?}"),
            }
            assert_eq!(tx.worker_session_reads, 1);
        }
    }

    #[tokio::test]
    async fn session_resolver_denies_read_error() {
        let cache = CardRoleCache::new();
        let wcc = WaveAreaCache::new();
        let mut tx = FakeWriteTx::with_worker_session_read_error();

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("s-error")),
            &area_updated(),
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
        let wcc = WaveAreaCache::new();
        let mut tx = FakeWriteTx::new();

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("")),
            &area_updated(),
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
        let wcc = WaveAreaCache::new();
        let ghost = CardId::from("ghost");
        let mut tx =
            FakeWriteTx::with_worker_session(worker_session("s-ghost", Some(ghost.clone())));

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("s-ghost")),
            &area_updated(),
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
            &area_updated(),
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
        let event = area_updated();
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
