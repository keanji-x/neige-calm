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

/// The gate seam: `event_append_in_tx` takes an [`Authorized`](gated::Authorized) capability whose fields are private
/// to `gated`, so in safe code no path reaches the appender without a gate decision on the very triple it inserts and
/// no earned capability can be retargeted. It does not bind the transaction: mint and append must share one `tx` by
/// construction. It is also not "the events table cannot be written": `write_in_tx` and the pool can still insert raw.
mod gated {
    use super::{ActorId, Event, EventScope};
    use crate::error::{CalmError, Result};

    /// Proof that the role gate allowed this one `(actor, scope, event)` triple. Every field is private so it is
    /// unconstructible (E0451) and un-retargetable (E0616) from outside; do not add setters, `pub` fields, or a `&mut` accessor.
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

    /// Run the role gate with `card → {role, home track}` and `track → area` read live from `tx`, and mint the capability on success.
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

    /// Run the role gate against the caller's write-through caches (the `RepoEventWrite` wrappers' entrance), and mint the capability on success.
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

    /// **Deliberate bypass, `#[cfg(test)]` only.** Backs `SqlxRepo::event_append_fixture`, which reconstructs an event stream verbatim without driving the handler stack.
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

/// The crate-internal escape probe: each feature compiles one bypass whose only job is to **fail to compile** with an
/// exact diagnostic, so `calm-truth` can never be built with `--all-features`. It must live inside the crate (a
/// descendant of `events`) because an external `trybuild` crate could not even name `Authorized` and would fail vacuously.
#[cfg(any(
    feature = "append-seam-escape-probe-retarget",
    feature = "append-seam-escape-probe-forge",
    feature = "append-seam-escape-probe-functional-update",
    feature = "append-seam-escape-probe-ungated-append",
))]
mod append_seam_escape_probe {
    #[allow(unused_imports)]
    use super::gated;
    #[allow(unused_imports)]
    use super::{ActorId, Event, EventScope, SqlxRepo};
    #[allow(unused_imports)]
    use crate::error::Result;
    #[allow(unused_imports)]
    use sqlx::{Sqlite, Transaction};

    /// P1 — retarget an earned capability at a different event. Must be **E0616**.
    #[cfg(feature = "append-seam-escape-probe-retarget")]
    pub(super) fn retarget<'a>(authorized: &mut gated::Authorized<'a>, other: &'a Event) {
        authorized.event = other;
    }

    /// P2 — forge a capability by literal construction. Must be **E0451**.
    #[cfg(feature = "append-seam-escape-probe-forge")]
    pub(super) fn forge<'a>(
        actor: &'a ActorId,
        scope: &'a EventScope,
        event: &'a Event,
    ) -> gated::Authorized<'a> {
        gated::Authorized {
            actor,
            scope,
            event,
        }
    }

    /// P3 — the functional-update spelling of P2. Must be **E0451**.
    #[cfg(feature = "append-seam-escape-probe-functional-update")]
    pub(super) fn functional_update<'a>(
        authorized: gated::Authorized<'a>,
        other: &'a Event,
    ) -> gated::Authorized<'a> {
        gated::Authorized {
            event: other,
            ..authorized
        }
    }

    /// P4 — reach the appender with a loose `(actor, scope, event)` triple. Must be **E0061** (wrong number of arguments).
    #[cfg(feature = "append-seam-escape-probe-ungated-append")]
    pub(super) async fn append_without_authorize(
        tx: &mut Transaction<'_, Sqlite>,
        actor: &ActorId,
        scope: &EventScope,
        event: &Event,
    ) -> Result<i64> {
        SqlxRepo::event_append_in_tx(tx, actor, scope, event, None).await
    }
}

/// Records the `kind_tag` of every event that passes through the two public `append_decision_event*_in_tx` entrances,
/// so a test can assert which write paths do not flow through this seam. A process-global recorder is correct only
/// because tests run under `cargo nextest`, one process per test.
#[cfg(any(test, feature = "test-helpers"))]
pub mod append_probe {
    use std::sync::Mutex;

    static KINDS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

    pub(super) fn record(kind: &'static str) {
        if let Ok(mut kinds) = KINDS.lock() {
            kinds.push(kind);
        }
    }

    /// Forget everything recorded so far.
    pub fn reset() {
        if let Ok(mut kinds) = KINDS.lock() {
            kinds.clear();
        }
    }

    /// Every event kind that reached the seam since the last [`reset`], in order.
    pub fn kinds() -> Vec<&'static str> {
        KINDS.lock().map(|kinds| kinds.clone()).unwrap_or_default()
    }
}

impl SqlxRepo {
    /// **Private.** The raw events-table insert; only the eventized wrappers reach it, so commit-then-emit is unbypassable
    /// from the route / plugin host layers. `actor` is stored as JSON; `scope` is decomposed into the `events.scope_*` columns.
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

    /// `#[cfg(test)]`-gated raw appender for fixture seeding / replay loaders; bypasses the wrapper deliberately.
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

/// Append one event inside the caller's transaction, gated on the live `cards` / `tracks` rows in that same transaction.
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

/// Batch form of [`append_decision_event_in_tx`]. The gate runs on **every** event before **any** is inserted, so a
/// refused batch writes no events row even for a caller that goes on to commit; splitting the loops is verdict-preserving
/// because the gate reads `cards`/`tracks`/`worker_sessions` and the append writes only `events`.
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
        let fut: BoxFuture<'_, Result<Event>> = f(&mut tx);
        let event = match fut.await {
            Ok(ev) => ev,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        // The gate runs after the closure produces an event so the closure can mint per-row roles through the cache first.
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
        // Commit-then-emit invariant: broadcast only after commit.
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
        let fut: BoxFuture<'_, Result<Vec<(EventScope, Event)>>> = f(&mut tx);
        let events = match fut.await {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        // At least one event per tx; an empty vec is a caller bug, so the closure's writes disappear with the rollback.
        if events.is_empty() {
            let _ = tx.rollback().await;
            return Err(CalmError::Internal(
                "write_with_events: closure returned an empty event batch".into(),
            ));
        }
        // Per-event gate; the cache is already write-through for any role insert the closure performed.
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
        tx.commit().await?;
        // Commit-then-emit invariant: broadcast in the order the closure produced.
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
        // Pure events have no entity write to populate the cache from, so the role lookup uses the cache's current contents.
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

    /// Event-less tx wrapper: commits on `Ok(())`, rolls back on `Err(_)`; no event row, no broadcast.
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
        // Clamp so no caller-supplied value can reach sqlite's `LIMIT -1` "no limit" sentinel.
        let cap = limit.max(0);
        // `event_version` is selected so replay stamps the version persisted on the row, not the current constant.
        // Rows predating the scope columns (or with a malformed `scope_kind`) collapse to `EventScope::System`, so replay never strands a client.
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
        // Same clamp as `events_since`; the aggregates are taken over a LIMITed id-only subquery so the probe is bounded regardless of table size.
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
        // `MIN(id)` over an empty table returns a single `NULL` row, read as `None`.
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
        // `MAX(id)` over an empty table returns a single `NULL` row, read as `None`.
        let row: (Option<i64>,) = sqlx::query_as("SELECT MAX(id) FROM events")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }
}
