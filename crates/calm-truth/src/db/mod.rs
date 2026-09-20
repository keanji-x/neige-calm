//! Storage contract: `Repo` is the interface every persistence backend implements; `SqlxRepo` is the only impl.
//! Split by capability so that no route handler can reach a raw sync-domain write at compile time: `RouteRepo` (what
//! handlers see) excludes `RepoSyncDomainRaw`; `AppState::raw_repo()` is the deliberate step outside the gate.
//! Conventions: "get" of a missing row is `Ok(None)`, "update/delete" is `Err(NotFound)`; patch fields `None` mean leave alone.

use crate::card_role_cache::CardRoleCache;
use crate::error::Result;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, AreaId, CardId, TrackId};
use crate::model::*;
use crate::session_projection_repo::WorkerSessionProjectionRepo;
use crate::session_repo::SessionRepo;
use crate::state::WriteContext;
use crate::track_area_cache::TrackAreaCache;
use async_trait::async_trait;
use calm_types::claude_permissions::ClaudePermissionsScope;
use calm_types::worker::{WorkerSession, WorkerSessionId};
use futures::future::BoxFuture;
use sqlx::{Sqlite, SqlitePool, Transaction};
use std::sync::Arc;

pub mod rows;
pub mod sqlite;

/// Closure shape accepted by `Repo::write_with_event`; returns the `Event` to persist + broadcast. Not generic over a
/// returned row (that would break dyn-compatibility) — `write_with_event_typed` captures the typed row for callers.
pub type WriteWithEventFn<'a> = Box<
    dyn for<'tx> FnOnce(&'tx mut Transaction<'_, Sqlite>) -> BoxFuture<'tx, Result<Event>>
        + Send
        + 'a,
>;

/// Plural counterpart to [`WriteWithEventFn`]: one transaction persists multiple events, each with its own scope.
/// Must return a non-empty vec; any single `RoleViolation` rolls the entire batch back; events persist and broadcast in vec order.
pub type WriteWithEventsFn<'a> = Box<
    dyn for<'tx> FnOnce(
            &'tx mut Transaction<'_, Sqlite>,
        ) -> BoxFuture<'tx, Result<Vec<(EventScope, Event)>>>
        + Send
        + 'a,
>;

/// Like [`WriteWithEventsFn`], but each event carries its own actor, for a kernel-auto lifecycle event committing atomically with the write that triggered it.
pub type WriteWithActorEventsFn<'a> = Box<
    dyn for<'tx> FnOnce(
            &'tx mut Transaction<'_, Sqlite>,
        ) -> BoxFuture<'tx, Result<Vec<(ActorId, EventScope, Event)>>>
        + Send
        + 'a,
>;

/// Event-less counterpart to [`WriteWithEventFn`]: no event row, no broadcast. Used by the dispatcher's two-stage
/// worker spawn. Crash-window hazard: if the kernel dies between this commit and the post-spawn `log_pure_event(CardAdded)`,
/// the card row exists but replay never surfaces it and the sweeper won't reap it while its session is active.
pub type WriteInTxFn<'a> = Box<
    dyn for<'tx> FnOnce(&'tx mut Transaction<'_, Sqlite>) -> BoxFuture<'tx, Result<()>> + Send + 'a,
>;

#[derive(Clone, Debug)]
pub struct TrackEvent {
    pub id: i64,
    pub at: i64,
    pub actor: ActorId,
    pub scope: EventScope,
    pub event: Event,
}

#[derive(Debug, Clone)]
pub struct SharedCodexDaemonRecord {
    pub state: String,
    pub pid: Option<i32>,
    pub pgid: Option<i32>,
    pub sock_path: Option<String>,
    pub codex_home_path: Option<String>,
    pub process_start_time: Option<u64>,
    pub boot_id: Option<String>,
    pub started_at: Option<i64>,
    pub updated_at: i64,
    pub restart_count: i64,
    pub last_error: Option<String>,
    pub daemon_env_signature: Option<String>,
}

/// Internal MCP auth identity recovered from `cards.session_id`; narrower than [`Card`] because `session_id` is not part of the public wire model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCardIdentity {
    pub card_id: CardId,
    pub role: CardRole,
    pub track_id: TrackId,
    pub area_id: AreaId,
}

#[derive(Debug, Clone)]
pub struct SharedCodexDaemonUpdate {
    pub state: String,
    pub pid: Option<i32>,
    pub pgid: Option<i32>,
    pub sock_path: Option<String>,
    pub codex_home_path: Option<String>,
    pub process_start_time: Option<u64>,
    pub boot_id: Option<String>,
    pub started_at: Option<i64>,
    pub last_error: Option<String>,
    pub increment_restart_count: bool,
    pub daemon_env_signature: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceLease {
    pub lease_id: String,
    pub card_id: String,
    pub track_id: String,
    pub path: String,
    pub state: String,
}

// Each sub-trait carries `Send + Sync + 'static` and is dyn-compatible; `dyn RouteRepo` upcasts to its supertraits via the merged vtable.

/// Universal read surface; no writes are reachable from here.
#[async_trait]
pub trait RepoRead: Send + Sync + 'static {
    /// Every area regardless of [`AreaKind`]; the user-facing route prefers [`RepoRead::areas_list_user_visible`] so the system area stays hidden.
    async fn areas_list(&self) -> Result<Vec<Area>>;
    /// `areas_list` filtered to `kind = 'user'`, so the system area never reaches the sidebar.
    async fn areas_list_user_visible(&self) -> Result<Vec<Area>>;
    async fn area_get(&self, id: &str) -> Result<Option<Area>>;
    /// The singleton system area, `None` until `POST /api/areas/system` mints the row.
    async fn area_get_system(&self) -> Result<Option<Area>>;

    /// Folders claimed by a single area, sorted by path for stable UI ordering.
    async fn area_folders_by_area(&self, area_id: &str) -> Result<Vec<AreaFolder>>;
    /// Every folder across every area, `ORDER BY path ASC`; the resolve endpoint finds the covering claim application-side.
    async fn area_folders_list_all(&self) -> Result<Vec<AreaFolder>>;
    async fn area_folder_get(&self, id: i64) -> Result<Option<AreaFolder>>;

    async fn tracks_by_area(&self, area_id: &str) -> Result<Vec<Track>>;
    async fn track_get(&self, id: &str) -> Result<Option<Track>>;
    /// The Today launchpad track (`purpose = 'launchpad'`, single-valued by a partial unique index), or `None` before it has been minted.
    async fn track_get_launchpad(&self) -> Result<Option<Track>>;
    async fn track_detail(&self, id: &str) -> Result<Option<TrackDetail>>;
    /// The Claude Code permission policy that applies to `id`: its tree ROOT's `claude_permissions_policy` (a child row
    /// is always NULL). Fails closed on an unresolvable root (`Conflict`) or an undecodable stored value.
    async fn track_claude_permissions_ceiling(
        &self,
        id: &str,
    ) -> Result<Option<ClaudePermissionsScope>>;
    /// Calendar window query: every track whose lifespan overlaps `[since, until]` (inclusive):
    /// `created_at <= until AND (terminal_at IS NULL OR terminal_at >= since)`; all filters optional. Sorted by `created_at ASC, id ASC`.
    async fn tracks_window(
        &self,
        area_id: Option<&str>,
        since: Option<i64>,
        until: Option<i64>,
    ) -> Result<Vec<Track>>;

    /// Current execution rows in the track's plan, ordered `priority DESC, created_at_ms ASC, key ASC` (the scheduler's ready-set order).
    async fn tasks_by_track(&self, track_id: &str) -> Result<Vec<Task>>;
    /// Single-row fetch by the composed `"{track_id}:{key}"` id.
    async fn task_get(&self, id: &str) -> Result<Option<Task>>;
    /// Current execution only; None also covers a withdrawn pending projection.
    async fn task_current_get(&self, track_id: &str, key: &str) -> Result<Option<Task>>;
    /// Historical membership matters too: a recovered/withdrawn execution's
    /// worker card must not become an unowned manual terminal by omission.
    async fn task_for_worker_card(&self, card_id: &str) -> Result<Option<Task>>;
    /// All surviving execution rows, oldest generation first.
    async fn task_history_by_key(&self, track_id: &str, key: &str) -> Result<Vec<Task>>;
    /// Every non-terminal task across every track, in stable `(track_id, priority DESC, created_at_ms ASC, key ASC)` order; the scheduler's sweep source.
    async fn tasks_nonterminal(&self) -> Result<Vec<Task>>;
    /// In-flight frozen contexts affected by an edit to `dst_track_id`; the JOIN guarantees stale index rows never revive a terminal or deleted task.
    async fn task_contexts_by_dst_track(&self, dst_track_id: &str) -> Result<Vec<TaskContextRow>>;
    /// Explicit recovery candidates affected by an edit to `dst_track_id`, kept separate so stale rows never re-enter material classification or its retry budget.
    async fn stale_task_contexts_by_dst_track(
        &self,
        dst_track_id: &str,
    ) -> Result<Vec<TaskContextRow>>;
    /// Sweep source. Deliberately reads tasks rather than the reverse index.
    async fn task_contexts_inflight_fresh(&self) -> Result<Vec<TaskContextRow>>;
    /// Recovery sweep source for non-terminal rows that already carry stale.
    async fn task_contexts_inflight_stale(&self) -> Result<Vec<TaskContextRow>>;
    /// `worker_sessions.spawn_op_id` resolves to `operations.idempotency_key`, the immutable task id the worker operation was submitted with.
    async fn operation_idempotency_key_by_id(&self, op_id: &str) -> Result<Option<String>>;

    async fn cards_by_track(&self, track_id: &str) -> Result<Vec<Card>>;
    async fn track_report_cards_by_area(&self, area_id: &str) -> Result<Vec<Card>>;
    async fn card_get(&self, id: &str) -> Result<Option<Card>>;
    /// Atomic single-row fetch of a card with its opaque CRDT blob (`cards.body_crdt`), so a concurrent persist can never tear payload vs. CRDT.
    async fn card_get_with_body_crdt(&self, id: &str) -> Result<Option<(Card, Option<Vec<u8>>)>>;
    /// Read-time task diagnostics, evaluated in one read transaction with the
    /// same DB-aware predicate as report projection.
    async fn task_diagnostics(
        &self,
        track_id: &str,
        blocks: &[calm_types::track_report::ReportBlock],
        task_budget_default: i64,
    ) -> Result<Vec<crate::db::sqlite::BlockVerdict>>;
    async fn card_role_get(&self, id: &str) -> Result<Option<CardRole>>;
    /// Page **every** `harness_items` row for a card, whatever its `method`. The transcript feed must NOT use it.
    async fn harness_item_list_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<HarnessItem>>;

    /// Page only the rows a transcript can render (`item/started`, `item/completed`, `turn/completed`). The filter is in the
    /// SQL on purpose: `limit` must be a budget of *renderable* rows, or stored plan rows eat the page and a short page
    /// reads as "no more rows". Do not widen this query back to unfiltered.
    async fn harness_item_list_transcript_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<HarnessItem>>;

    /// Page the `worker_flow_items` capture table for a card; same paging semantics as `harness_item_list_by_card` (rows always ascending regardless of direction).
    async fn worker_flow_item_list_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<crate::db::rows::WorkerFlowItemRow>>;

    /// Fetch the passive worker-flow capture cursor for one card/source.
    async fn worker_flow_cursor_get(
        &self,
        card_id: &str,
        source_kind: &str,
    ) -> Result<Option<crate::db::rows::WorkerFlowCursor>>;

    async fn overlays_for(&self, entity_kind: &str, entity_id: &str) -> Result<Vec<Overlay>>;
    /// Every overlay attached to entities of the given `entity_kind`, regardless of `entity_id`.
    async fn overlays_by_kind(&self, entity_kind: &str) -> Result<Vec<Overlay>>;

    async fn terminal_get(&self, id: &str) -> Result<Option<Terminal>>;
    async fn terminal_get_by_card(&self, card_id: &str) -> Result<Option<Terminal>>;
    /// Every terminal row whose card has no active worker session and is older than `grace_seconds`, excluding exited
    /// Terminal-card terminals (those follow their card). Used exclusively by the `terminal_sweeper`.
    async fn terminals_orphaned(&self, grace_seconds: i64) -> Result<Vec<Terminal>>;
    /// Every terminal row whose child has not recorded an exit yet, for boot-time supervisor reconciliation.
    async fn terminals_running(&self) -> Result<Vec<Terminal>>;

    /// Shared-daemon empty-goal planner cards that still need the TUI to fresh-start their first thread; re-registered with `PendingThreadStartRegistry` on boot.
    async fn shared_planner_cards_for_initial_prompt_takeover(
        &self,
    ) -> Result<Vec<(String, String, String, i64)>>;
    async fn plugins_list(&self) -> Result<Vec<Plugin>>;
    async fn plugins_list_all(&self) -> Result<Vec<Plugin>>;
    async fn plugin_get_by_id(&self, id: &str) -> Result<Option<Plugin>>;
    async fn plugin_token_get(&self, plugin_id: &str) -> Result<Option<(String, i64)>>;
    async fn plugin_kv_get(&self, plugin_id: &str, key: &str) -> Result<Option<serde_json::Value>>;
    async fn plugin_kv_list(
        &self,
        plugin_id: &str,
        prefix: &str,
    ) -> Result<Vec<(String, serde_json::Value)>>;

    async fn settings_get_all(&self) -> Result<Vec<(String, String)>>;

    /// Populate the supplied `CardRoleCache` from `cards.role`, so `AppState` can seed through the dyn-trait alone.
    async fn seed_card_role_cache(&self, cache: &CardRoleCache) -> Result<()>;

    /// Populate the supplied `TrackAreaCache` from `tracks.area_id`.
    async fn seed_track_area_cache(&self, cache: &TrackAreaCache) -> Result<()>;

    /// Look up `(card_id, stored_hash)` for a presented MCP token's SHA-256 hash. The caller must still run
    /// `verify_token(presented, &stored_hash)` for a constant-time compare before trusting the binding.
    async fn card_mcp_token_lookup_by_hash(
        &self,
        hashed_token: &str,
    ) -> Result<Option<(String, String)>>;

    /// Recover the card-derived actor identity for an authenticated worker session, keyed by `cards.session_id`.
    async fn card_identity_get_by_session(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionCardIdentity>>;

    /// The newest workspace lease held by a card; `releasing` leases are excluded because they may already be mid-teardown.
    async fn workspace_lease_for_card(&self, card_id: &str) -> Result<Option<WorkspaceLease>>;

    /// Look up the active worker session bound to a presented MCP token's hash; the caller then runs `verify_token`.
    /// Terminal or stale rows (`failed`, `exited`, `superseded`) deliberately collapse to `None`.
    async fn session_get_by_active_token_hash(
        &self,
        hashed_token: &str,
    ) -> Result<Option<WorkerSession>>;

    /// Reload a worker session by id without authority filtering, so per-call revalidation can reject identities whose session left the active set.
    async fn session_get_by_id(&self, id: &WorkerSessionId) -> Result<Option<WorkerSession>>;

    /// Whether a card owns a per-card MCP token row; only such threads may be reused without reminting.
    async fn card_mcp_token_exists_for_card(&self, card_id: &str) -> Result<bool>;

    async fn shared_daemon_runtime_get(&self) -> Result<SharedCodexDaemonRecord>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskContextRow {
    pub task_id: String,
    pub track_id: String,
    pub claim_context_json: Option<String>,
    pub closure_truncated: bool,
}

/// Eventized write surface: the **only** path that writes the persistent event log + broadcasts on the bus.
#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait RepoEventWrite: RepoRead {
    /// Run the closure inside one transaction, append the event in the same txn, commit, then broadcast; any failure
    /// rolls back with no entity row, no event row and no broadcast. `actor` is declared, not authenticated. Pick the most
    /// specific `scope`; `EventScope::System` only when none is determinable.
    async fn write_with_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &crate::event::EventBus,
        write: &WriteContext,
        f: WriteWithEventFn<'_>,
    ) -> Result<i64>;

    /// Plural counterpart to [`write_with_event`]: either every event lands and is broadcast, or none do. An empty vec
    /// rolls back with `CalmError::Internal`; a role denial on any event rolls back the whole batch as `Forbidden`.
    async fn write_with_events(
        &self,
        actor: ActorId,
        correlation: Option<&str>,
        bus: &crate::event::EventBus,
        write: &WriteContext,
        f: WriteWithEventsFn<'_>,
    ) -> Result<Vec<i64>>;

    /// Plural eventized write where each event carries its own actor, for atomic kernel-auto lifecycle hooks; role enforcement runs per tuple.
    async fn write_with_actor_events(
        &self,
        correlation: Option<&str>,
        bus: &crate::event::EventBus,
        write: &WriteContext,
        f: WriteWithActorEventsFn<'_>,
    ) -> Result<Vec<i64>>;

    /// Persist + broadcast a pure event (no associated entity write), same commit-then-emit invariant.
    async fn log_pure_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &crate::event::EventBus,
        card_role_cache: &CardRoleCache,
        track_area_cache: &TrackAreaCache,
        event: Event,
    ) -> Result<i64>;

    /// Run a tx-scoped write without persisting or broadcasting an event; the caller broadcasts downstream events via
    /// `log_pure_event`. No event means no role gate; cache write-through still happens inside the `_tx` helpers.
    async fn write_in_tx(&self, f: WriteInTxFn<'_>) -> Result<()>;

    /// Replay query: events with `id > since_id`, ascending. `limit` is required (the table grows for the deployment's
    /// lifetime); `i64::MAX` says "the whole log". Rows whose payload fails to deserialize are logged and skipped.
    /// Each tuple is `(events.id, event_version, EventScope, Event)` as persisted on the row, not the kernel's current constants.
    async fn events_since(
        &self,
        since_id: i64,
        limit: i64,
    ) -> Result<Vec<(i64, u32, EventScope, Event)>>;

    /// Bounded probe over the RAW `events` window past `since_id`: `(count, max_id)` of the first `probe_limit` rows.
    /// Raw is load-bearing: `events_since` silently drops malformed rows, so an over-cap decision made on its filtered
    /// length could stamp `_replay_complete` past events that were never sent. Index-only scan, never a full `COUNT(*)`.
    async fn events_raw_window_since(
        &self,
        since_id: i64,
        probe_limit: i64,
    ) -> Result<(i64, Option<i64>)>;

    /// Selected event kinds scoped to one track — a bounded audit-log slice for projection tools, not a replay cursor.
    async fn events_for_track(
        &self,
        track_id: &str,
        kinds: &[&str],
        since_id: Option<i64>,
    ) -> Result<Vec<TrackEvent>>;

    /// Lowest live `events.id`; a `since` cursor below it predates the retention horizon and gets `_snapshot_required`.
    async fn events_earliest_id(&self) -> Result<Option<i64>>;

    /// Highest `events.id` ever deleted by the retention pruner (durable, `0` if never pruned). A `since` cursor below it
    /// may have interior holes that `MIN(id)` cannot detect, because structural events are permanent.
    async fn events_prune_watermark(&self) -> Result<i64>;

    /// Highest live `events.id`; `_replay_complete` stamps the actual log tip so a client whose cursor is *ahead* of it can detect a reset kernel.
    async fn events_latest_id(&self) -> Result<Option<i64>>;
}

/// Raw sync-domain entity writes. Gated: `RouteRepo` does **not** carry this supertrait, so route handlers cannot call
/// these; a direct write here bypasses `write_with_event` and is invisible to replicas.
#[async_trait]
pub trait RepoSyncDomainRaw: RepoRead {
    async fn area_create(&self, p: NewArea) -> Result<Area>;
    async fn area_update(&self, id: &str, p: AreaPatch) -> Result<Area>;
    async fn area_delete(&self, id: &str) -> Result<()>;

    async fn track_create(&self, p: NewTrack) -> Result<Track>;
    async fn track_update(&self, id: &str, p: TrackPatch) -> Result<Track>;
    async fn track_delete(&self, id: &str) -> Result<()>;

    async fn card_create(&self, p: NewCard) -> Result<Card>;
    async fn card_update(&self, id: &str, p: CardPatch) -> Result<Card>;
    async fn card_delete(&self, id: &str) -> Result<()>;

    /// Upserts on the `(plugin_id, entity_kind, entity_id, kind)` unique tuple.
    async fn overlay_upsert(&self, p: NewOverlay) -> Result<Overlay>;
    async fn overlay_delete(
        &self,
        plugin_id: &str,
        entity_kind: &str,
        entity_id: &str,
        kind: &str,
    ) -> Result<()>;
}

/// Out-of-sync-domain writes (terminal lifecycle, plugin install/config, settings): server-private operational state, deliberately **not** event-sourced.
#[async_trait]
pub trait RepoOutOfDomain: RepoRead {
    // Track recipes are a user's private authoring artifact and emit no `Event`; cross-window staleness is handled by the `revision` CAS (loser gets 409).
    async fn track_recipe_create(&self, p: NewTrackRecipe) -> Result<TrackRecipe>;
    async fn track_recipe_update(
        &self,
        id: &str,
        p: NewTrackRecipe,
        if_revision: i64,
    ) -> Result<TrackRecipe>;
    async fn track_recipe_get(&self, id: &str) -> Result<Option<TrackRecipe>>;

    // Track-create idempotency binding: pure request-dedup bookkeeping. The WRITE side is deliberately absent from this
    // trait — a pooled write is precisely the failure this table exists to remove; the only writer is the `_tx` free function.
    /// Which track `(area_id, Idempotency-Key)` already minted, if any.
    async fn track_create_idempotency_get(
        &self,
        area_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<crate::db::sqlite::TrackCreateBinding>>;
    async fn track_recipe_delete(&self, id: &str) -> Result<()>;
    async fn track_recipe_list(&self) -> Result<Vec<TrackRecipe>>;

    async fn terminal_create(&self, p: NewTerminal) -> Result<Terminal>;
    /// Persist the child PID; the orphan-terminal sweeper uses it as a SIGTERM fallback target.
    async fn terminal_set_pid(&self, id: &str, pid: Option<u32>) -> Result<()>;
    /// Record the child's exit info. A signal-killed child writes `exit_code = None, signal_killed = true`; an `exit()`
    /// child writes `Some(_), false`. The repo enforces neither.
    async fn terminal_set_exit(
        &self,
        id: &str,
        exit_code: Option<i32>,
        signal_killed: bool,
    ) -> Result<()>;
    /// Atomically persist the exit status and bounded merged PTY output; synthetic recovery writers use `terminal_set_exit` and record an empty stream.
    async fn terminal_set_exit_with_output(
        &self,
        id: &str,
        exit_code: Option<i32>,
        signal_killed: bool,
        pty_output: &str,
        pty_output_truncated: bool,
    ) -> Result<()>;
    /// Clear stale PID and exit markers immediately before spawning or reattaching; recovered rows may carry a previous PID plus boot reconciliation markers.
    async fn terminal_clear_exit_for_spawn(&self, id: &str) -> Result<()>;
    /// Remove a terminal row by id.
    async fn terminal_delete(&self, id: &str) -> Result<()>;

    async fn shared_daemon_runtime_set(&self, update: SharedCodexDaemonUpdate) -> Result<()>;
    async fn shared_daemon_record_event(&self, action: &str, error: Option<&str>) -> Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn harness_item_insert(
        &self,
        worker_session_id: &str,
        card_id: &str,
        track_id: &str,
        thread_id: &str,
        turn_id: Option<&str>,
        item_uuid: Option<&str>,
        item_type: Option<&str>,
        method: &str,
        params: &str,
        input_segments: Option<&str>,
    ) -> Result<i64>;

    /// Idempotently record a terminal outcome under its exact session/card/
    /// thread/turn identity. Concurrent live and recovery writes return one row.
    async fn harness_turn_outcome_put(
        &self,
        worker_session_id: &str,
        card_id: &str,
        track_id: &str,
        thread_id: &str,
        turn_id: &str,
        params: &str,
    ) -> Result<i64>;

    // A projection row is a transcript row the KERNEL wrote at queue drain: `method = 'item/completed'`,
    // `item_type = 'userMessage'`, `turn_id IS NULL`, `item_uuid` = the CLIENT id. Nothing codex sends can produce that shape.
    // Keyed by `card_id`, never `worker_session_id`, because a successor runtime may upgrade the row after a repoint.

    /// The `id` of the projection row for `client_id` on `card_id`, if one stands.
    async fn transcript_projection_id(&self, card_id: &str, client_id: &str)
    -> Result<Option<i64>>;

    /// Upgrade the projection row in place with codex's echo (turn, item id, verbatim `params`); `input_segments` is
    /// untouched. `None` when no projection stands (the echo is then an ordinary insert).
    async fn transcript_projection_upgrade(
        &self,
        card_id: &str,
        client_id: &str,
        turn_id: Option<&str>,
        item_uuid: &str,
        params: &str,
    ) -> Result<Option<i64>>;

    /// Remove the projection row for `client_id` — its `turn/start` did not go out. Returns 0 or 1.
    async fn transcript_projection_delete(&self, card_id: &str, client_id: &str) -> Result<u64>;

    /// Append one captured worker-flow item, returning the new row id. `card_id` is nullable so the row can outlive
    /// its worker card (`ON DELETE SET NULL`); `worker_session_id` is a required FK.
    #[allow(clippy::too_many_arguments)]
    async fn worker_flow_item_insert(
        &self,
        card_id: Option<&str>,
        captured_session_id: Option<&str>,
        track_id: Option<&str>,
        worker_session_id: Option<&str>,
        kind: &str,
        payload: &str,
        created_at_ms: i64,
    ) -> Result<i64>;

    /// Upsert the passive worker-flow capture cursor for one card/source. `record_index` may move down when a rollout
    /// file is rewritten during compaction; callers validate the source identity before taking that reset path.
    #[allow(clippy::too_many_arguments)]
    async fn worker_flow_cursor_upsert(
        &self,
        card_id: &str,
        source_kind: &str,
        source_path: &str,
        record_index: i64,
        byte_offset: i64,
        last_source_uuid: Option<&str>,
        last_line_hash: Option<&str>,
        updated_at_ms: i64,
    ) -> Result<()>;

    /// Upsert by id; `installed_at` is preserved on update and `enabled` defaults to false on the install row.
    async fn plugin_install(&self, p: NewPlugin) -> Result<Plugin>;
    async fn plugin_update_enabled(&self, id: &str, enabled: bool) -> Result<Plugin>;
    /// Overwrite `user_config` (the opaque JSON blob the PATCH config route writes).
    async fn plugin_update_user_config(
        &self,
        id: &str,
        user_config: serde_json::Value,
    ) -> Result<Plugin>;
    /// Overwrite the persisted manifest blob; `GET /api/plugins/:id` reads from the DB row, not the live registry.
    async fn plugin_update_manifest(&self, id: &str, manifest: serde_json::Value)
    -> Result<Plugin>;
    async fn plugin_delete(&self, id: &str) -> Result<()>;

    /// Drop every overlay owned by a plugin, so a deleted plugin's overlays don't render as ghosts.
    async fn overlays_clear_by_plugin(&self, plugin_id: &str) -> Result<()>;

    /// Drop every KV row owned by a plugin.
    async fn plugin_kv_clear(&self, plugin_id: &str) -> Result<()>;

    // Per-plugin tokens: hash is hex-encoded `SHA-256(raw_token)`; expires_at is unix millis.
    async fn plugin_token_set(
        &self,
        plugin_id: &str,
        hashed_token: &str,
        expires_at: i64,
    ) -> Result<()>;
    async fn plugin_token_delete(&self, plugin_id: &str) -> Result<()>;

    // Per-plugin KV: values are arbitrary JSON; namespacing is enforced at this trait layer (no method takes a global key).
    async fn plugin_kv_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()>;
    async fn plugin_kv_delete(&self, plugin_id: &str, key: &str) -> Result<()>;

    // App-global settings: per-key INSERT OR REPLACE; an empty string is treated as a delete at the *route* boundary.
    async fn settings_upsert(&self, key: &str, value: &str) -> Result<()>;
    async fn settings_delete(&self, key: &str) -> Result<()>;

    // area_folders is an operational mapping table, not on the event-sourced path.
    /// Insert a folder with **no** overlap check; not reachable from HTTP, which goes through
    /// [`Self::area_folder_create_checked`]. Survives for tests and seeds that want states the checked writer refuses.
    async fn area_folder_create(&self, area_id: &str, path: &str) -> Result<AreaFolder>;
    /// Atomically claim `path` for `area_id`: the overlap scan and the INSERT run inside one `BEGIN IMMEDIATE` transaction
    /// (`UNIQUE(path)` only catches *equal* paths). Returns `Conflict`, not `Err`, for overlap. `path` MUST already be
    /// normalized: a non-normalized input is silently misclassified, and `.`/`..` segments are not caught at this layer.
    async fn area_folder_create_checked(
        &self,
        area_id: &str,
        path: &str,
    ) -> Result<crate::area_folder_claim::AreaFolderClaim>;
    /// Delete a folder by integer id; `NotFound` when no row exists.
    async fn area_folder_delete(&self, id: i64) -> Result<()>;
}

/// Re-exports every sub-trait + `Repo` for test modules; production code should import the *narrowest* trait it needs.
pub mod prelude {
    pub use super::{
        Repo, RepoEventWrite, RepoOutOfDomain, RepoRead, RepoSyncDomainRaw, RouteRepo,
        WorkspaceLease,
    };
    pub use crate::session_projection_repo::WorkerSessionProjectionRepo;
    pub use crate::session_repo::SessionRepo;
}

/// The trait object route handlers see via `AppState::repo`. Excludes [`RepoSyncDomainRaw`] — that's the gate.
/// Blanket-implemented for any type combining the route-facing supertraits.
pub trait RouteRepo: RepoEventWrite + RepoOutOfDomain + WorkerSessionProjectionRepo {}
impl<T> RouteRepo for T where
    T: RepoEventWrite + RepoOutOfDomain + WorkerSessionProjectionRepo + ?Sized
{
}

/// Full repo capability; `Arc<dyn Repo>` upcasts to `Arc<dyn RouteRepo>`. `&dyn Repo` is the internal-access escape
/// hatch reached via `AppState::raw_repo()`.
pub trait Repo: RouteRepo + RepoSyncDomainRaw + WorkerSessionProjectionRepo + SessionRepo {
    /// Internal sqlite escape hatch for infrastructure that owns tables outside the route-facing traits; kept off `RouteRepo`.
    fn sqlite_pool(&self) -> Option<SqlitePool> {
        None
    }

    /// The stable identity of the database behind this repo, minted into the one-row `database_identity` table on first open; two boots on the same file answer the same id.
    fn database_id(&self) -> Arc<String>;
}

/// Generic wrapper over `RepoEventWrite::write_with_event` for callers who want a typed row back; the row is captured
/// in an outer mutex so the trait method stays dyn-compatible. Purely sugar — the invariants come from the trait method.
#[allow(clippy::too_many_arguments)]
pub async fn write_with_event_typed<R, F>(
    repo: &dyn RepoEventWrite,
    actor: ActorId,
    scope: EventScope,
    correlation: Option<&str>,
    bus: &crate::event::EventBus,
    write: &WriteContext,
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
        .write_with_event(actor, scope, correlation, bus, write, boxed)
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

/// Generic plural counterpart to [`write_with_event_typed`]: one typed row + one or more `(scope, event)` pairs from one tx.
pub async fn write_with_events_typed<R, F>(
    repo: &dyn RepoEventWrite,
    actor: ActorId,
    correlation: Option<&str>,
    bus: &crate::event::EventBus,
    write: &WriteContext,
    f: F,
) -> Result<(R, Vec<i64>)>
where
    R: Send + 'static,
    F: for<'tx> FnOnce(
            &'tx mut Transaction<'_, Sqlite>,
        ) -> BoxFuture<'tx, Result<(R, Vec<(EventScope, Event)>)>>
        + Send
        + 'static,
{
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let captured: Arc<Mutex<Option<R>>> = Arc::new(Mutex::new(None));
    let captured_inner = Arc::clone(&captured);

    let boxed: WriteWithEventsFn<'_> = Box::new(move |tx| {
        let captured_inner = Arc::clone(&captured_inner);
        Box::pin(async move {
            let (row, events) = f(tx).await?;
            *captured_inner.lock().await = Some(row);
            Ok(events)
        })
    });

    let event_ids = repo
        .write_with_events(actor, correlation, bus, write, boxed)
        .await?;
    let row = Arc::try_unwrap(captured)
        .map_err(|_| {
            crate::error::CalmError::Internal(
                "write_with_events_typed: outstanding reference to captured row".into(),
            )
        })?
        .into_inner()
        .ok_or_else(|| {
            crate::error::CalmError::Internal(
                "write_with_events_typed: closure did not set row".into(),
            )
        })?;
    Ok((row, event_ids))
}

/// Typed counterpart to [`RepoEventWrite::write_with_actor_events`].
pub async fn write_with_actor_events_typed<R, F>(
    repo: &dyn RepoEventWrite,
    correlation: Option<&str>,
    bus: &crate::event::EventBus,
    write: &WriteContext,
    f: F,
) -> Result<(R, Vec<i64>)>
where
    R: Send + 'static,
    F: for<'tx> FnOnce(
            &'tx mut Transaction<'_, Sqlite>,
        ) -> BoxFuture<'tx, Result<(R, Vec<(ActorId, EventScope, Event)>)>>
        + Send
        + 'static,
{
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let captured: Arc<Mutex<Option<R>>> = Arc::new(Mutex::new(None));
    let captured_inner = Arc::clone(&captured);

    let boxed: WriteWithActorEventsFn<'_> = Box::new(move |tx| {
        let captured_inner = Arc::clone(&captured_inner);
        Box::pin(async move {
            let (row, events) = f(tx).await?;
            *captured_inner.lock().await = Some(row);
            Ok(events)
        })
    });

    let event_ids = repo
        .write_with_actor_events(correlation, bus, write, boxed)
        .await?;
    let row = Arc::try_unwrap(captured)
        .map_err(|_| {
            crate::error::CalmError::Internal(
                "write_with_actor_events_typed: outstanding reference to captured row".into(),
            )
        })?
        .into_inner()
        .ok_or_else(|| {
            crate::error::CalmError::Internal(
                "write_with_actor_events_typed: closure did not set row".into(),
            )
        })?;
    Ok((row, event_ids))
}

/// Typed counterpart to [`RepoEventWrite::write_in_tx`], with the same row-capture trick as [`write_with_event_typed`].
pub async fn write_in_tx_typed<R, F>(repo: &dyn RepoEventWrite, f: F) -> Result<R>
where
    R: Send + 'static,
    F: for<'tx> FnOnce(&'tx mut Transaction<'_, Sqlite>) -> BoxFuture<'tx, Result<R>>
        + Send
        + 'static,
{
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let captured: Arc<Mutex<Option<R>>> = Arc::new(Mutex::new(None));
    let captured_inner = Arc::clone(&captured);

    let boxed: WriteInTxFn<'_> = Box::new(move |tx| {
        let captured_inner = Arc::clone(&captured_inner);
        Box::pin(async move {
            let row = f(tx).await?;
            *captured_inner.lock().await = Some(row);
            Ok(())
        })
    });

    repo.write_in_tx(boxed).await?;
    let row = Arc::try_unwrap(captured)
        .map_err(|_| {
            crate::error::CalmError::Internal(
                "write_in_tx_typed: outstanding reference to captured row".into(),
            )
        })?
        .into_inner()
        .ok_or_else(|| {
            crate::error::CalmError::Internal("write_in_tx_typed: closure did not set row".into())
        })?;
    Ok(row)
}
