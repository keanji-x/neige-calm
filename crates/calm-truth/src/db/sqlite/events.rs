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
    RepoEventWrite, WaveEvent, WriteInTxFn, WriteWithActorEventsFn, WriteWithEventFn,
    WriteWithEventsFn,
};
use crate::decision_gate::DecisionGate;
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventBus, EventScope, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, WaveId};
use crate::model::*;
use crate::wave_cove_cache::WaveCoveCache;
use crate::wave_vcs;

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
    ///     prefix of the cove → wave → card chain they carry.
    async fn event_append_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        actor: &ActorId,
        scope: &EventScope,
        correlation: Option<&str>,
        event: &Event,
    ) -> Result<i64> {
        let kind = event.kind_tag();
        let payload = event.payload_value();
        let payload_text = serde_json::to_string(&payload)?;
        let actor_text = serde_json::to_string(actor)?;
        let at = now_ms();
        let scope_kind = scope.kind();
        let scope_cove = scope.cove_id().map(|c| c.as_str());
        let scope_wave = scope.wave_id().map(|w| w.as_str());
        let scope_card = scope.card_id().map(|c| c.as_str());
        let row = sqlx::query(
            r#"INSERT INTO events (
                   kind, payload, actor, at, correlation, event_version,
                   scope_kind, scope_cove, scope_wave, scope_card
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
        .bind(scope_cove)
        .bind(scope_wave)
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
        let id = Self::event_append_in_tx(&mut tx, &actor, &scope, correlation, event).await?;
        tx.commit().await?;
        Ok(id)
    }
}

pub async fn append_decision_event_in_tx<G: DecisionGate + ?Sized>(
    tx: &mut Transaction<'_, Sqlite>,
    gate: &G,
    actor: &ActorId,
    scope: &EventScope,
    correlation: Option<&str>,
    event: &Event,
) -> Result<i64> {
    gate.decide(tx, actor, scope, event).await?.into_result()?;
    let event_id = SqlxRepo::event_append_in_tx(tx, actor, scope, correlation, event).await?;
    if let Some(wave_id) = scope.wave_id() {
        wave_vcs::commit_in_tx(
            tx,
            wave_id,
            actor,
            event_id,
            event,
            wave_vcs::MANIFEST_SCHEMA_VERSION,
        )
        .await?;
    }
    Ok(event_id)
}

pub async fn append_decision_events_in_tx<G: DecisionGate + ?Sized>(
    tx: &mut Transaction<'_, Sqlite>,
    gate: &G,
    actor: &ActorId,
    scope: &EventScope,
    correlation: Option<&str>,
    events: &[Event],
) -> Result<Vec<i64>> {
    let mut event_ids = Vec::with_capacity(events.len());
    for event in events {
        gate.decide(tx, actor, scope, event).await?.into_result()?;
        event_ids.push(SqlxRepo::event_append_in_tx(tx, actor, scope, correlation, event).await?);
    }
    if let (Some(wave_id), Some(event_id)) = (scope.wave_id(), event_ids.last()) {
        wave_vcs::commit_events_in_tx(
            tx,
            wave_id,
            actor,
            *event_id,
            events,
            wave_vcs::MANIFEST_SCHEMA_VERSION,
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
        if let Err(violation) = crate::decision_gate::enforce_role_resolving_session(
            &mut tx,
            &actor,
            &event,
            &scope,
            write.role_cache(),
            write.cove_cache(),
        )
        .await
        {
            let _ = tx.rollback().await;
            return Err(CalmError::Forbidden(violation.to_string()));
        }
        // Persist the event in the same txn.
        let event_id =
            match Self::event_append_in_tx(&mut tx, &actor, &scope, correlation, &event).await {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            };
        if let Some(wave_id) = scope.wave_id()
            && let Err(e) = wave_vcs::commit_in_tx(
                &mut tx,
                wave_id,
                &actor,
                event_id,
                &event,
                wave_vcs::MANIFEST_SCHEMA_VERSION,
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
        // performed, so a wave-create-with-spec-card batch can mint
        // the spec card in the closure and immediately have its
        // role visible to the `WaveUpdated` enforce_role call below.
        for (scope, event) in &events {
            if let Err(violation) = crate::decision_gate::enforce_role_resolving_session(
                &mut tx,
                &actor,
                event,
                scope,
                write.role_cache(),
                write.cove_cache(),
            )
            .await
            {
                let _ = tx.rollback().await;
                return Err(CalmError::Forbidden(violation.to_string()));
            }
        }
        // Persist every event in the same txn, in order.
        let mut event_ids: Vec<i64> = Vec::with_capacity(events.len());
        for (scope, event) in &events {
            match Self::event_append_in_tx(&mut tx, &actor, scope, correlation, event).await {
                Ok(id) => event_ids.push(id),
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            }
        }
        let mut wave_events = HashMap::<WaveId, (i64, Vec<Event>)>::new();
        for ((scope, event), event_id) in events.iter().zip(event_ids.iter()) {
            if let Some(wave_id) = scope.wave_id() {
                let entry = wave_events
                    .entry(wave_id.clone())
                    .or_insert_with(|| (*event_id, Vec::new()));
                entry.0 = *event_id;
                entry.1.push(event.clone());
            }
        }
        for (wave_id, (event_id, events_for_wave)) in &wave_events {
            if let Err(e) = wave_vcs::commit_events_in_tx(
                &mut tx,
                wave_id,
                &actor,
                *event_id,
                events_for_wave,
                wave_vcs::MANIFEST_SCHEMA_VERSION,
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
        for (actor, scope, event) in &events {
            if let Err(violation) = crate::decision_gate::enforce_role_resolving_session(
                &mut tx,
                actor,
                event,
                scope,
                write.role_cache(),
                write.cove_cache(),
            )
            .await
            {
                let _ = tx.rollback().await;
                return Err(CalmError::Forbidden(violation.to_string()));
            }
        }
        let mut event_ids: Vec<i64> = Vec::with_capacity(events.len());
        for (actor, scope, event) in &events {
            match Self::event_append_in_tx(&mut tx, actor, scope, correlation, event).await {
                Ok(id) => event_ids.push(id),
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            }
        }
        let mut wave_events = HashMap::<WaveId, (i64, Option<ActorId>, Vec<Event>)>::new();
        for ((actor, scope, event), event_id) in events.iter().zip(event_ids.iter()) {
            if let Some(wave_id) = scope.wave_id() {
                let entry = wave_events
                    .entry(wave_id.clone())
                    .or_insert_with(|| (*event_id, Some(actor.clone()), Vec::new()));
                // Commit author is exact only for a single-actor wave batch; mixed actor batches
                // are stored as NULL so the diff renderer leaves them unattributed.
                entry.0 = *event_id;
                if !matches!(&entry.1, Some(existing) if existing == actor) {
                    entry.1 = None;
                }
                entry.2.push(event.clone());
            }
        }
        for (wave_id, (event_id, author, events_for_wave)) in &wave_events {
            if let Err(e) = wave_vcs::commit_events_with_author_in_tx(
                &mut tx,
                wave_id,
                author.as_ref(),
                *event_id,
                events_for_wave,
                wave_vcs::MANIFEST_SCHEMA_VERSION,
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
        wave_cove_cache: &WaveCoveCache,
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
        if let Err(violation) = crate::decision_gate::enforce_role_resolving_session(
            &mut tx,
            &actor,
            &event,
            &scope,
            card_role_cache,
            wave_cove_cache,
        )
        .await
        {
            let _ = tx.rollback().await;
            return Err(CalmError::Forbidden(violation.to_string()));
        }
        let event_id =
            match Self::event_append_in_tx(&mut tx, &actor, &scope, correlation, &event).await {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            };
        if let Some(wave_id) = scope.wave_id()
            && let Err(e) = wave_vcs::commit_in_tx(
                &mut tx,
                wave_id,
                &actor,
                event_id,
                &event,
                wave_vcs::MANIFEST_SCHEMA_VERSION,
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
            Option<String>, // scope_cove
            Option<String>, // scope_wave
            Option<String>, // scope_card
        );
        let rows: Vec<ScopeRow> = sqlx::query_as(
            r#"SELECT id, kind, payload, event_version,
                      scope_kind, scope_cove, scope_wave, scope_card
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

    async fn events_for_wave(
        &self,
        wave_id: &str,
        kinds: &[&str],
        since_id: Option<i64>,
    ) -> Result<Vec<WaveEvent>> {
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
            Option<String>, // scope_cove
            Option<String>, // scope_wave
            Option<String>, // scope_card
        );

        let mut query = QueryBuilder::<Sqlite>::new(
            r#"SELECT id, kind, payload, actor, at,
                      scope_kind, scope_cove, scope_wave, scope_card
               FROM events
               WHERE scope_wave = "#,
        );
        query.push_bind(wave_id);
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
                        "events_for_wave: skipping row with malformed payload JSON",
                    );
                    continue;
                }
            };
            let actor: ActorId = match serde_json::from_str(&actor_text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_for_wave: skipping row with malformed actor JSON",
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
                Ok(event) => out.push(WaveEvent {
                    id,
                    at,
                    actor,
                    scope,
                    event,
                }),
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_for_wave: skipping row that no longer matches Event enum",
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
