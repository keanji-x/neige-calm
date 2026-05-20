//! Storage contract.
//!
//! `Repo` is the interface every persistence backend implements. The kernel
//! is generic over it: REST handlers, terminal lifecycle, plugin host all
//! consume `Arc<dyn Repo>`. The only concrete impl is `SqlxRepo`
//! (sqlite.rs) — used both in production (file-backed sqlite) and in
//! tests/dev (`sqlite::memory:`). A second hand-maintained in-memory
//! `MockRepo` used to live here; it was removed in D3 once tests covered
//! cascade semantics directly — running both impls in lockstep had drifted
//! and become a booby trap (see issue #4).
//!
//! ## Conventions
//!
//! * Methods that "get" a missing row return `Ok(None)`. Methods that
//!   "update/delete" a missing row return `Err(CalmError::NotFound(...))`.
//! * Patch fields that are `None` mean "leave alone".
//! * The repo stamps `created_at` / `updated_at` itself via `model::now_ms()`.
//! * The repo allocates ids via `model::new_id()`.
//! * `sort` defaults to "append to end" (current max + 1.0) when `None`.
//!
//! ## Sync engine write path (phase 1)
//!
//! After Scope A, every mutating handler in `routes/*.rs`,
//! `plugin_host/callbacks.rs`, and the `card_fsm` overlay projector funnels
//! through `Repo::write_with_event`. The wrapper opens a sqlx transaction,
//! runs the caller-supplied closure (which must use the `_tx`-suffixed
//! free functions in `db::sqlite` for any nested entity write), persists
//! the produced `Event` into the `events` table in the same txn, commits,
//! and only then emits a `BroadcastEnvelope { id, event }` on the
//! `EventBus`. Failure of either the entity write or the event insert
//! rolls back the whole transaction — neither row exists, and the bus is
//! never notified. See `docs/sync-engine-design.md` §1.4 and §3.
//!
//! `log_pure_event` is the same shape for events that don't have an
//! associated entity write (e.g. `Event::CodexHook`, `Event::PluginState`).
//! It still goes through the events table and produces a stamped
//! `BroadcastEnvelope`, so every broadcast a client sees has a real id
//! it can use as a cursor.
//!
//! The raw `INSERT INTO events ...` is **private** to `SqlxRepo` (see
//! `sqlite.rs::SqlxRepo::event_append_in_tx`). Exposing two parallel
//! write paths on the trait would invite handlers to drift back to a
//! bare insert and bypass the commit-then-emit guarantee.

use crate::error::Result;
use crate::event::Event;
use crate::model::*;
use async_trait::async_trait;
use futures::future::BoxFuture;
use sqlx::{Sqlite, Transaction};

pub mod sqlite;

/// Closure shape accepted by `Repo::write_with_event`. The closure receives
/// a mutable transaction handle (so it can call the `_tx`-suffixed helpers
/// in `db::sqlite`) and returns the `Event` to persist + broadcast.
///
/// The closure is **not** generic over a returned row type — that would
/// make `Repo` not dyn-compatible, and `Arc<dyn Repo>` is plumbed through
/// every handler and the plugin host. The typed row a handler wants to
/// return to its REST caller is communicated via an outer captured
/// `Arc<Mutex<Option<R>>>` (or similar). The thin `write_with_event_typed`
/// free function below does that capture for ergonomic callers.
///
/// We require `for<'tx>` so the borrow of the transaction doesn't bleed
/// out into the surrounding handler scope — same shape `sqlx::Transaction`
/// itself uses on its associated executor functions.
pub type WriteWithEventFn<'a> = Box<
    dyn for<'tx> FnOnce(&'tx mut Transaction<'_, Sqlite>) -> BoxFuture<'tx, Result<Event>>
        + Send
        + 'a,
>;

#[async_trait]
pub trait Repo: Send + Sync + 'static {
    // ---- coves
    async fn coves_list(&self) -> Result<Vec<Cove>>;
    async fn cove_get(&self, id: &str) -> Result<Option<Cove>>;
    async fn cove_create(&self, p: NewCove) -> Result<Cove>;
    async fn cove_update(&self, id: &str, p: CovePatch) -> Result<Cove>;
    async fn cove_delete(&self, id: &str) -> Result<()>;

    // ---- waves
    async fn waves_by_cove(&self, cove_id: &str) -> Result<Vec<Wave>>;
    async fn wave_get(&self, id: &str) -> Result<Option<Wave>>;
    async fn wave_detail(&self, id: &str) -> Result<Option<WaveDetail>>;
    async fn wave_create(&self, p: NewWave) -> Result<Wave>;
    async fn wave_update(&self, id: &str, p: WavePatch) -> Result<Wave>;
    async fn wave_delete(&self, id: &str) -> Result<()>;

    // ---- cards
    async fn cards_by_wave(&self, wave_id: &str) -> Result<Vec<Card>>;
    async fn card_get(&self, id: &str) -> Result<Option<Card>>;
    async fn card_create(&self, p: NewCard) -> Result<Card>;
    async fn card_update(&self, id: &str, p: CardPatch) -> Result<Card>;
    async fn card_delete(&self, id: &str) -> Result<()>;

    // ---- overlays
    /// Upserts on the `(plugin_id, entity_kind, entity_id, kind)` unique tuple.
    async fn overlay_upsert(&self, p: NewOverlay) -> Result<Overlay>;
    async fn overlay_delete(
        &self,
        plugin_id: &str,
        entity_kind: &str,
        entity_id: &str,
        kind: &str,
    ) -> Result<()>;
    async fn overlays_for(&self, entity_kind: &str, entity_id: &str) -> Result<Vec<Overlay>>;
    /// List every overlay attached to entities of the given `entity_kind`
    /// (e.g. `"wave"`), regardless of `entity_id`. Used by the sidebar so
    /// wave status indicators stay accurate without per-wave detail fetches.
    async fn overlays_by_kind(&self, entity_kind: &str) -> Result<Vec<Overlay>>;

    // ---- terminals
    async fn terminal_create(&self, p: NewTerminal) -> Result<Terminal>;
    async fn terminal_get(&self, id: &str) -> Result<Option<Terminal>>;
    async fn terminal_get_by_card(&self, card_id: &str) -> Result<Option<Terminal>>;
    async fn terminal_set_handle(&self, id: &str, handle: Option<&str>) -> Result<()>;
    /// Persist the daemon PID captured by `routes::terminal::spawn_daemon_for`
    /// (and the WS-side revive path). The orphan-terminal sweeper uses this
    /// as a SIGTERM fallback target when graceful `ClientMsg::Kill` fails.
    async fn terminal_set_pid(&self, id: &str, pid: Option<u32>) -> Result<()>;
    /// Return every terminal row that has no card pointing at it via
    /// `cards.payload.terminal_id`, and whose `created_at` is older than
    /// `grace_seconds` ago. The grace window absorbs the 3-step
    /// terminal-card create race (see `web/src/app/eventBridge.tsx:60-70`).
    /// Used exclusively by the `terminal_sweeper` background task.
    async fn terminals_orphaned(&self, grace_seconds: i64) -> Result<Vec<Terminal>>;
    /// Remove a terminal row by id. Sibling to `card_delete` etc.; surfaced
    /// on the trait so the sweeper can call it from inside its
    /// `write_with_event` closure via the `_tx`-suffixed helper.
    async fn terminal_delete(&self, id: &str) -> Result<()>;

    // ---- plugins
    //
    // M3 (Slice A) surface: install / enable / get-by-id / delete / list-all.
    // `plugins_list` is kept as a thin alias around `plugins_list_all` so
    // Slice D's REST handler (and the existing stub) can keep calling it.
    async fn plugins_list(&self) -> Result<Vec<Plugin>>;
    async fn plugins_list_all(&self) -> Result<Vec<Plugin>>;
    async fn plugin_get_by_id(&self, id: &str) -> Result<Option<Plugin>>;
    /// Upsert by id. The repo stamps `installed_at` (preserving the existing
    /// value on update) and `updated_at`. `enabled` defaults to false on the
    /// install row — the user (or Slice D's enable endpoint) flips it later.
    async fn plugin_install(&self, p: NewPlugin) -> Result<Plugin>;
    async fn plugin_update_enabled(&self, id: &str, enabled: bool) -> Result<Plugin>;
    /// Overwrite `user_config` (the opaque JSON blob the PATCH config route
    /// writes). The repo stamps `updated_at`; everything else is preserved.
    async fn plugin_update_user_config(
        &self,
        id: &str,
        user_config: serde_json::Value,
    ) -> Result<Plugin>;
    /// Overwrite the persisted manifest blob. The reload route calls this
    /// after re-reading manifest.json from disk so subsequent `GET
    /// /api/plugins/:id` responses (which read from the DB row, not the
    /// live registry) reflect on-disk reality.
    async fn plugin_update_manifest(&self, id: &str, manifest: serde_json::Value)
    -> Result<Plugin>;
    async fn plugin_delete(&self, id: &str) -> Result<()>;

    /// Drop every overlay owned by a plugin. Slice D's uninstall route fires
    /// this so a deleted plugin's overlays don't render as ghosts. (Design
    /// doc §2.7 calls this out as the default; the alternative — "keep for
    /// forensics" — is what users have to opt into manually.)
    async fn overlays_clear_by_plugin(&self, plugin_id: &str) -> Result<()>;

    /// Drop every KV row owned by a plugin. Called from the uninstall path so
    /// per-plugin KV doesn't outlive the install row.
    async fn plugin_kv_clear(&self, plugin_id: &str) -> Result<()>;

    // ---- per-plugin tokens (Slice H wires the lifecycle; Slice A just owns
    // the storage). Hash is hex-encoded `SHA-256(raw_token)`; expires_at is
    // unix millis (matches the rest of the kernel's `*_at` columns).
    async fn plugin_token_set(
        &self,
        plugin_id: &str,
        hashed_token: &str,
        expires_at: i64,
    ) -> Result<()>;
    async fn plugin_token_get(&self, plugin_id: &str) -> Result<Option<(String, i64)>>;
    async fn plugin_token_delete(&self, plugin_id: &str) -> Result<()>;

    // ---- per-plugin KV store (Slice C will surface to plugins via
    // `neige.kv.*`; Slice A owns the bare CRUD). Values are arbitrary JSON;
    // the kernel does not parse semantics, but it does enforce per-plugin
    // namespacing at this trait layer (no method takes a global key).
    async fn plugin_kv_get(&self, plugin_id: &str, key: &str) -> Result<Option<serde_json::Value>>;
    async fn plugin_kv_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()>;
    async fn plugin_kv_list(
        &self,
        plugin_id: &str,
        prefix: &str,
    ) -> Result<Vec<(String, serde_json::Value)>>;
    async fn plugin_kv_delete(&self, plugin_id: &str, key: &str) -> Result<()>;

    // ---- app-global settings (Settings page, codex spawn proxy override).
    //
    // Tiny KV. `settings_get_all` returns every key/value pair the kernel
    // owns; the Settings route just hands it back, and `routes::codex`
    // reads the snapshot at spawn time to derive HTTP_PROXY env overrides.
    // `settings_upsert` is per-key INSERT OR REPLACE; an empty string is
    // treated as a delete on the *route* boundary (callers can still
    // upsert an empty value if they have a reason to).
    async fn settings_get_all(&self) -> Result<Vec<(String, String)>>;
    async fn settings_upsert(&self, key: &str, value: &str) -> Result<()>;
    async fn settings_delete(&self, key: &str) -> Result<()>;

    // ---- sync engine (phase 1) ------------------------------------------
    //
    // `write_with_event` is the *only* public path that writes events to
    // the kernel's persistent log. The raw `INSERT INTO events` lives
    // privately on `SqlxRepo`; see `sqlite::SqlxRepo::event_append_in_tx`.
    //
    // Two reasons to keep the raw form off the trait:
    //
    //  1. Two parallel paths invite handlers to drift back to the raw
    //     form, bypassing the `write_with_event` transaction guarantee.
    //  2. Loosening the surface later (if a real use case shows up) is
    //     trivial; tightening it after callers have spread is hard.
    //
    // `log_pure_event` is the sibling for events that don't have an
    // associated entity write — `Event::PluginState`,
    // `Event::CodexHook`. It runs its own minimal transaction (just the
    // events insert) so every event still has a real `events.id` to
    // stamp on the wire envelope.

    /// Atomic write + event-log invariant: run the closure inside one
    /// sqlx transaction, then `INSERT INTO events ... RETURNING id` in
    /// the same txn, commit, and emit `BroadcastEnvelope { id, event }`
    /// on the supplied event bus.
    ///
    /// Error semantics:
    ///   * Closure returns `Err(e)`: txn rolls back, `Err(e)` bubbles up,
    ///     no entity row, no event row, no broadcast.
    ///   * Events-insert fails (DB-level): txn rolls back, error bubbles
    ///     up, same as above.
    ///   * Commit fails: error bubbles up, no broadcast.
    ///   * Commit succeeds, broadcast send returns zero subscribers: the
    ///     event is persisted and visible to replay; current live clients
    ///     see nothing, but that's fine (they have no live socket).
    ///
    /// `actor` is the declared identity of the producer
    /// (`"user"`, `"kernel"`, `"plugin:<id>"`, `"ai:<id>"`). Not
    /// authenticated — see design doc §1.1 disclaimer.
    ///
    /// `correlation` is optional; populated for plugin tool-call writes
    /// per design §9 (`"user_tool_call:<call_id>"`).
    async fn write_with_event(
        &self,
        actor: &str,
        correlation: Option<&str>,
        bus: &crate::event::EventBus,
        f: WriteWithEventFn<'_>,
    ) -> Result<i64>;

    /// Persist + broadcast a pure event (no associated entity write). Same
    /// commit-then-emit invariant as `write_with_event`, but no transaction
    /// closure — the event itself is the only write.
    ///
    /// Used for `Event::CodexHook` (ingest at
    /// `routes::codex::ingest_hook`) and `Event::PluginState` (plugin
    /// supervisor lifecycle in `plugin_host::PluginHost::emit_state`).
    /// Returns the assigned `events.id`.
    async fn log_pure_event(
        &self,
        actor: &str,
        correlation: Option<&str>,
        bus: &crate::event::EventBus,
        event: Event,
    ) -> Result<i64>;

    /// Scope D — replay query. Read events with `id > since_id` from the
    /// persistent log, deserialize each `(kind, payload)` row back into a
    /// typed `Event`, and return them in ascending id order.
    ///
    /// Pairs with the WS `since` protocol (see `ws::events::handle`): the
    /// handler calls this to stream historical frames to a reconnecting
    /// client, then transitions to live broadcast. The cursor protocol
    /// relies on the strict-monotonic `events.id` to dedupe replay-vs-live
    /// at the boundary (design §2.2).
    ///
    /// `limit = None` returns every row above `since_id`; `Some(n)`
    /// truncates at the first `n` rows in id order (used by chunked
    /// pagination if a future tuning splits very large historical windows).
    ///
    /// Rows whose payload fails to deserialize back into an `Event` variant
    /// are logged + skipped, not propagated as an error — corrupt history
    /// shouldn't strand otherwise-connected clients.
    async fn events_since(&self, since_id: i64, limit: Option<i64>) -> Result<Vec<(i64, Event)>>;

    /// Lowest live `events.id`, or `None` if the table is empty.
    ///
    /// Used by the WS handler to detect a `since` cursor that predates the
    /// retention horizon (after the operator turns on
    /// `events_retention_days`). When `since < earliest_id`, the server
    /// can't honor the replay; it must reply with a `_snapshot_required`
    /// control frame so the client throws away its cached state and
    /// refetches via REST (design §2.3).
    async fn events_earliest_id(&self) -> Result<Option<i64>>;
}

// ---------------------------------------------------------------------------
// `write_with_event_typed` — ergonomic generic wrapper around the
// dyn-compatible trait method.
// ---------------------------------------------------------------------------

/// Generic convenience wrapper over `Repo::write_with_event` for callers
/// who want to return a typed row to their REST / plugin-host caller. The
/// closure returns `(R, Event)`; we capture `R` in an outer mutex so the
/// trait method's `WriteWithEventFn` (which only knows about `Event`) can
/// stay dyn-compatible.
///
/// This is *purely sugar* — the underlying invariants (single transaction,
/// commit-then-emit) come from the trait method.
pub async fn write_with_event_typed<R, F>(
    repo: &dyn Repo,
    actor: &str,
    correlation: Option<&str>,
    bus: &crate::event::EventBus,
    f: F,
) -> Result<(R, i64)>
where
    R: Send + 'static,
    F: for<'tx> FnOnce(&'tx mut Transaction<'_, Sqlite>) -> BoxFuture<'tx, Result<(R, Event)>>
        + Send
        + 'static,
{
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let captured: Arc<Mutex<Option<R>>> = Arc::new(Mutex::new(None));
    let captured_inner = Arc::clone(&captured);

    let boxed: WriteWithEventFn<'_> = Box::new(move |tx| {
        let captured_inner = Arc::clone(&captured_inner);
        Box::pin(async move {
            let (row, event) = f(tx).await?;
            *captured_inner.lock().await = Some(row);
            Ok(event)
        })
    });

    let event_id = repo
        .write_with_event(actor, correlation, bus, boxed)
        .await?;
    let row = Arc::try_unwrap(captured)
        .map_err(|_| {
            crate::error::CalmError::Internal(
                "write_with_event_typed: outstanding reference to captured row".into(),
            )
        })?
        .into_inner()
        .ok_or_else(|| {
            crate::error::CalmError::Internal(
                "write_with_event_typed: closure did not set row".into(),
            )
        })?;
    Ok((row, event_id))
}
