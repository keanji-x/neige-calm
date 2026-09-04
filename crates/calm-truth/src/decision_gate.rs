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
    async fn read_track_root_session_id(
        &mut self,
        track: &TrackId,
    ) -> Result<Option<WorkerSessionId>>;

    async fn read_worker_session(
        &mut self,
        id: &WorkerSessionId,
    ) -> Result<Option<WorkerSessionRow>>;

    async fn read_card_role(&mut self, card: &CardId) -> Result<Option<CardRole>>;

    /// #1189 §3.6 — the card's home track, the load-bearing half of the
    /// recorder criterion. Same shape as [`WriteTx::read_card_role`]: a
    /// live `cards` read inside the caller's write transaction, `None`
    /// for a row that isn't there (deny, never assume).
    async fn read_card_track(&mut self, card: &CardId) -> Result<Option<TrackId>>;

    async fn read_track_area(&mut self, track: &TrackId) -> Result<Option<AreaId>>;
}

/// Resolve session-keyed actors through the live `worker_sessions` row, then
/// reuse the sync role gate for the final containment decision.
///
/// This is HP1-a-2's option (b) seam: session→card is a live DB read at gate
/// time, not the option-(a) session→card cache, so deleted, unknown, or
/// never-committed sessions deny by construction. Once a session resolves to a
/// bound card, card→{role,track,area} still comes from the existing
/// `CardRoleCache` and `TrackAreaCache` through [`enforce_role`]. That keeps a
/// session actor's decision identical to the equivalent card-keyed actor's
/// decision, with no duplicate containment logic. All ambiguous states deny
/// closed, and cardless authority remains denied until PR11 lands.
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
        | ActorId::AiClaudeSession(session) => session.clone(),
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

    let card_id = session
        .card_id
        .ok_or_else(|| RoleViolation::CardlessSessionDenied {
            session: session_id.clone(),
        })?;

    let synthetic = match actor {
        ActorId::AiPlannerSession(_) => {
            // Live read gave ground-truth card_id; the AiPlanner path in enforce_role
            // does not re-check role/scope for ordinary events, so verify the card
            // is actually Planner-roled before granting planner authority. Fail-closed on
            // non-Planner or unknown card. Worker variants below stay delegated;
            // enforce_role's self-scope/UnknownCard arms already cover every role.
            if cache.get(&card_id) != Some(CardRole::Planner) {
                return Err(RoleViolation::SessionPlannerRoleMismatch {
                    session: session_id,
                    card: card_id,
                });
            }
            ActorId::AiPlanner(card_id)
        }
        ActorId::AiCodexSession(_) => ActorId::AiCodex(card_id),
        ActorId::AiClaudeSession(_) => ActorId::AiClaude(card_id),
        _ => unreachable!("session actor match above guarantees session variant"),
    };

    enforce_role(&synthetic, event, scope, cache, track_area_cache)
}

/// Same policy as [`enforce_role_resolving_session`], with `card → {role,
/// home track}` and `track → area` read live from the caller's write
/// transaction instead of from the write-through
/// [`CardRoleCache`] / [`TrackAreaCache`] pair.
///
/// ## Why a transaction read instead of the caches
///
/// The caches are a *performance* substrate, not a *correctness* substrate:
/// [`WriteTx::read_card_role`] already runs `SELECT role FROM cards WHERE
/// id = ?1` inside the caller's transaction, so a role written earlier in the
/// same transaction (e.g. by `card_create_with_id_tx`) is visible to it — the
/// transaction read is at least as fresh as the write-through cache. It is
/// also immune to the two `CardRoleCache` instances in the tree (`SqlxRepo`
/// holds one, `AppState::new` allocates another) drifting apart, which a
/// seam that grabbed whichever cache was nearest would silently suffer.
///
/// Cost, inside the write lock: at most nine primary-key `SELECT`s per event.
/// The bound is `2` session reads (one here to find the hydration key, one
/// authoritative read in `enforce_role_resolving_session`) + `2 cards × 2`
/// (`role` and `track_id`, for the actor-or-session card and a distinct
/// `scope.card`) + `3 tracks × 1` (`scope.track` plus each card's home track).
/// The common shapes are far cheaper: a cardless actor (`User`, `Kernel`,
/// `KernelDispatcher`) with a card scope costs at most four, and the same
/// actor with a system scope costs none.
///
/// ## Why this is a substitution and not a re-implementation
///
/// This function does not restate any of the gate's branches. It hydrates two
/// throwaway cache instances from the transaction and then calls
/// [`enforce_role_resolving_session`] — the original — to make the decision.
/// See [`hydrate_role_caches_from_tx`] for why the hydrated key set is
/// complete by construction rather than by mirroring the gate's logic.
pub async fn enforce_role_resolving_session_from_tx<T: WriteTx + ?Sized + Send>(
    tx: &mut T,
    actor: &ActorId,
    event: &Event,
    scope: &EventScope,
) -> std::result::Result<(), RoleViolation> {
    // A session actor resolves to a card that the syntactic closure of
    // `(actor, scope)` cannot see, so look it up first and hand it to the
    // hydrator as an extra key. Errors and absences are deliberately
    // *not* interpreted here: `enforce_role_resolving_session` below does
    // the authoritative session read and owns every deny reason for it.
    // Reading it twice costs one extra `SELECT` and keeps the decision in
    // exactly one place.
    let session_card = match actor {
        ActorId::AiPlannerSession(session)
        | ActorId::AiCodexSession(session)
        | ActorId::AiClaudeSession(session)
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

/// The `CardId` an actor carries, if its variant carries one.
///
/// Session variants carry a `WorkerSessionId`, not a card; their card is
/// resolved from `worker_sessions` and passed to
/// [`hydrate_role_caches_from_tx`] as `extra_card`.
fn actor_card_id(actor: &ActorId) -> Option<&CardId> {
    match actor {
        ActorId::AiCodex(card) | ActorId::AiClaude(card) | ActorId::AiPlanner(card) => Some(card),
        ActorId::User
        | ActorId::Kernel
        | ActorId::KernelDispatcher
        | ActorId::Plugin(_)
        | ActorId::AiPlannerSession(_)
        | ActorId::AiCodexSession(_)
        | ActorId::AiClaudeSession(_) => None,
    }
}

/// Fill throwaway [`CardRoleCache`] / [`TrackAreaCache`] instances from the
/// caller's transaction with every row [`enforce_role`] can key on.
///
/// ## Completeness (why this is not a mirror of the gate's branches)
///
/// [`enforce_role`] is a pure function of `(actor, event, scope, cache,
/// track_area_cache)`, and the only cache keys it can name are identifiers it
/// can *reach*:
///
///   * `cache.get(card)` / `cache.track_of(card)` are only ever called with
///     the `CardId` carried by `actor` or the `CardId` carried by `scope`
///     (`EventScope::Card { card, .. }`, via `enforce_card_scope`'s `target`);
///   * `track_area_cache.area_of(track)` is only ever called with a home track
///     that came out of `cache.track_of(..)`;
///   * no cache key is ever derived from the event payload.
///
/// So the syntactic closure of `actor ∪ scope ∪ {resolved session card}` under
/// `card → home track → area` is a superset of the gate's key set for *any*
/// branch it takes, present or future-added, as long as the gate keeps keying
/// only on identifiers reachable from its arguments. `scope`'s own track is
/// hydrated too, which costs one `SELECT` and removes the need to reason about
/// which side of a `scope.track` comparison is read from where.
///
/// A row that is absent from the transaction is left absent from the cache,
/// and `enforce_role` denies on that miss for AI worker actors.
///
/// ## Why reading the transaction is safe: **the verdict and the event share
/// one transaction**
///
/// Every row this function reads comes out of the caller's `tx` — the three
/// reads below are `tx.read_card_role`, `tx.read_card_track` and
/// `tx.read_track_area`. On the production entrance that is the *same*
/// transaction the event is inserted into: `append_decision_event_in_tx` and
/// its batch form (`db/sqlite/events.rs`) pass one `&mut Transaction` first to
/// `gated::authorize` — which calls
/// [`enforce_role_resolving_session_from_tx`], hence this hydrator — and then
/// to `SqlxRepo::event_append_in_tx`.
///
/// So the verdict is computed from the same view of `cards` / `tracks` that
/// the `events` row is written into: the committed tables as amended by this
/// transaction's own pending writes. Every row the gate read was therefore
/// either already committed, or a pending write of this same transaction — and
/// in the second case it commits exactly when the event commits and rolls back
/// exactly when the event rolls back. **The event can never be committed on
/// the strength of a row state that never committed. That is the entire safety
/// argument, and it does not appeal to any direction of disagreement with the
/// write-through caches.**
///
/// ## Evidence that the caches lack this property: they diverge **both** ways
///
/// "Absent from the DB ⇒ absent from the cache" is *not* true of
/// `CardRoleCache` / `TrackAreaCache`, so this substrate is not equivalent to
/// them on every possible state — and the disagreement is not one-directional:
///
///   * **Rolled-back create ⇒ the cache is the more permissive side.**
///     `card_create_with_id_tx` (`db/sqlite/card.rs`) inserts into
///     `CardRoleCache` before commit, and its comment records that "a txn
///     rollback leaves a stale entry". The cached gate can then admit an
///     `AiCodex` / `AiClaude` actor for a card that has no row, while this
///     hydrator finds no row, leaves the card out, and that same arm of
///     `enforce_role` (`role_gate.rs`) returns `UnknownCard`.
///   * **Rolled-back delete ⇒ the cache is the more restrictive side.**
///     `card_delete_tx` (`db/sqlite/card.rs`) calls `card_role_cache.remove`
///     before commit, and its comment records that "a txn rollback would leave
///     the cache temporarily missing an entry". In that state the cached gate
///     denies an `AiCodex` / `AiClaude` actor with `UnknownCard` although the
///     card's row is still there, while this hydrator reads that row, so the
///     miss-deny never fires here. `track_delete_tx` (`db/sqlite/track.rs`)
///     removes from `TrackAreaCache` before commit in the same shape one level
///     up. That one is *not* a divergence: `enforce_card_scope`
///     (`role_gate.rs`) reaches the track→area lookup through a fail-closed
///     `let ... else` that denies with `RoleLookupFailed` (#1381), and this
///     hydrator leaves the same entry out when the `tracks` read returns no
///     row, so both substrates deny alike on a track→area miss.
///
/// These two are recorded as *evidence*, not as the argument: because the
/// caches can drift either way, "the transaction read is always the stricter
/// side" would be false, which is exactly why the safety argument above is
/// stated against the transaction's own view of the tables instead. The
/// equivalence matrix in this file's tests can see neither case — both of its
/// substrates are projected from one consistent `World` — so they are recorded
/// here rather than asserted there.
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

/// Pluggable "should this write be allowed" policy.
///
/// **Test-only abstraction.** After #1252 S3′ there is no production
/// implementor and no production consumer: the event-append seam takes its
/// decision from [`enforce_role_resolving_session_from_tx`] instead of from an
/// injected gate, and the only implementors left in the tree
/// (`PermissiveGate` here, `DenyGate` / `DenyOnRoot` / `RootOnlyGate` in
/// `calm-truth-test-harness`) exist to drive invariant fixtures. It is kept
/// behind `cfg(any(test, feature = "test-helpers"))` so a permissive stub
/// cannot be handed to a production write path again — a seam you *cannot*
/// pass "no policy" to is stronger than a seam whose default policy is a real
/// gate.
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

/// Allow-everything [`DecisionGate`]. Test scaffolding only — see the trait's
/// note. Gated for the same reason.
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

    /// #1189 §3.6 — may this agent session record into `track`'s report?
    ///
    /// The criterion is **the session's card**, not the track's root session:
    /// `card.track_id == track` ∧ `role ∈ {Planner, Assistant}`.
    ///
    /// The two halves carry different weight and are worth naming:
    ///
    /// * `role ∈ {Planner, Assistant}` keeps worker-card sessions out (G-A).
    ///   The MCP entry points have a `require_role` of their own, so a
    ///   mutation here is masked on the production path — this half is
    ///   pinned at the gate-unit level only, and that is admitted.
    /// * `card.track_id == track` is the load-bearing half (G-A'): once the
    ///   role whitelist admits N assistant cards, nothing else stops one
    ///   track's assistant from recording into another track's report.
    ///
    /// **The old `session_id == tracks.root_session_id` criterion is gone,
    /// not OR-ed in.** Keeping it as an extra allow-arm would have been a
    /// bypass of the role whitelist rather than a compatibility shim:
    /// `session_mark_track_root_tx` never checks the root session's card
    /// role or home track, so a root-marked worker session would sail
    /// straight past the very check G-A exists to make. The root session
    /// of a track is bound to that track's Planner card, so it is covered by
    /// the new criterion on its own merits; what it loses is the ability
    /// to be the *only* planner-roled session that may record, which #1189
    /// deliberately gives up (an assistant is by construction not root).
    ///
    /// Liveness is checked explicitly, mirroring
    /// [`enforce_role_resolving_session`]: `session_get_tx` is a plain
    /// `WHERE id = ?1` with no state filter, so a `superseded`/`exited`
    /// row still resolves and still carries its `card_id`. Production reaches
    /// this state on every resume: `session_supersede_active_tx` — reached
    /// through `session_supersede_and_start_tx` — only flips
    /// `worker_sessions.state` to `superseded`. The row stays, bound to the
    /// same card on the same track, so both halves of the card criterion still
    /// admit the predecessor.
    ///
    /// Moving `tracks.root_session_id` is a *separate and conditional* path,
    /// not part of the supersede: `session_repoint_current_links_tx` calls
    /// `session_mark_track_root_tx` only when the successor is a `Planner` in
    /// an active-authority state. So the old root-session criterion got
    /// liveness for free only on that path (a Planner resume moves the pointer
    /// off the predecessor); the card criterion never gets it, on any path, so
    /// the check has to be written down.
    ///
    /// Every unresolvable step denies: no session row, a session that is no
    /// longer an active authority, a cardless session, an unknown card, an
    /// unknown card track.
    pub async fn decide_recorder<T>(&self, tx: &mut T, track: &TrackId) -> Result<GateDecision>
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
        if !matches!(role, CardRole::Planner | CardRole::Assistant) {
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

/// Run `f` inside one write transaction behind a [`DecisionGate`].
///
/// Test-only, and gated for the same reason as the trait: it has no production
/// call site — the invariant fixtures in `calm-truth-test-harness` are its only
/// consumers.
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
mod tests {
    use super::*;
    use crate::model::{Area, AreaKind, Track, TrackLifecycle};
    use crate::worker::{
        LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSessionState,
    };

    struct FakeWriteTx {
        root_session_id: Option<WorkerSessionId>,
        worker_session: Option<WorkerSessionRow>,
        worker_session_reads: usize,
        worker_session_read_error: bool,
        /// `cards` rows this fake knows about: id → (role, home track).
        cards: Vec<(CardId, CardRole, TrackId)>,
        /// `tracks` rows this fake knows about: id → area.
        tracks: Vec<(TrackId, AreaId)>,
    }

    impl FakeWriteTx {
        fn new() -> Self {
            Self {
                root_session_id: None,
                worker_session: None,
                worker_session_reads: 0,
                worker_session_read_error: false,
                cards: Vec::new(),
                tracks: Vec::new(),
            }
        }

        fn with_card(mut self, card: &str, role: CardRole, track: &str) -> Self {
            self.cards
                .push((CardId::from(card), role, TrackId::from(track)));
            self
        }

        fn with_track(mut self, track: &str, area: &str) -> Self {
            self.tracks.push((TrackId::from(track), AreaId::from(area)));
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
        async fn read_track_root_session_id(
            &mut self,
            _track: &TrackId,
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

        async fn read_card_track(&mut self, card: &CardId) -> Result<Option<TrackId>> {
            Ok(self
                .cards
                .iter()
                .find(|(id, _, _)| id == card)
                .map(|(_, _, track)| track.clone()))
        }

        async fn read_track_area(&mut self, track: &TrackId) -> Result<Option<AreaId>> {
            Ok(self
                .tracks
                .iter()
                .find(|(id, _)| id == track)
                .map(|(_, area)| area.clone()))
        }
    }

    fn agent(session_id: &str) -> Principal {
        Principal::Agent {
            session_id: WorkerSessionId::from(session_id),
            track_id: TrackId::from("track-1"),
            area_id: AreaId::from("area-1"),
        }
    }

    fn worker_session(session_id: &str, card_id: Option<CardId>) -> WorkerSession {
        WorkerSession {
            id: WorkerSessionId::from(session_id),
            track_id: TrackId::from("w"),
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

    fn track(id: &str, area: &str) -> Track {
        Track {
            id: TrackId::from(id),
            area_id: AreaId::from(area),
            title: "t".into(),
            sort: 1.0,
            archived_at: None,
            pinned_at: None,
            lifecycle: TrackLifecycle::Draft,
            cwd_wire_alias: String::new(),
            template_id: None,
            plugin_scope: None,
            purpose: None,
            template_input: None,
            terminal_at: None,
            recipe_id: None,
            recipe_revision: None,
            workspace: Default::default(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn card_scope(card: &str, track: &str, area: &str) -> EventScope {
        EventScope::Card {
            card: CardId::from(card),
            track: TrackId::from(track),
            area: AreaId::from(area),
        }
    }

    fn track_scope(track: &str, area: &str) -> EventScope {
        EventScope::Track {
            track: TrackId::from(track),
            area: AreaId::from(area),
        }
    }

    fn track_updated() -> Event {
        Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
            track("w", "c"),
            None,
        ))
    }

    fn area_updated() -> Event {
        Event::AreaUpdated(Area {
            id: AreaId::from("c"),
            name: "n".into(),
            color: "#fff".into(),
            sort: 1.0,
            kind: AreaKind::User,
            default_template_id: None,
            default_cwd: None,
            created_at: 0,
            updated_at: 0,
        })
    }

    fn seeded_caches(card: &CardId, role: CardRole) -> (CardRoleCache, TrackAreaCache) {
        let cache = CardRoleCache::new();
        cache.insert(card.clone(), role, TrackId::from("w"));
        let wcc = TrackAreaCache::new();
        wcc.insert(TrackId::from("w"), AreaId::from("c"));
        (cache, wcc)
    }

    /// #1189 §3.6 — one table for the whole recorder criterion, so the two
    /// halves (`role ∈ {Planner, Assistant}` and `card.track_id == track`) are
    /// each pinned by a row that flips when that half is removed.
    async fn recorder_grant_for(
        session: &str,
        card: Option<&str>,
        card_role: CardRole,
        card_track: &str,
        target_track: &str,
    ) -> bool {
        let mut tx =
            FakeWriteTx::with_worker_session(worker_session(session, card.map(CardId::from)));
        if let Some(card) = card {
            tx = tx.with_card(card, card_role, card_track);
        }
        PrincipalDecisionGate::new(agent(session))
            .recorder_grant(&mut tx, &TrackId::from(target_track))
            .await
            .expect("recorder grant computes")
    }

    #[tokio::test]
    async fn recorder_grant_admits_planner_and_assistant_cards_of_the_target_track() {
        assert!(
            recorder_grant_for(
                "s-planner",
                Some("card-planner"),
                CardRole::Planner,
                "track-1",
                "track-1"
            )
            .await,
            "the track's planner card may record — this is the path the old \
             root-session criterion used to serve"
        );
        assert!(
            recorder_grant_for(
                "s-assistant",
                Some("card-assistant"),
                CardRole::Assistant,
                "track-1",
                "track-1"
            )
            .await,
            "#1189: an assistant card on the track records into the same report"
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
                "track-1",
                "track-1"
            )
            .await
        );
        assert!(
            !recorder_grant_for(
                "s-report",
                Some("card-report"),
                CardRole::ReportCard,
                "track-1",
                "track-1"
            )
            .await
        );
    }

    /// G-A' — the load-bearing half. Same role, same everything, different
    /// home track: drop `card.track_id == track` from `decide_recorder` and
    /// this is the assertion that goes red.
    #[tokio::test]
    async fn recorder_grant_refuses_a_card_from_another_track() {
        assert!(
            !recorder_grant_for(
                "s-foreign-assistant",
                Some("card-foreign-assistant"),
                CardRole::Assistant,
                "track-2",
                "track-1"
            )
            .await,
            "another track's assistant must not record into this track's report"
        );
        assert!(
            !recorder_grant_for(
                "s-foreign-planner",
                Some("card-foreign-planner"),
                CardRole::Planner,
                "track-2",
                "track-1"
            )
            .await,
            "and neither may another track's planner — the role whitelist alone \
             is not containment"
        );
    }

    #[tokio::test]
    async fn recorder_grant_denies_every_unresolvable_step() {
        // No session row at all (the fake only answers for its own id).
        let mut tx = FakeWriteTx::new();
        assert!(
            !PrincipalDecisionGate::new(agent("ghost"))
                .recorder_grant(&mut tx, &TrackId::from("track-1"))
                .await
                .expect("missing session row is a denial, not an error")
        );
        // Session row, no card binding.
        assert!(
            !recorder_grant_for("s-cardless", None, CardRole::Planner, "track-1", "track-1").await
        );
        // Session bound to a card that has no `cards` row.
        let mut tx = FakeWriteTx::with_worker_session(worker_session(
            "s-dangling",
            Some(CardId::from("card-gone")),
        ));
        assert!(
            !PrincipalDecisionGate::new(agent("s-dangling"))
                .recorder_grant(&mut tx, &TrackId::from("track-1"))
                .await
                .expect("unknown card is a denial, not an error")
        );
    }

    /// #1189 §3.6 liveness — a session row whose card passes *both* halves of
    /// the recorder criterion, parameterised only on `worker_sessions.state`.
    /// `session_get_tx` is `WHERE id = ?1` with no state filter, so every one
    /// of these rows resolves; only `is_active_authority` separates them.
    async fn recorder_decision_for_state(state: WorkerSessionState) -> GateDecision {
        let mut session = worker_session("s-planner", Some(CardId::from("card-planner")));
        session.state = state;
        let mut tx = FakeWriteTx::with_worker_session(session).with_card(
            "card-planner",
            CardRole::Planner,
            "w-1",
        );
        PrincipalDecisionGate::new(agent("s-planner"))
            .decide_recorder(&mut tx, &TrackId::from("w-1"))
            .await
            .expect("recorder decision computes")
    }

    /// The predecessor row a resume leaves behind must not keep recording
    /// rights. `session_supersede_active_tx` (reached through
    /// `session_supersede_and_start_tx`) only flips the old row's state to
    /// `superseded`; the row keeps its `card_id` on the same track, so the card
    /// criterion alone still admits it. Moving `tracks.root_session_id` off the
    /// predecessor is a different path — `session_repoint_current_links_tx` →
    /// `session_mark_track_root_tx` — and runs only when the successor is an
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
                "a {state:?} session on this track's planner card must not record"
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
                "a {state:?} session on this track's planner card still records"
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
            .decide_recorder(&mut tx, &TrackId::from("w-1"))
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
    /// worker-card session as the track root used to be — and must not
    /// become again — a way around the role whitelist: nothing in
    /// `session_mark_track_root_tx` checks the root card's role.
    #[tokio::test]
    async fn a_root_marked_worker_session_is_still_refused() {
        let mut tx = FakeWriteTx::with_worker_session(worker_session(
            "s-root",
            Some(CardId::from("card-worker")),
        ))
        .with_card("card-worker", CardRole::Worker, "track-1");
        tx.root_session_id = Some(WorkerSessionId::from("s-root"));

        assert!(
            !PrincipalDecisionGate::new(agent("s-root"))
                .recorder_grant(&mut tx, &TrackId::from("track-1"))
                .await
                .expect("root grant computes"),
            "being the track root must not buy a worker card recording rights"
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
    async fn session_resolver_allows_planner_track_updated() {
        let planner_card = CardId::from("planner-card");
        let (cache, wcc) = seeded_caches(&planner_card, CardRole::Planner);
        let mut tx =
            FakeWriteTx::with_worker_session(worker_session("s2", Some(planner_card.clone())));

        let res = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiPlannerSession(WorkerSessionId::from("s2")),
            &track_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        )
        .await;

        assert!(res.is_ok(), "planner session should update track: {res:?}");
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_denies_planner_session_bound_to_worker_card_on_ordinary_event() {
        let worker_card = CardId::from("worker-card");
        let (cache, wcc) = seeded_caches(&worker_card, CardRole::Worker);
        let mut tx = FakeWriteTx::with_worker_session(worker_session(
            "s-planner-worker",
            Some(worker_card.clone()),
        ));

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiPlannerSession(WorkerSessionId::from("s-planner-worker")),
            &area_updated(),
            &card_scope(worker_card.as_str(), "w", "c"),
            &cache,
            &wcc,
        )
        .await
        .expect_err("planner session bound to worker card must deny");

        match err {
            RoleViolation::SessionPlannerRoleMismatch { session, card } => {
                assert_eq!(session, WorkerSessionId::from("s-planner-worker"));
                assert_eq!(card, worker_card);
            }
            other => panic!("unexpected violation: {other:?}"),
        }
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_denies_planner_session_bound_to_unknown_card_on_ordinary_event() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let ghost = CardId::from("ghost");
        let mut tx = FakeWriteTx::with_worker_session(worker_session(
            "s-planner-ghost",
            Some(ghost.clone()),
        ));

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiPlannerSession(WorkerSessionId::from("s-planner-ghost")),
            &area_updated(),
            &card_scope(ghost.as_str(), "w", "c"),
            &cache,
            &wcc,
        )
        .await
        .expect_err("planner session bound to unknown card must deny");

        match err {
            RoleViolation::SessionPlannerRoleMismatch { session, card } => {
                assert_eq!(session, WorkerSessionId::from("s-planner-ghost"));
                assert_eq!(card, ghost);
            }
            other => panic!("unexpected violation: {other:?}"),
        }
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_allows_planner_session_bound_to_planner_card_on_ordinary_event() {
        let planner_card = CardId::from("planner-card");
        let (cache, wcc) = seeded_caches(&planner_card, CardRole::Planner);
        let mut tx = FakeWriteTx::with_worker_session(worker_session(
            "s-planner-ordinary",
            Some(planner_card.clone()),
        ));

        let res = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiPlannerSession(WorkerSessionId::from("s-planner-ordinary")),
            &area_updated(),
            &card_scope(planner_card.as_str(), "w", "c"),
            &cache,
            &wcc,
        )
        .await;

        assert!(
            res.is_ok(),
            "planner session bound to planner card should pass ordinary event: {res:?}"
        );
        assert_eq!(tx.worker_session_reads, 1);
    }

    #[tokio::test]
    async fn session_resolver_denies_empty_card_id_via_worker_variant_sync_gate() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
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
        let wcc = TrackAreaCache::new();
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
        let wcc = TrackAreaCache::new();
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
        let wcc = TrackAreaCache::new();
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
        let wcc = TrackAreaCache::new();
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
        let wcc = TrackAreaCache::new();
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
    async fn session_resolver_denies_worker_track_updated_via_sync_gate() {
        let worker_card = CardId::from("worker-card");
        let (cache, wcc) = seeded_caches(&worker_card, CardRole::Worker);
        let mut tx =
            FakeWriteTx::with_worker_session(worker_session("s-worker-track", Some(worker_card)));

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from("s-worker-track")),
            &track_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        )
        .await
        .expect_err("worker session must not update track");

        assert!(matches!(err, RoleViolation::NotPlannerForTrack { .. }));
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

    #[tokio::test]
    async fn tx_read_gate_denies_when_home_track_row_is_missing() {
        // #1381 — under this substrate the track→area entry exists iff
        // `SELECT area_id FROM tracks WHERE id = ?1` returned a row, so a
        // card whose home track row is absent leaves the lookup empty. The
        // gate runs inside the caller's write transaction (the same one the
        // `events` row is inserted into), so that miss has to come back as
        // `Err`, never as a panic unwinding out of a half-written write.
        let card = CardId::from("orphan");
        let mut tx = FakeWriteTx::new().with_card("orphan", CardRole::Worker, "w-gone");

        let err = enforce_role_resolving_session_from_tx(
            &mut tx,
            &ActorId::AiCodex(card),
            &area_updated(),
            &card_scope("orphan", "w-gone", "c"),
        )
        .await
        .expect_err("a card with no home track row must be denied, not panic");

        assert!(
            matches!(err, RoleViolation::RoleLookupFailed { ref subject }
                if subject == "tracks.area_id(w-gone)"),
            "unexpected violation: {err:?}"
        );
    }
    // -----------------------------------------------------------------
    // #1252 S3′ — equivalence matrix.
    //
    // `enforce_role_resolving_session_from_tx` replaces the write-through
    // caches with live reads from the caller's transaction. That swap is only
    // legitimate if the two substrates produce the *same verdict* on every
    // input **whose two substrates agree**, so this matrix walks
    // (actor × event × scope) and asserts the two functions agree exactly —
    // including on the violation text, so a same-shape-different-reason
    // divergence is caught too.
    //
    // Both substrates are projected from one `World` description, which is
    // what makes this an equivalence test rather than two hand-written
    // expectations that could drift together. Mutating either substrate — the
    // hydrator's key set, the fake's rows, the cache seeding — must turn this
    // red.
    //
    // What it therefore does *not* cover: states where the cache and the DB
    // disagree. Two such states are documented and reachable — a rolled-back
    // `card_create_with_id_tx` leaves a stale `CardRoleCache` entry, and a
    // rolled-back `card_delete_tx` leaves that cache missing one — and the two
    // gates genuinely differ on both, in *opposite* directions: the cache is
    // the permissive side on the first and the restrictive side on the second.
    // Neither direction is what makes the swap legitimate: the
    // transaction read is safe because the verdict and the `events` insert
    // share one transaction. See `hydrate_role_caches_from_tx`'s doc comment
    // for both halves. A single consistent `World` cannot express either
    // state, so the divergences live there as prose, not here as rows.
    // -----------------------------------------------------------------

    /// The rows both substrates are built from.
    struct World {
        cards: Vec<(&'static str, CardRole, &'static str)>,
        tracks: Vec<(&'static str, &'static str)>,
        session: Option<WorkerSessionRow>,
    }

    impl World {
        fn tx(&self) -> FakeWriteTx {
            let mut tx = match &self.session {
                Some(session) => FakeWriteTx::with_worker_session(session.clone()),
                None => FakeWriteTx::new(),
            };
            for (card, role, track) in &self.cards {
                tx = tx.with_card(card, *role, track);
            }
            for (track, area) in &self.tracks {
                tx = tx.with_track(track, area);
            }
            tx
        }

        fn caches(&self) -> (CardRoleCache, TrackAreaCache) {
            let cache = CardRoleCache::new();
            for (card, role, track) in &self.cards {
                cache.insert(CardId::from(*card), *role, TrackId::from(*track));
            }
            let area_cache = TrackAreaCache::new();
            for (track, area) in &self.tracks {
                area_cache.insert(TrackId::from(*track), AreaId::from(*area));
            }
            (cache, area_cache)
        }
    }

    fn matrix_world() -> World {
        World {
            cards: vec![
                ("planner", CardRole::Planner, "w"),
                ("worker", CardRole::Worker, "w"),
                ("report", CardRole::ReportCard, "w"),
                ("assistant", CardRole::Assistant, "w"),
                ("foreign-worker", CardRole::Worker, "w2"),
                // #1381 — a card whose home track has no row / no cache
                // entry. Both substrates must deny its own-scope write with
                // `RoleLookupFailed` rather than one of them panicking.
                ("orphan", CardRole::Worker, "w-gone"),
            ],
            tracks: vec![("w", "c"), ("w2", "c2")],
            session: Some(worker_session("s-live", Some(CardId::from("planner")))),
        }
    }

    fn matrix_actors() -> Vec<ActorId> {
        vec![
            ActorId::User,
            ActorId::Kernel,
            ActorId::KernelDispatcher,
            ActorId::Plugin("p".into()),
            ActorId::AiPlanner(CardId::from("planner")),
            ActorId::AiPlanner(CardId::from("worker")),
            ActorId::AiPlanner(CardId::from("")),
            ActorId::AiCodex(CardId::from("worker")),
            ActorId::AiCodex(CardId::from("planner")),
            ActorId::AiCodex(CardId::from("report")),
            ActorId::AiCodex(CardId::from("assistant")),
            ActorId::AiCodex(CardId::from("foreign-worker")),
            ActorId::AiCodex(CardId::from("ghost")),
            ActorId::AiCodex(CardId::from("orphan")),
            ActorId::AiCodex(CardId::from("")),
            ActorId::AiClaude(CardId::from("worker")),
            // Session actors: live (resolves to the planner card), unknown
            // (no row), and empty (unresolvable).
            ActorId::AiCodexSession(WorkerSessionId::from("s-live")),
            ActorId::AiPlannerSession(WorkerSessionId::from("s-live")),
            ActorId::AiClaudeSession(WorkerSessionId::from("s-live")),
            ActorId::AiCodexSession(WorkerSessionId::from("s-ghost")),
            ActorId::AiCodexSession(WorkerSessionId::from("")),
        ]
    }

    fn matrix_scopes() -> Vec<EventScope> {
        vec![
            EventScope::System,
            EventScope::Area {
                area: AreaId::from("c"),
            },
            track_scope("w", "c"),
            track_scope("w2", "c2"),
            card_scope("worker", "w", "c"),
            card_scope("report", "w", "c"),
            card_scope("assistant", "w", "c"),
            card_scope("planner", "w", "c"),
            // #232 / #234 spoof shapes: right card, wrong track / wrong area.
            card_scope("worker", "w2", "c2"),
            card_scope("worker", "w", "c2"),
            card_scope("foreign-worker", "w2", "c2"),
            card_scope("ghost", "w", "c"),
            // #1381 — own-scope write by a card whose home track row is gone.
            card_scope("orphan", "w-gone", "c"),
        ]
    }

    fn matrix_events() -> Vec<(&'static str, Event)> {
        vec![
            ("track.updated", track_updated()),
            ("area.updated", area_updated()),
            (
                "plan.updated",
                Event::PlanUpdated {
                    track_id: TrackId::from("w"),
                    changed_keys: Vec::new(),
                    agent_message: None,
                },
            ),
            (
                "task.dispatched",
                Event::TaskDispatched {
                    idempotency_key: "k".into(),
                    kind: "codex".into(),
                    agent_message: None,
                },
            ),
            (
                "task.context_frozen",
                Event::TaskContextFrozen {
                    track_id: TrackId::from("w"),
                    task_key: "k".into(),
                    idempotency_key: "k".into(),
                    task_id: "t".into(),
                    refs: Vec::new(),
                    doc_revs: Default::default(),
                    truncated: false,
                },
            ),
            (
                "ratify.resolved",
                Event::RatifyResolved {
                    track_id: TrackId::from("w"),
                    decision: crate::event::RatifyDecision::Grant,
                },
            ),
            (
                "hook.codex",
                Event::CodexHook {
                    card_id: CardId::from("worker"),
                    kind: "hook.codex.pre_tool_use".into(),
                    hook_idempotency_key: "k".into(),
                    payload: serde_json::json!({}),
                },
            ),
        ]
    }

    /// Format a verdict so "allowed" and every distinct denial reason are
    /// separate strings — comparing `is_err()` alone would let a divergence in
    /// *why* a write was refused slip through.
    fn verdict(result: std::result::Result<(), RoleViolation>) -> String {
        match result {
            Ok(()) => "allow".to_string(),
            Err(violation) => format!("deny: {violation}"),
        }
    }

    #[tokio::test]
    async fn tx_read_gate_agrees_with_cache_gate_on_the_whole_matrix() {
        let world = matrix_world();
        let (cache, area_cache) = world.caches();
        let mut compared = 0usize;
        let mut denials = 0usize;
        let mut lookup_failures = 0usize;

        for actor in matrix_actors() {
            for (event_label, event) in matrix_events() {
                for scope in matrix_scopes() {
                    let mut tx_a = world.tx();
                    let from_tx = verdict(
                        enforce_role_resolving_session_from_tx(&mut tx_a, &actor, &event, &scope)
                            .await,
                    );
                    let mut tx_b = world.tx();
                    let from_cache = verdict(
                        enforce_role_resolving_session(
                            &mut tx_b,
                            &actor,
                            &event,
                            &scope,
                            &cache,
                            &area_cache,
                        )
                        .await,
                    );
                    assert_eq!(
                        from_tx, from_cache,
                        "substrate divergence for actor={actor:?} event={event_label} scope={scope:?}"
                    );
                    compared += 1;
                    if from_tx.starts_with("deny") {
                        denials += 1;
                    }
                    if from_tx.contains("role lookup failed") {
                        lookup_failures += 1;
                    }
                }
            }
        }

        // Guard against the matrix silently collapsing to nothing (an empty
        // or all-allow matrix would agree trivially and prove nothing).
        assert_eq!(compared, 21 * 7 * 13, "matrix size changed unexpectedly");
        assert!(
            denials > compared / 4,
            "matrix is too permissive to be evidence: only {denials} of {compared} rows deny"
        );
        // #1381 — the `orphan` card / `w-gone` track rows exist so that both
        // substrates meet an empty track→area lookup. If no row reaches that
        // denial the agreement on it is vacuous.
        assert!(
            lookup_failures > 0,
            "no matrix row reached the track→area lookup miss"
        );
    }
}
