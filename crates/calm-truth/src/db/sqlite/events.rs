use async_trait::async_trait;
use futures::future::BoxFuture;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::Transaction;
use std::collections::HashMap;

use super::SqlxRepo;
use super::begin_immediate_tx;
use crate::card_role_cache::CardRoleCache;
use crate::db::{
    RepoEventWrite, TrackEvent, WriteInTxFn, WriteWithActorEventsFn, WriteWithEventFn,
    WriteWithEventsFn,
};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventBus, EventScope, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, TrackId};
use crate::model::*;
use crate::track_area_cache::TrackAreaCache;
use crate::track_vcs;

/// The gate seam (#1252 S3′).
///
/// [`SqlxRepo::event_append_in_tx`] no longer takes an `(actor, scope, event)`
/// triple — it takes an [`Authorized`](gated::Authorized), and the only way to
/// obtain one is [`authorize`](gated::authorize) / [`authorize_with_caches`]
/// (or, under `cfg(test)`, the loudly-named fixture bypass next to them).
///
/// [`authorize_with_caches`]: gated::authorize_with_caches
///
/// ## What the compiler guarantees, exactly
///
/// **No path can reach this appender without a gate decision on the very
/// triple it inserts.** Three properties combine to give that:
///
///   * every field of `Authorized` is private to `gated`, so no module outside
///     it — including this file's parent module — can literally construct one
///     (E0451);
///   * `Authorized` is only ever returned by a function that has just run the
///     role gate on the triple it carries, so "appended without a gate
///     decision" is not expressible;
///   * those same private fields cannot be reassigned from outside `gated`
///     (E0616), so a capability earned on one triple cannot be pointed at
///     another before the insert. This is the load-bearing half: the
///     *borrows* alone only stop a triple whose values have been dropped
///     (E0716); they do nothing about swapping in another live value.
///
/// ## Residual gap: this is not "the events table cannot be written"
///
/// The guarantee above is about *this appender*, not about the table.
/// `RepoEventWrite::write_in_tx` (`db/mod.rs`) and its public wrappers hand a
/// bare `Transaction<Sqlite>` to callers, and `SqlxRepo::pool` hands out the
/// pool; either can `INSERT INTO events` directly and commit. No production
/// code does today — every raw `events` insert outside this file is inside
/// `tests/` or `#[cfg(test)]` — but nothing here makes that a compile error.
/// Closing that is the job of #1252 S3′ PR-B's textual ratchet, not of this
/// type.
///
/// ## Plumbing, plus one load-bearing arm
///
/// Turning the seam on changed no behaviour: every triple it refuses was
/// already refused, either by the gate elsewhere on the path or by one of the
/// eight manual `enforce_role` calls this slice deleted from the line above
/// the append. But "it refuses nothing" would be false. The
/// codex / claude / terminal create routes can reach the appenders with
/// `ActorId::AiCodex(CardId(""))` — `X-Calm-Actor: ai:codex` survives
/// `validate_header_actor`, becomes that value in `Actor::to_actor_id`, and
/// travels to the adapter in the operation payload untouched — and the gate's
/// empty-`CardId` arm denies it. With those eight guards gone this seam is now
/// that triple's only rejection point. See
/// `append_seam_gate_tests`'s module header for the full actor inventory.
///
/// The rest of the value is forward-looking: the *next* `role_gate` rule
/// applies here without anyone re-auditing fifteen call sites.
mod gated {
    use super::{ActorId, Event, EventScope};
    use crate::error::{CalmError, Result};

    /// Proof that the role gate allowed this one `(actor, scope, event)`
    /// triple.
    ///
    /// Every field is private to this module, which buys two distinct
    /// properties. It is **unconstructible** from the parent module
    /// (`Authorized { .. }` there is E0451), so a capability can only come
    /// from a function in this module that has just run the gate. And it is
    /// **un-retargetable**: `authorized.event = &something_else` in the parent
    /// module is E0616, so the triple that reaches the insert is the same
    /// triple the gate decided on, not merely *a* triple the gate saw.
    ///
    /// The accessors below hand out the borrows read-only. Do not add
    /// setters, `pub` fields, or a `&mut` accessor — retargeting is exactly
    /// what they would restore.
    pub(in crate::db::sqlite::events) struct Authorized<'a> {
        actor: &'a ActorId,
        scope: &'a EventScope,
        event: &'a Event,
    }

    impl<'a> Authorized<'a> {
        pub(in crate::db::sqlite::events) fn actor(&self) -> &'a ActorId {
            self.actor
        }

        pub(in crate::db::sqlite::events) fn scope(&self) -> &'a EventScope {
            self.scope
        }

        pub(in crate::db::sqlite::events) fn event(&self) -> &'a Event {
            self.event
        }
    }

    /// Run the role gate with `card → {role, home track}` and `track → area`
    /// read live from `tx`, and mint the capability on success.
    pub(in crate::db::sqlite::events) async fn authorize<'a, T>(
        tx: &mut T,
        actor: &'a ActorId,
        scope: &'a EventScope,
        event: &'a Event,
    ) -> Result<Authorized<'a>>
    where
        T: crate::decision_gate::WriteTx + ?Sized + Send,
    {
        crate::decision_gate::enforce_role_resolving_session_from_tx(tx, actor, event, scope)
            .await
            .map_err(|violation| CalmError::Forbidden(violation.to_string()))?;
        Ok(Authorized {
            actor,
            scope,
            event,
        })
    }

    /// Run the role gate against the caller's write-through caches, and mint
    /// the capability on success.
    ///
    /// This is the entrance used by the four `RepoEventWrite` wrappers, which
    /// already hold a `WriteContext` and have always gated on its caches. It
    /// exists so those four keep their exact previous behaviour — same
    /// function, same caches — while still being unable to append without a
    /// decision.
    pub(in crate::db::sqlite::events) async fn authorize_with_caches<'a, T>(
        tx: &mut T,
        actor: &'a ActorId,
        scope: &'a EventScope,
        event: &'a Event,
        card_role_cache: &crate::card_role_cache::CardRoleCache,
        track_area_cache: &crate::track_area_cache::TrackAreaCache,
    ) -> Result<Authorized<'a>>
    where
        T: crate::decision_gate::WriteTx + ?Sized + Send,
    {
        crate::decision_gate::enforce_role_resolving_session(
            tx,
            actor,
            event,
            scope,
            card_role_cache,
            track_area_cache,
        )
        .await
        .map_err(|violation| CalmError::Forbidden(violation.to_string()))?;
        Ok(Authorized {
            actor,
            scope,
            event,
        })
    }

    /// **Deliberate bypass, `#[cfg(test)]` only.** Backs
    /// `SqlxRepo::event_append_fixture`, whose whole job is to reconstruct an
    /// event stream verbatim without driving the handler stack. It has been
    /// ungated since it was written; this keeps that unchanged rather than
    /// silently tightening a replay loader. There is no non-test build in
    /// which this function exists.
    #[cfg(test)]
    pub(in crate::db::sqlite::events) fn ungated_fixture_replay<'a>(
        actor: &'a ActorId,
        scope: &'a EventScope,
        event: &'a Event,
    ) -> Authorized<'a> {
        Authorized {
            actor,
            scope,
            event,
        }
    }
}

/// #1252 S3′ negative nail. Records the `kind_tag` of every event that passes
/// through the two public `append_decision_event*_in_tx` entrances, so a test
/// can assert which write paths do — and above all do **not** — flow through
/// this seam.
///
/// Why it exists: #1252's design claimed that once S2 routed fork / template /
/// recipe creation through a unified apply, those events would start flowing
/// through this seam, and asked for a test of that intersection. The
/// intersection does not exist — fork goes through
/// `write_with_actor_events_typed` → `write_with_actor_events`, one of the four
/// `RepoEventWrite` wrappers, which was already gated. A test that asserted the
/// intersection would have been asserting a fiction, so this probe pins the
/// negative instead: it goes red the day a report/fork write starts arriving
/// here.
///
/// A process-global recorder is correct here only because the gate command
/// runs tests with `cargo nextest`, which gives every test its own process.
#[cfg(any(test, feature = "test-helpers"))]
pub mod append_probe {
    use std::sync::Mutex;

    static KINDS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

    pub(super) fn record(kind: &'static str) {
        if let Ok(mut kinds) = KINDS.lock() {
            kinds.push(kind);
        }
    }

    /// Forget everything recorded so far. Call this immediately before the
    /// request under observation.
    pub fn reset() {
        if let Ok(mut kinds) = KINDS.lock() {
            kinds.clear();
        }
    }

    /// Every event kind that reached the seam since the last [`reset`], in
    /// order.
    pub fn kinds() -> Vec<&'static str> {
        KINDS.lock().map(|kinds| kinds.clone()).unwrap_or_default()
    }
}

impl SqlxRepo {
    /// **Private.** The raw events-table insert. Lives off the trait per
    /// design doc §1.4: only `Repo::write_with_event` and
    /// `Repo::log_pure_event` may reach this path, so the commit-then-emit
    /// invariant is unbypassable from the route / plugin host layers.
    ///
    /// Returns the auto-incremented row id, which is then stamped onto
    /// the `BroadcastEnvelope` the wrapper emits on the bus.
    ///
    /// PR2 of #136:
    ///   * `actor` is typed [`ActorId`] and stored as `serde_json::to_string(&actor)`
    ///     in the `events.actor` TEXT column (forward-compatible with future
    ///     actor enrichment).
    ///   * `scope` is decomposed into the four `events.scope_*` columns added
    ///     in migration 0007. `EventScope::System` writes `scope_kind='system'`
    ///     with NULL ancestor cols; the other variants populate whatever
    ///     prefix of the area → track → card chain they carry.
    ///
    /// #1252 S3′: the `(actor, scope, event)` triple arrives as an
    /// [`Authorized`](gated::Authorized) capability rather than as three loose
    /// arguments, so there is no way to reach this insert without a gate
    /// decision on exactly the triple being inserted.
    async fn event_append_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        authorized: &gated::Authorized<'_>,
        correlation: Option<&str>,
    ) -> Result<i64> {
        let actor = authorized.actor();
        let scope = authorized.scope();
        let event = authorized.event();
        let kind = event.kind_tag();
        let payload = event.payload_value();
        let payload_text = serde_json::to_string(&payload)?;
        let actor_text = serde_json::to_string(actor)?;
        let at = now_ms();
        let scope_kind = scope.kind();
        let scope_area = scope.area_id().map(|c| c.as_str());
        let scope_track = scope.track_id().map(|w| w.as_str());
        let scope_card = scope.card_id().map(|c| c.as_str());
        let row = sqlx::query(
            r#"INSERT INTO events (
                   kind, payload, actor, at, correlation, event_version,
                   scope_kind, scope_area, scope_track, scope_card
               )
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
               RETURNING id"#,
        )
        .bind(kind)
        .bind(&payload_text)
        .bind(&actor_text)
        .bind(at)
        .bind(correlation)
        .bind(SYNC_EVENT_VERSION)
        .bind(scope_kind)
        .bind(scope_area)
        .bind(scope_track)
        .bind(scope_card)
        .fetch_one(&mut **tx)
        .await?;
        let id: i64 = row.try_get("id")?;
        Ok(id)
    }

    /// `#[cfg(test)]`-gated raw appender for fixture seeding / replay
    /// loaders. Bypasses the wrapper deliberately so test scaffolds can
    /// reconstruct an event stream verbatim (id-stamped) without driving
    /// the full handler stack.
    #[cfg(test)]
    pub async fn event_append_fixture(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        event: &Event,
    ) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        let authorized = gated::ungated_fixture_replay(&actor, &scope, event);
        let id = Self::event_append_in_tx(&mut tx, &authorized, correlation).await?;
        tx.commit().await?;
        Ok(id)
    }
}

/// Append one event inside the caller's transaction, gated on the live
/// `cards` / `tracks` rows in that same transaction.
///
/// #1252 S3′ removed the `gate: &G` parameter: there is no policy to inject
/// any more. See the [`gated`] module for what replaced it.
pub async fn append_decision_event_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &ActorId,
    scope: &EventScope,
    correlation: Option<&str>,
    event: &Event,
) -> Result<i64> {
    #[cfg(any(test, feature = "test-helpers"))]
    append_probe::record(event.kind_tag());
    let authorized = gated::authorize(tx, actor, scope, event).await?;
    let event_id = SqlxRepo::event_append_in_tx(tx, &authorized, correlation).await?;
    if let Some(track_id) = scope.track_id() {
        track_vcs::commit_in_tx(
            tx,
            track_id,
            actor,
            event_id,
            event,
            track_vcs::MANIFEST_SCHEMA_VERSION,
        )
        .await?;
    }
    Ok(event_id)
}

/// Batch form of [`append_decision_event_in_tx`], same seam, same removal of
/// the injected policy.
///
/// The gate runs on **every** event before **any** of them is inserted. That
/// ordering is what makes "a refused batch writes no events row" a property of
/// this function rather than a property of what the caller does with the
/// transaction afterwards: on the first refusal we return `Err` having issued
/// no `INSERT`, so the claim holds even for a caller that goes on to commit.
/// The interleaved form this replaced left every event before the refused one
/// already inserted in the transaction.
///
/// Splitting the loops is verdict-preserving: the gate reads `cards`,
/// `tracks` and `worker_sessions`, and `event_append_in_tx` writes only
/// `events`, so no append can change the verdict of a later `authorize`.
pub async fn append_decision_events_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &ActorId,
    scope: &EventScope,
    correlation: Option<&str>,
    events: &[Event],
) -> Result<Vec<i64>> {
    let mut authorized_batch = Vec::with_capacity(events.len());
    for event in events {
        #[cfg(any(test, feature = "test-helpers"))]
        append_probe::record(event.kind_tag());
        authorized_batch.push(gated::authorize(tx, actor, scope, event).await?);
    }
    let mut event_ids = Vec::with_capacity(events.len());
    for authorized in &authorized_batch {
        event_ids.push(SqlxRepo::event_append_in_tx(tx, authorized, correlation).await?);
    }
    drop(authorized_batch);
    if let (Some(track_id), Some(event_id)) = (scope.track_id(), event_ids.last()) {
        track_vcs::commit_events_in_tx(
            tx,
            track_id,
            actor,
            *event_id,
            events,
            track_vcs::MANIFEST_SCHEMA_VERSION,
        )
        .await?;
    }
    Ok(event_ids)
}

// ---------------------------------------------------------------------------
// RepoEventWrite — the eventized write path. Every public write that the
// sync engine cares about lands here: `write_with_event` (atomic entity-
// write + event-log), `log_pure_event` (entity-less event log), and the
// `events_*` cursor queries used by replay.
// ---------------------------------------------------------------------------

#[allow(deprecated)]
#[async_trait]
impl RepoEventWrite for SqlxRepo {
    async fn write_with_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &EventBus,
        write: &crate::state::WriteContext,
        f: WriteWithEventFn<'_>,
    ) -> Result<i64> {
        // BEGIN IMMEDIATE takes the writer lock at tx start; deferred SELECT-then-UPDATE upgrades can hit SQLITE_BUSY_SNAPSHOT, which busy_timeout does not cover.
        let mut tx = begin_immediate_tx(&self.pool).await?;
        // Run the caller-supplied entity write.
        let fut: BoxFuture<'_, Result<Event>> = f(&mut tx);
        let event = match fut.await {
            Ok(ev) => ev,
            Err(e) => {
                // Rollback is implicit on `tx` drop, but be explicit so the
                // intent reads clearly.
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        // PR3 (#136) — authorization gate. Runs after the closure
        // produces an event so the closure can mint per-row roles
        // (e.g. `card_create_with_id_tx` writes through the cache)
        // before the gate checks them. Violations roll back: no
        // entity write, no event row, no broadcast.
        let authorized = match gated::authorize_with_caches(
            &mut tx,
            &actor,
            &scope,
            &event,
            write.role_cache(),
            write.area_cache(),
        )
        .await
        {
            Ok(authorized) => authorized,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        // Persist the event in the same txn.
        let event_id = match Self::event_append_in_tx(&mut tx, &authorized, correlation).await {
            Ok(id) => id,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        if let Some(track_id) = scope.track_id()
            && let Err(e) = track_vcs::commit_in_tx(
                &mut tx,
                track_id,
                &actor,
                event_id,
                &event,
                track_vcs::MANIFEST_SCHEMA_VERSION,
            )
            .await
        {
            let _ = tx.rollback().await;
            return Err(e);
        }
        // Commit before any externally-visible side effect.
        tx.commit().await?;
        // Commit-then-emit invariant: now (and only now) do we broadcast.
        bus.emit_envelope(BroadcastEnvelope {
            id: event_id,
            event_version: SYNC_EVENT_VERSION,
            actor,
            scope,
            event,
        });
        Ok(event_id)
    }

    async fn write_with_events(
        &self,
        actor: ActorId,
        correlation: Option<&str>,
        bus: &EventBus,
        write: &crate::state::WriteContext,
        f: WriteWithEventsFn<'_>,
    ) -> Result<Vec<i64>> {
        // BEGIN IMMEDIATE takes the writer lock at tx start; deferred SELECT-then-UPDATE upgrades can hit SQLITE_BUSY_SNAPSHOT, which busy_timeout does not cover.
        let mut tx = begin_immediate_tx(&self.pool).await?;
        // Run the caller-supplied entity write — closure returns one
        // or more (scope, event) pairs for this tx.
        let fut: BoxFuture<'_, Result<Vec<(EventScope, Event)>>> = f(&mut tx);
        let events = match fut.await {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        // Contract: at least one event per tx. An empty vec is a
        // caller bug — refuse to commit so the closure's writes
        // disappear with the rollback.
        if events.is_empty() {
            let _ = tx.rollback().await;
            return Err(CalmError::Internal(
                "write_with_events: closure returned an empty event batch".into(),
            ));
        }
        // PR3 (#136) — authorization gate, per event. The cache is
        // already write-through for any role insert the closure
        // performed, so a track-create-with-planner-card batch can mint
        // the planner card in the closure and immediately have its
        // role visible to the `TrackUpdated` enforce_role call below.
        let mut authorized_batch = Vec::with_capacity(events.len());
        for (scope, event) in &events {
            match gated::authorize_with_caches(
                &mut tx,
                &actor,
                scope,
                event,
                write.role_cache(),
                write.area_cache(),
            )
            .await
            {
                Ok(authorized) => authorized_batch.push(authorized),
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            }
        }
        // Persist every event in the same txn, in order.
        let mut event_ids: Vec<i64> = Vec::with_capacity(events.len());
        for authorized in &authorized_batch {
            match Self::event_append_in_tx(&mut tx, authorized, correlation).await {
                Ok(id) => event_ids.push(id),
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            }
        }
        drop(authorized_batch);
        let mut track_events = HashMap::<TrackId, (i64, Vec<Event>)>::new();
        for ((scope, event), event_id) in events.iter().zip(event_ids.iter()) {
            if let Some(track_id) = scope.track_id() {
                let entry = track_events
                    .entry(track_id.clone())
                    .or_insert_with(|| (*event_id, Vec::new()));
                entry.0 = *event_id;
                entry.1.push(event.clone());
            }
        }
        for (track_id, (event_id, events_for_track)) in &track_events {
            if let Err(e) = track_vcs::commit_events_in_tx(
                &mut tx,
                track_id,
                &actor,
                *event_id,
                events_for_track,
                track_vcs::MANIFEST_SCHEMA_VERSION,
            )
            .await
            {
                let _ = tx.rollback().await;
                return Err(e);
            }
        }
        // Commit before any externally-visible side effect.
        tx.commit().await?;
        // Commit-then-emit invariant: broadcast in the same order the
        // closure produced.
        for (id, (scope, event)) in event_ids.iter().zip(events) {
            bus.emit_envelope(BroadcastEnvelope {
                id: *id,
                event_version: SYNC_EVENT_VERSION,
                actor: actor.clone(),
                scope,
                event,
            });
        }
        Ok(event_ids)
    }

    async fn write_with_actor_events(
        &self,
        correlation: Option<&str>,
        bus: &EventBus,
        write: &crate::state::WriteContext,
        f: WriteWithActorEventsFn<'_>,
    ) -> Result<Vec<i64>> {
        // BEGIN IMMEDIATE takes the writer lock at tx start; deferred SELECT-then-UPDATE upgrades can hit SQLITE_BUSY_SNAPSHOT, which busy_timeout does not cover.
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let fut: BoxFuture<'_, Result<Vec<(ActorId, EventScope, Event)>>> = f(&mut tx);
        let events = match fut.await {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        if events.is_empty() {
            let _ = tx.rollback().await;
            return Err(CalmError::Internal(
                "write_with_actor_events: closure returned an empty event batch".into(),
            ));
        }
        let mut authorized_batch = Vec::with_capacity(events.len());
        for (actor, scope, event) in &events {
            match gated::authorize_with_caches(
                &mut tx,
                actor,
                scope,
                event,
                write.role_cache(),
                write.area_cache(),
            )
            .await
            {
                Ok(authorized) => authorized_batch.push(authorized),
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            }
        }
        let mut event_ids: Vec<i64> = Vec::with_capacity(events.len());
        for authorized in &authorized_batch {
            match Self::event_append_in_tx(&mut tx, authorized, correlation).await {
                Ok(id) => event_ids.push(id),
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            }
        }
        drop(authorized_batch);
        let mut track_events = HashMap::<TrackId, (i64, Option<ActorId>, Vec<Event>)>::new();
        for ((actor, scope, event), event_id) in events.iter().zip(event_ids.iter()) {
            if let Some(track_id) = scope.track_id() {
                let entry = track_events
                    .entry(track_id.clone())
                    .or_insert_with(|| (*event_id, Some(actor.clone()), Vec::new()));
                // Commit author is exact only for a single-actor track batch; mixed actor batches
                // are stored as NULL so the diff renderer leaves them unattributed.
                entry.0 = *event_id;
                if !matches!(&entry.1, Some(existing) if existing == actor) {
                    entry.1 = None;
                }
                entry.2.push(event.clone());
            }
        }
        for (track_id, (event_id, author, events_for_track)) in &track_events {
            if let Err(e) = track_vcs::commit_events_with_author_in_tx(
                &mut tx,
                track_id,
                author.as_ref(),
                *event_id,
                events_for_track,
                track_vcs::MANIFEST_SCHEMA_VERSION,
            )
            .await
            {
                let _ = tx.rollback().await;
                return Err(e);
            }
        }
        tx.commit().await?;
        for (id, (actor, scope, event)) in event_ids.iter().zip(events) {
            bus.emit_envelope(BroadcastEnvelope {
                id: *id,
                event_version: SYNC_EVENT_VERSION,
                actor,
                scope,
                event,
            });
        }
        Ok(event_ids)
    }

    async fn log_pure_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &EventBus,
        card_role_cache: &CardRoleCache,
        track_area_cache: &TrackAreaCache,
        event: Event,
    ) -> Result<i64> {
        // BEGIN IMMEDIATE takes the writer lock at tx start; deferred SELECT-then-UPDATE upgrades can hit SQLITE_BUSY_SNAPSHOT, which busy_timeout does not cover.
        let mut tx = begin_immediate_tx(&self.pool).await?;
        // PR3 (#136) — gate. Pure events don't have an entity write to
        // populate the cache from, so the role lookup uses the cache's
        // current contents. `log_pure_event` callers (codex hook
        // ingest, plugin state transitions) always supply a real actor
        // identity; the gate's defense-in-depth checks (empty
        // CardId, unknown card) still apply.
        let authorized = match gated::authorize_with_caches(
            &mut tx,
            &actor,
            &scope,
            &event,
            card_role_cache,
            track_area_cache,
        )
        .await
        {
            Ok(authorized) => authorized,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        let event_id = match Self::event_append_in_tx(&mut tx, &authorized, correlation).await {
            Ok(id) => id,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        if let Some(track_id) = scope.track_id()
            && let Err(e) = track_vcs::commit_in_tx(
                &mut tx,
                track_id,
                &actor,
                event_id,
                &event,
                track_vcs::MANIFEST_SCHEMA_VERSION,
            )
            .await
        {
            let _ = tx.rollback().await;
            return Err(e);
        }
        tx.commit().await?;
        bus.emit_envelope(BroadcastEnvelope {
            id: event_id,
            event_version: SYNC_EVENT_VERSION,
            actor,
            scope,
            event,
        });
        Ok(event_id)
    }

    /// Issue #310 — event-less tx wrapper. Runs the caller-supplied
    /// closure inside one sqlx transaction; commits on `Ok(())`, rolls
    /// back on `Err(_)`. No event row is appended to the `events` log;
    /// no broadcast is emitted. The caller is responsible for
    /// broadcasting any downstream event via `log_pure_event` after
    /// this returns. See [`crate::db::WriteInTxFn`] for the rationale.
    async fn write_in_tx(&self, f: WriteInTxFn<'_>) -> Result<()> {
        // BEGIN IMMEDIATE takes the writer lock at tx start; deferred SELECT-then-UPDATE upgrades can hit SQLITE_BUSY_SNAPSHOT, which busy_timeout does not cover.
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let fut: BoxFuture<'_, Result<()>> = f(&mut tx);
        match fut.await {
            Ok(()) => {}
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        }
        tx.commit().await?;
        Ok(())
    }

    async fn events_since(
        &self,
        since_id: i64,
        limit: i64,
    ) -> Result<Vec<(i64, u32, EventScope, Event)>> {
        // Clamp so no caller-supplied value can reach sqlite's `LIMIT -1`
        // "no limit" sentinel — the bound is load-bearing (issue #854: a
        // cold WS replay against a 214k-row table pulled the entire log).
        let cap = limit.max(0);
        // `event_version` is selected so the replay path can stamp the
        // envelope with the version persisted on the row, not the current
        // `SYNC_EVENT_VERSION` constant — old rows that predate migration
        // 0006 backfill to `1` via the column default, and any future row
        // written under a newer envelope schema must round-trip its own
        // version, not the kernel's.
        //
        // `scope_*` columns (migration 0007) reconstruct the typed
        // `EventScope`. Rows that predate the migration carry
        // `scope_kind='system'` (column default) with NULL ancestor cols,
        // which `EventScope::from_row` collapses to `EventScope::System`.
        // The same fallback covers any malformed row whose declared
        // `scope_kind` doesn't line up with its ancestor cols — replay
        // never strands a client on a malformed scope.
        type ScopeRow = (
            i64,            // id
            String,         // kind
            String,         // payload
            u32,            // event_version
            Option<String>, // scope_kind
            Option<String>, // scope_area
            Option<String>, // scope_track
            Option<String>, // scope_card
        );
        let rows: Vec<ScopeRow> = sqlx::query_as(
            r#"SELECT id, kind, payload, event_version,
                      scope_kind, scope_area, scope_track, scope_card
               FROM events
               WHERE id > ?1
               ORDER BY id ASC
               LIMIT ?2"#,
        )
        .bind(since_id)
        .bind(cap)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for (id, kind, payload_text, event_version, sk, sc, sw, scard) in rows {
            let payload: serde_json::Value = match serde_json::from_str(&payload_text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_since: skipping row with malformed payload JSON",
                    );
                    continue;
                }
            };
            let scope = EventScope::from_row(
                sk.as_deref(),
                sc.as_deref(),
                sw.as_deref(),
                scard.as_deref(),
            );
            match Event::from_kind_and_payload(&kind, payload) {
                Ok(ev) => out.push((id, event_version, scope, ev)),
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_since: skipping row that no longer matches Event enum",
                    );
                }
            }
        }
        Ok(out)
    }

    async fn events_raw_window_since(
        &self,
        since_id: i64,
        probe_limit: i64,
    ) -> Result<(i64, Option<i64>)> {
        // Same clamp rationale as `events_since`: no caller-supplied value
        // may reach sqlite's `LIMIT -1` "no limit" sentinel. The aggregates
        // are taken over a LIMITed id-only subquery so the probe is bounded
        // by `probe_limit` regardless of table size — this exists so the WS
        // replay cap can be decided on RAW row count (pre-deserialization;
        // see the trait doc for why the filtered `events_since` length is
        // not a safe basis for that decision) and so the caller knows the
        // raw end of the window it is about to read.
        let cap = probe_limit.max(0);
        let (n, max_id): (i64, Option<i64>) = sqlx::query_as(
            r#"SELECT COUNT(*), MAX(id)
               FROM (SELECT id FROM events WHERE id > ?1 ORDER BY id ASC LIMIT ?2)"#,
        )
        .bind(since_id)
        .bind(cap)
        .fetch_one(&self.pool)
        .await?;
        Ok((n, max_id))
    }

    async fn events_for_track(
        &self,
        track_id: &str,
        kinds: &[&str],
        since_id: Option<i64>,
    ) -> Result<Vec<TrackEvent>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }

        type ScopeRow = (
            i64,            // id
            String,         // kind
            String,         // payload
            String,         // actor
            i64,            // at
            Option<String>, // scope_kind
            Option<String>, // scope_area
            Option<String>, // scope_track
            Option<String>, // scope_card
        );

        let mut query = QueryBuilder::<Sqlite>::new(
            r#"SELECT id, kind, payload, actor, at,
                      scope_kind, scope_area, scope_track, scope_card
               FROM events
               WHERE scope_track = "#,
        );
        query.push_bind(track_id);
        if let Some(since_id) = since_id {
            query.push(" AND id > ");
            query.push_bind(since_id);
        }
        query.push(" AND kind IN (");
        let mut separated = query.separated(", ");
        for kind in kinds {
            separated.push_bind(*kind);
        }
        separated.push_unseparated(") ORDER BY id ASC");

        let rows: Vec<ScopeRow> = query.build_query_as().fetch_all(&self.pool).await?;

        let mut out = Vec::with_capacity(rows.len());
        for (id, kind, payload_text, actor_text, at, sk, sc, sw, scard) in rows {
            let payload: serde_json::Value = match serde_json::from_str(&payload_text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_for_track: skipping row with malformed payload JSON",
                    );
                    continue;
                }
            };
            let actor: ActorId = match serde_json::from_str(&actor_text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_for_track: skipping row with malformed actor JSON",
                    );
                    continue;
                }
            };
            let scope = EventScope::from_row(
                sk.as_deref(),
                sc.as_deref(),
                sw.as_deref(),
                scard.as_deref(),
            );
            match Event::from_kind_and_payload(&kind, payload) {
                Ok(event) => out.push(TrackEvent {
                    id,
                    at,
                    actor,
                    scope,
                    event,
                }),
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_for_track: skipping row that no longer matches Event enum",
                    );
                }
            }
        }
        Ok(out)
    }

    async fn events_earliest_id(&self) -> Result<Option<i64>> {
        // `MIN(id)` over an empty table returns a single `NULL` row. Reading
        // the column as `Option<i64>` surfaces that as `None`; non-empty
        // tables return `Some(min)`.
        let row: (Option<i64>,) = sqlx::query_as("SELECT MIN(id) FROM events")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }

    async fn events_prune_watermark(&self) -> Result<i64> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT value FROM retention_meta WHERE key = ?1")
            .bind(crate::events_prune::EVENTS_PRUNE_WATERMARK_KEY)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(v,)| v).unwrap_or(0))
    }

    async fn events_latest_id(&self) -> Result<Option<i64>> {
        // Mirror of `events_earliest_id`: `MAX(id)` over an empty table
        // returns a single `NULL` row, surfaced as `None` here. Used by
        // the WS handler to detect a client cursor that's ahead of the
        // server's actual log tip (see the `events_latest_id` trait
        // docstring for the reset detection contract). Issue #290.
        let row: (Option<i64>,) = sqlx::query_as("SELECT MAX(id) FROM events")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }
}
