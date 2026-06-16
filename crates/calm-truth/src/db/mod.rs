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
//! and only then emits a `BroadcastEnvelope { id, actor, event }` on the
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
//!
//! ## Trait capability split (Scope α)
//!
//! `Repo` is split into sub-traits along the *capability* axis. The
//! goal is to make "no route handler can reach a raw sync-domain write"
//! a compile-time invariant, not a grep-time one:
//!
//!   * [`RepoRead`] — universal read surface (`coves_list`, `wave_get`,
//!     `overlays_for`, `plugins_list_all`, `terminal_get`, …). Anyone
//!     with a `&dyn RepoRead` can fetch anything; no writes.
//!   * [`RepoEventWrite`] — the audited write surface
//!     (`write_with_event`, `log_pure_event`, `events_since`,
//!     `events_earliest_id`). Supertrait `RepoRead` because every audited
//!     write closure typically needs to read a parent row first.
//!   * [`RepoSyncDomainRaw`] — **gated.** Raw entity writes for the
//!     in-scope sync domain: coves, waves, cards, overlays. These exist
//!     on the trait because `SqlxRepo` is the canonical impl and the
//!     types must be addressable somewhere — but the `RouteRepo` trait
//!     object route handlers see does **not** include this supertrait,
//!     so a handler that types `s.repo.cove_create(...)` fails to
//!     compile. The only legitimate consumers are db-internal helpers,
//!     tests, and fixtures.
//!   * [`RepoOutOfDomain`] — operational writes the kernel deliberately
//!     keeps off the sync engine: `terminal_*` (server-side process
//!     lifecycle), `plugin_*` (install/enable/config/KV/tokens),
//!     `settings_*` (app-global config). These do **not** emit events;
//!     they are server-private state that no other peer needs to
//!     replicate. Routes see them — they are part of the normal REST
//!     surface for plugin install, settings PUT, etc.
//!   * [`RuntimeRepo`] — runtime table ownership for provider/card runtime
//!     bookkeeping. It stays on the full internal repo surface, outside
//!     route-facing sync-domain writes.
//!
//! [`Repo`] is the full internal marker that requires all of those
//! capabilities. `SqlxRepo` implements it directly so infrastructure can
//! also expose the sqlite pool escape hatch without widening `RouteRepo`.
//!
//! [`RouteRepo`] is the *narrow* trait object `AppState::repo` exposes
//! to handlers: `RepoEventWrite + RepoOutOfDomain` (which transitively
//! grants `RepoRead`). It deliberately excludes `RepoSyncDomainRaw` —
//! that's the whole point.
//!
//! Internal callers that legitimately need raw access (db-private
//! helpers, replay lib, terminal_sweeper, tests) reach `&dyn Repo` via
//! `AppState::raw_repo()` — the ugly name is a deliberate signal that
//! you're stepping outside the gate.

use crate::card_role_cache::CardRoleCache;
use crate::error::Result;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::model::*;
use crate::runtime_repo::RuntimeRepo;
use crate::session_repo::SessionRepo;
use crate::state::WriteContext;
use crate::wave_cove_cache::WaveCoveCache;
use async_trait::async_trait;
use calm_types::worker::{WorkerSession, WorkerSessionId};
use futures::future::BoxFuture;
use sqlx::{Sqlite, SqlitePool, Transaction};

pub mod rows;
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

/// PR6 (#136) — plural counterpart to [`WriteWithEventFn`]. Closure
/// returns a `Vec<(EventScope, Event)>` so a single transaction can
/// persist multiple events, each tagged with its own scope. Used by
/// `routes::waves::create_wave` to atomically emit both
/// `Event::WaveUpdated` (scope = Wave) and `Event::CardAdded`
/// (scope = Card) for the auto-minted spec card.
///
/// Invariants:
///   * The closure must return a non-empty vec — an empty vec is a
///     contract violation and causes the trait method to roll back
///     with `CalmError::Internal`.
///   * Each `(scope, event)` is independently checked against
///     `enforce_role` with the supplied `actor`. Any single
///     `RoleViolation` rolls the entire batch back: neither the
///     entity write nor any event row survives.
///   * Events are persisted in vec order and broadcast in the same
///     order post-commit. A subscriber that listens to multiple
///     scopes sees the order the closure declared.
pub type WriteWithEventsFn<'a> = Box<
    dyn for<'tx> FnOnce(
            &'tx mut Transaction<'_, Sqlite>,
        ) -> BoxFuture<'tx, Result<Vec<(EventScope, Event)>>>
        + Send
        + 'a,
>;

/// #597 internal helper shape: like [`WriteWithEventsFn`], but each event
/// carries its own actor. Used when a kernel-auto lifecycle event must commit
/// atomically with the spec/worker write that triggered it.
pub type WriteWithActorEventsFn<'a> = Box<
    dyn for<'tx> FnOnce(
            &'tx mut Transaction<'_, Sqlite>,
        ) -> BoxFuture<'tx, Result<Vec<(ActorId, EventScope, Event)>>>
        + Send
        + 'a,
>;

/// Issue #310 — event-less counterpart to [`WriteWithEventFn`]. Closure
/// runs in one sqlx transaction and returns nothing; no event row is
/// appended to the `events` log, no broadcast is sent. Used by the
/// dispatcher's two-stage worker spawn, where the `card.added` event is
/// deferred until the renderer/supervisor entry has been established.
///
/// **Caveat — crash-window orphan.** The window between the row-creation
/// tx commit and the post-spawn `log_pure_event(CardAdded)` is on the
/// order of microseconds, but it is real.
/// but it's real. If the kernel process dies mid-window (SIGKILL,
/// OOM, panic that escapes the tokio task supervisor), the durable
/// state on next boot is:
///   * The card row is on disk; the terminal row is on disk; the
///     daemon child may or may not be alive depending on when in the
///     window we died.
///   * No `CardAdded` event was appended to the events log — replay
///     will not surface the card.
///   * No `CardAdded` broadcast fired — no subscriber learned about
///     the row at the time it was written.
///   * The terminal-sweeper will NOT reap the orphan while the runtime
///     row is active: its SQL excludes terminals still referenced by
///     `runtimes.terminal_run_id`.
///   * The operation idempotency row owns future retries — a user
///     who re-dispatches with the same key observes the existing
///     operation result instead of a fresh worker spawn.
///
/// The dispatcher's `TaskFailed` emission only fires on a returned
/// error from a live spawn, not on a process death mid-spawn, so the
/// requesting spec harness never receives a task failure observation.
/// Net effect: an undead card that nothing knows about.
///
/// This is accepted scope for the current fix (the alternative —
/// emitting `CardAdded` inside the tx — is the live `child-exited`
/// bug we're solving). A proper fix needs boot-time events-log
/// reconciliation (scan for terminal rows whose `cards.payload.id`
/// has no corresponding `CardAdded` event and either emit the event
/// or rollback the row). Tracked for followup; see issue TBD.
pub type WriteInTxFn<'a> = Box<
    dyn for<'tx> FnOnce(&'tx mut Transaction<'_, Sqlite>) -> BoxFuture<'tx, Result<()>> + Send + 'a,
>;

#[derive(Clone, Debug)]
pub struct WaveEvent {
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

/// Internal MCP auth identity recovered from `cards.session_id`.
///
/// Deliberately narrower than [`Card`]: `cards.session_id` is not part of
/// the public card wire model, but the MCP transport needs this card-derived
/// actor data while session identity is the credential authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCardIdentity {
    pub card_id: CardId,
    pub role: CardRole,
    pub wave_id: WaveId,
    pub cove_id: CoveId,
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

// ---------------------------------------------------------------------------
// Sub-trait split. See the "Trait capability split" section in the module
// docs for the rationale. Each sub-trait carries `Send + Sync + 'static` so
// the resulting trait objects can live in `Arc<dyn ...>`.
//
// One implementation note: every sub-trait below uses `#[async_trait]` and
// is dyn-compatible (no generic methods, no `Self` in return types). The
// `RouteRepo` alias is also dyn-compatible because the only methods it
// "carries" are inherited via supertraits — `dyn RouteRepo` upcasts to
// `dyn RepoEventWrite` (and from there to `dyn RepoRead`) via the same
// vtable, since trait objects with supertrait constraints have a single
// merged vtable layout.
// ---------------------------------------------------------------------------

/// Universal read surface. Anything that can hand out a `&dyn RepoRead`
/// permits arbitrary reads; no writes are reachable from here.
#[async_trait]
pub trait RepoRead: Send + Sync + 'static {
    // ---- coves
    /// Every cove regardless of [`CoveKind`]. Internal callers (replay,
    /// debug surfaces, integration tests that assert on the system
    /// cove's existence) use this; the user-facing `GET /api/coves`
    /// route prefers [`RepoRead::coves_list_user_visible`] so the
    /// singleton system cove introduced by issue #175 stays hidden
    /// from the sidebar surface.
    async fn coves_list(&self) -> Result<Vec<Cove>>;
    /// Issue #175 — `coves_list` filtered to `kind = 'user'`. Default
    /// read surface for `GET /api/coves` so the system cove that hosts
    /// the default Today terminal's wave never reaches the sidebar.
    /// Opt back into the full list via `?include_system=true` (calls
    /// [`RepoRead::coves_list`]).
    async fn coves_list_user_visible(&self) -> Result<Vec<Cove>>;
    async fn cove_get(&self, id: &str) -> Result<Option<Cove>>;
    /// Issue #175 — fetch the singleton system cove if one exists.
    /// Returns `None` until the first call to `POST /api/coves/system`
    /// mints the row. Backed by the unique partial index on
    /// `coves(kind) WHERE kind = 'system'` from migration 0009.
    async fn cove_get_system(&self) -> Result<Option<Cove>>;

    // ---- cove_folders
    /// Issue #250 PR 1 — folders claimed by a single cove, sorted by
    /// path for stable UI ordering.
    async fn cove_folders_by_cove(&self, cove_id: &str) -> Result<Vec<CoveFolder>>;
    /// Issue #250 PR 1 — every folder across every cove. Used by the
    /// resolve endpoint to do longest-prefix matching application-side
    /// (SQLite has no native prefix function fast enough to outweigh
    /// a Rust-side O(N) scan at the table sizes we expect — folders are
    /// minted manually by users, not auto-discovered).
    async fn cove_folders_list_all(&self) -> Result<Vec<CoveFolder>>;
    /// Issue #250 PR 1 — single-row fetch for the DELETE handler's
    /// existence check.
    async fn cove_folder_get(&self, id: i64) -> Result<Option<CoveFolder>>;

    // ---- waves
    async fn waves_by_cove(&self, cove_id: &str) -> Result<Vec<Wave>>;
    async fn wave_get(&self, id: &str) -> Result<Option<Wave>>;
    async fn wave_detail(&self, id: &str) -> Result<Option<WaveDetail>>;
    /// Issue #250 PR 2 — calendar window query.
    ///
    /// Returns every wave whose lifespan overlaps the half-open
    /// `[since, until]` millisecond range (both endpoints inclusive
    /// per the issue spec): `created_at <= until AND (terminal_at IS
    /// NULL OR terminal_at >= since)`. `cove_id`, when `Some(_)`,
    /// further restricts the result to a single cove.
    ///
    /// Any combination of the three filters is legal — when all three
    /// are `None` the query degenerates to "every wave in the DB" so
    /// callers that omit every parameter still get a sensible default.
    /// Sorted by `created_at ASC, id ASC` for stable pagination later;
    /// PR 2 returns the full window in one shot.
    async fn waves_window(
        &self,
        cove_id: Option<&str>,
        since: Option<i64>,
        until: Option<i64>,
    ) -> Result<Vec<Wave>>;

    // ---- tasks (issue #644 — wave-scoped task plan)
    /// Every task in the wave's plan, ordered for stable listing:
    /// `priority DESC, created_at_ms ASC, key ASC` (the same order the
    /// PR-B scheduler's ready-set query uses, design §5.2). Backed by
    /// the `tasks_wave_status_idx` index from migration 0041.
    async fn tasks_by_wave(&self, wave_id: &str) -> Result<Vec<Task>>;
    /// Single-row fetch by the composed `"{wave_id}:{key}"` id.
    async fn task_get(&self, id: &str) -> Result<Option<Task>>;
    /// Issue #644 PR-B — every non-terminal task across every wave
    /// (`pending` / `dispatched` / `running` / `verifying`), in stable
    /// `(wave_id, priority DESC, created_at_ms ASC, key ASC)` order.
    /// Backed by `tasks_wave_status_idx`. Used by the scheduler's sweep
    /// (boot, periodic reconcile, post-`Lagged`) — design §8.
    async fn tasks_nonterminal(&self) -> Result<Vec<Task>>;
    /// Minimal operation lookup for session-owned worker convergence:
    /// `worker_sessions.spawn_op_id` resolves to `operations.idempotency_key`,
    /// which is the immutable task id the worker operation was submitted with.
    async fn operation_idempotency_key_by_id(&self, op_id: &str) -> Result<Option<String>>;

    // ---- cards
    async fn cards_by_wave(&self, wave_id: &str) -> Result<Vec<Card>>;
    async fn card_get(&self, id: &str) -> Result<Option<Card>>;
    async fn card_role_get(&self, id: &str) -> Result<Option<CardRole>>;
    async fn harness_item_list_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<HarnessItem>>;

    /// #695 PR2 — page the `worker_flow_items` capture table for a card.
    ///
    /// Sibling of [`harness_item_list_by_card`](Self::harness_item_list_by_card)
    /// with identical paging semantics: `after_id` is the exclusive cursor
    /// (0 means "from the start" — for `descending` it maps to "from the
    /// newest"), `limit` is clamped to a sane ceiling, and rows come back in
    /// ascending `id` order regardless of paging direction so callers can
    /// always append. Returns the raw [`WorkerFlowItemRow`](crate::db::rows::WorkerFlowItemRow);
    /// projection into a render shape is PR3's job, not this storage layer's.
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

    // ---- overlays
    async fn overlays_for(&self, entity_kind: &str, entity_id: &str) -> Result<Vec<Overlay>>;
    /// List every overlay attached to entities of the given `entity_kind`
    /// (e.g. `"wave"`), regardless of `entity_id`. Used by the sidebar so
    /// wave status indicators stay accurate without per-wave detail fetches.
    async fn overlays_by_kind(&self, entity_kind: &str) -> Result<Vec<Overlay>>;

    // ---- terminals (read-only)
    async fn terminal_get(&self, id: &str) -> Result<Option<Terminal>>;
    async fn terminal_get_by_card(&self, card_id: &str) -> Result<Option<Terminal>>;
    /// Return every terminal row that has no active runtime pointing at it
    /// via `runtimes.terminal_run_id`, and whose `created_at` is older than
    /// `grace_seconds` ago.
    /// Used exclusively by the `terminal_sweeper` background task.
    async fn terminals_orphaned(&self, grace_seconds: i64) -> Result<Vec<Terminal>>;
    /// Return every terminal row whose child has not recorded an exit yet.
    /// Used by boot-time supervisor reconciliation after #388 Phase 3b.
    async fn terminals_running(&self) -> Result<Vec<Terminal>>;

    /// Shared-daemon empty-goal spec cards that still need the TUI to
    /// fresh-start their first thread. These are excluded from the legacy
    /// initial-prompt bootstrap path and must be re-registered with
    /// `PendingThreadStartRegistry` on boot.
    async fn shared_spec_cards_for_initial_prompt_takeover(
        &self,
    ) -> Result<Vec<(String, String, String, i64)>>;
    // ---- plugins (read-only)
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

    // ---- settings (read-only)
    async fn settings_get_all(&self) -> Result<Vec<(String, String)>>;

    /// PR3 (#136) — populate the supplied `CardRoleCache` from the persisted
    /// `cards.role` column. Boot-time helper for `AppState::new` that keeps
    /// the cache implementation pool-agnostic (the `&SqlitePool`-typed
    /// `CardRoleCache::seed_from_db` is private to the sqlite backend, but
    /// this trait method lets `AppState` seed through the dyn-trait alone).
    async fn seed_card_role_cache(&self, cache: &CardRoleCache) -> Result<()>;

    /// #234 — populate the supplied `WaveCoveCache` from the persisted
    /// `waves.cove_id` column. Mirror of [`seed_card_role_cache`].
    async fn seed_wave_cove_cache(&self, cache: &WaveCoveCache) -> Result<()>;

    /// PR7a (#136) — look up the card id bound to a presented MCP
    /// token's `SHA-256` hash. Returns `None` if no row matches. The
    /// MCP server uses this during the `initialize` handshake to
    /// resolve which card identity to bind the connection to.
    ///
    /// Returns `Some((card_id, stored_hash))` on match, or `None` when
    /// no row carries the queried hash. The caller is expected to pass
    /// `hash_token(presented)` to look up the row, then immediately run
    /// `verify_token(presented, &stored_hash)` against the returned hash
    /// for constant-time equality before trusting the binding —
    /// `SELECT WHERE hashed_token = ?` already operates on the hash, so
    /// the column-equality check is the primary filter; the explicit
    /// verify is defense-in-depth against a malformed `hashed_token`
    /// (e.g. truncated migration) somehow matching a non-equivalent
    /// presented hash. PR7a.1 (#136 followup) tightened this from
    /// `Option<String>` to `Option<(String, String)>` so the handshake
    /// can actually run that constant-time compare.
    async fn card_mcp_token_lookup_by_hash(
        &self,
        hashed_token: &str,
    ) -> Result<Option<(String, String)>>;

    /// PR7b-i Unit 2 (#679) — recover the card-derived actor identity for
    /// an authenticated worker session. This is intentionally keyed by
    /// `cards.session_id` so persisted events continue to use card-shaped
    /// actors while the token authority comes from `worker_sessions`.
    async fn card_identity_get_by_session(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionCardIdentity>>;

    /// PR7b-i Unit 1 (#679) — look up the active worker session bound to
    /// a presented MCP token's `SHA-256` hash. Mirrors
    /// [`RepoRead::card_mcp_token_lookup_by_hash`]: the caller passes
    /// `hash_token(presented)`, receives the stored session row, then
    /// immediately runs `verify_token(presented, stored_hash)`.
    ///
    /// Only live authority-bearing sessions are returned. Terminal or
    /// stale rows (`failed`, `exited`, `superseded`) deliberately collapse
    /// to `None` so Unit 2 can bind a connection principal only to the
    /// current session actor.
    async fn session_get_by_active_token_hash(
        &self,
        hashed_token: &str,
    ) -> Result<Option<WorkerSession>>;

    /// Reload a worker session by id without applying authority filtering.
    /// MCP per-call revalidation uses this to reject cached card-bound
    /// identities once their bound session leaves the active authority set.
    async fn session_get_by_id(&self, id: &WorkerSessionId) -> Result<Option<WorkerSession>>;

    /// Return whether a card owns a per-card MCP token row. Used by the
    /// spec-harness reusable-thread invariant: only threads minted under
    /// PR #567 should be reused without reminting.
    async fn card_mcp_token_exists_for_card(&self, card_id: &str) -> Result<bool>;

    async fn shared_daemon_runtime_get(&self) -> Result<SharedCodexDaemonRecord>;
}

/// Eventized write surface. The **only** path that writes to the persistent
/// event log + broadcasts on the bus. Carries `RepoRead` as a supertrait
/// because every write closure typically needs to read a parent row first
/// (and any read is also legal from inside the closure).
#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait RepoEventWrite: RepoRead {
    /// Atomic write + event-log invariant: run the closure inside one
    /// sqlx transaction, then `INSERT INTO events ... RETURNING id` in
    /// the same txn, commit, and emit `BroadcastEnvelope { id, actor, event }`
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
    /// ([`ActorId::User`] / [`ActorId::Kernel`] / [`ActorId::Plugin`] /
    /// [`ActorId::AiCodex`] / …). Not authenticated — see design doc
    /// §1.1 disclaimer. PR2 of #136 typed this from `&str` to `ActorId`
    /// so PR3's `enforce_role` can pattern-match on the variant cleanly.
    /// The value is JSON-serialized into the existing `events.actor`
    /// TEXT column (`serde_json::to_string(&actor)`) — forward-compatible
    /// with future actor enrichment without a schema bump.
    ///
    /// `scope` is the event's "home scope" in the cove → wave → card
    /// hierarchy. Persisted into the `events.scope_*` columns added in
    /// migration 0007 so PR3/PR5/PR8 can filter / route / authorize
    /// without re-parsing the event payload. Pick the most specific
    /// scope you can determine at the call site; fall back to
    /// [`EventScope::System`] only when no scope is determinable
    /// (e.g. plugin state transitions, server-internal lifecycle).
    ///
    /// `correlation` is optional; populated for plugin tool-call writes
    /// per design §9 (`"user_tool_call:<call_id>"`).
    ///
    /// `write` is the [`WriteContext`] wrapper used by PR3's
    /// `role_gate::enforce_role` to consult both write-through caches
    /// inside the transaction, after the closure produces an event
    /// and before the event row is appended.
    async fn write_with_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &crate::event::EventBus,
        write: &WriteContext,
        f: WriteWithEventFn<'_>,
    ) -> Result<i64>;

    /// PR6 (#136) — plural counterpart to [`write_with_event`]. Persist
    /// and broadcast **multiple events** from one transaction, each
    /// tagged with its own [`EventScope`]. The single transaction
    /// invariant (closure → enforce_role per event → persist all →
    /// commit → broadcast all) is preserved; either every event lands
    /// and is broadcast, or none of them do.
    ///
    /// All events in the batch share the supplied `actor` — the
    /// "request initiator" is one per transaction. Per-event scopes
    /// let a single mutation (e.g. wave create with auto-minted spec
    /// card) emit both a wave-scoped and a card-scoped envelope so
    /// subscribers filtered by either scope pick up the relevant
    /// frame without re-routing through ancestors.
    ///
    /// Error semantics mirror `write_with_event`:
    ///   * Closure returns `Err(e)`: txn rolls back; `Err(e)` bubbles
    ///     up; no entity rows, no event rows, no broadcasts.
    ///   * `enforce_role` denies any event in the batch: txn rolls
    ///     back; the violation surfaces as `CalmError::Forbidden`;
    ///     no rows survive.
    ///   * Empty vec returned by the closure: txn rolls back with
    ///     `CalmError::Internal` — every caller must emit at least
    ///     one event (use `write_with_event` if the singular case is
    ///     all you need).
    ///   * Per-event `event_append_in_tx` failure mid-batch: txn
    ///     rolls back; subsequent events in the batch are never
    ///     persisted; the earlier-persisted events vanish with the
    ///     rollback (commit-then-emit invariant: nothing was
    ///     broadcast yet).
    ///
    /// Returns the assigned `events.id` for each persisted event, in
    /// the order the closure produced them.
    async fn write_with_events(
        &self,
        actor: ActorId,
        correlation: Option<&str>,
        bus: &crate::event::EventBus,
        write: &WriteContext,
        f: WriteWithEventsFn<'_>,
    ) -> Result<Vec<i64>>;

    /// #597 — plural eventized write where each event carries its own actor.
    ///
    /// This is reserved for atomic kernel-auto lifecycle hooks: the triggering
    /// write remains attributed to the spec/worker actor, while the automatic
    /// `wave.updated` transition is attributed to `Kernel` or
    /// `KernelDispatcher`. Role enforcement still runs independently for each
    /// `(actor, scope, event)` tuple and any refusal rolls back the full tx.
    async fn write_with_actor_events(
        &self,
        correlation: Option<&str>,
        bus: &crate::event::EventBus,
        write: &WriteContext,
        f: WriteWithActorEventsFn<'_>,
    ) -> Result<Vec<i64>>;

    /// Persist + broadcast a pure event (no associated entity write). Same
    /// commit-then-emit invariant as `write_with_event`, but no transaction
    /// closure — the event itself is the only write.
    ///
    /// Used for `Event::CodexHook` (ingest at
    /// `routes::codex::ingest_hook`) and `Event::PluginState` (plugin
    /// supervisor lifecycle in `plugin_host::PluginHost::emit_state`).
    /// Returns the assigned `events.id`.
    ///
    /// PR2 of #136: `actor` is now typed [`ActorId`]; `scope` carries the
    /// event's home scope (use [`EventScope::System`] for plugin-state
    /// transitions which have no entity scope; use [`EventScope::Card`]
    /// for codex hooks when the wave→cove chain is joinable, otherwise
    /// fall back to [`EventScope::System`]).
    async fn log_pure_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &crate::event::EventBus,
        card_role_cache: &CardRoleCache,
        wave_cove_cache: &WaveCoveCache,
        event: Event,
    ) -> Result<i64>;

    /// Issue #310 — run a tx-scoped write without persisting or broadcasting
    /// an event. Same atomicity contract as `write_with_event` (closure
    /// runs in one tx, error rolls back, success commits), but the caller
    /// takes responsibility for broadcasting any downstream event(s) via
    /// `log_pure_event` after this returns.
    ///
    /// The dispatcher uses this for the first stage of its two-stage
    /// worker-spawn pipeline: the tx mints the worker card + terminal row,
    /// commits, and only then establishes the renderer/supervisor entry.
    /// The `card.added` event is emitted via `log_pure_event`
    /// post-spawn-success so subscribers never see a `CardAdded` frame
    /// whose backing terminal is not yet attachable.
    ///
    /// **Why a separate method instead of passing a no-op event to
    /// `write_with_event`**: the broadcast bus is hard-coded into
    /// `write_with_event`'s post-commit step; suppressing the broadcast
    /// would require a flag on the trait method that every other call
    /// site has to default. A dedicated event-less method makes the
    /// "no event from this tx" intent explicit at the call site and
    /// keeps the role-gate machinery out of the path (no event = no
    /// gate to enforce; cache write-through still happens inside
    /// `card_create_with_id_tx` exactly as before).
    async fn write_in_tx(&self, f: WriteInTxFn<'_>) -> Result<()>;

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
    ///
    /// Each tuple is `(events.id, event_version, EventScope, Event)`. The
    /// `event_version` is the value persisted on the row (migration 0006);
    /// rows that predate the migration backfill to `1` via the column
    /// default. The `EventScope` is reconstructed from the `events.scope_*`
    /// columns added in migration 0007 — rows that predate it (NULL
    /// ancestor cols) load as [`EventScope::System`]. The replay path
    /// stamps both onto the `BroadcastEnvelope` so frame consumers see the
    /// version + scope the row was written under, not the kernel's current
    /// constants.
    async fn events_since(
        &self,
        since_id: i64,
        limit: Option<i64>,
    ) -> Result<Vec<(i64, u32, EventScope, Event)>>;

    /// Read only selected event kinds scoped to one wave. This is for
    /// projection tools that need a bounded audit-log slice, not a replay
    /// cursor: callers must pass the exact kind tags they need and the query
    /// filters on `scope_wave = ?`. When `since_id` is present, the SQL
    /// additionally filters on `events.id > since_id`.
    async fn events_for_wave(
        &self,
        wave_id: &str,
        kinds: &[&str],
        since_id: Option<i64>,
    ) -> Result<Vec<WaveEvent>>;

    /// Lowest live `events.id`, or `None` if the table is empty.
    ///
    /// Used by the WS handler to detect a `since` cursor that predates the
    /// retention horizon (after the operator turns on
    /// `events_retention_days`). When `since < earliest_id`, the server
    /// can't honor the replay; it must reply with a `_snapshot_required`
    /// control frame so the client throws away its cached state and
    /// refetches via REST (design §2.3).
    async fn events_earliest_id(&self) -> Result<Option<i64>>;

    /// Highest live `events.id`, or `None` if the table is empty.
    ///
    /// Used by the WS handler so `_replay_complete` can stamp the
    /// server's actual log tip — not just the highest id returned by
    /// the replay window — into the frame's `_id`. That lets a client
    /// whose persisted cursor is *ahead* of the server's tip (e.g. the
    /// dev `/dev/reset` path that wipes `sqlite_sequence`, so re-seeded
    /// events restart at id=1) detect "the server is no longer the
    /// kernel I was talking to" and re-bootstrap its cache. Issue #290.
    async fn events_latest_id(&self) -> Result<Option<i64>>;
}

/// Raw sync-domain entity writes. Gated: the trait object `RouteRepo` that
/// `AppState::repo` exposes does **not** carry this supertrait, so route
/// handlers cannot call these methods. They live on the trait so the
/// concrete `SqlxRepo` impl can be addressed by db-internal helpers, the
/// replay lib, and tests via `AppState::raw_repo()`.
///
/// Sync-domain == the per-user/per-AI co-edit shared state surface defined
/// by the sync engine: coves, waves, cards, overlays. Any direct write here
/// bypasses `write_with_event` and is therefore invisible to replicas — the
/// whole reason this surface is gated.
#[async_trait]
pub trait RepoSyncDomainRaw: RepoRead {
    // ---- coves
    async fn cove_create(&self, p: NewCove) -> Result<Cove>;
    async fn cove_update(&self, id: &str, p: CovePatch) -> Result<Cove>;
    async fn cove_delete(&self, id: &str) -> Result<()>;

    // ---- waves
    async fn wave_create(&self, p: NewWave) -> Result<Wave>;
    async fn wave_update(&self, id: &str, p: WavePatch) -> Result<Wave>;
    async fn wave_delete(&self, id: &str) -> Result<()>;

    // ---- cards
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
}

/// Out-of-sync-domain writes: terminal lifecycle, plugin install/config,
/// app-global settings. Deliberately **not** event-sourced — these are
/// server-private operational state, not shared co-edit state. Routes
/// see this surface (plugin install REST, settings PUT, the terminal
/// PID-persist sidecar).
#[async_trait]
pub trait RepoOutOfDomain: RepoRead {
    // ---- terminals (writes)
    async fn terminal_create(&self, p: NewTerminal) -> Result<Terminal>;
    /// Persist the child PID captured by the renderer/supervisor path. The
    /// orphan-terminal sweeper uses this as a SIGTERM fallback target.
    async fn terminal_set_pid(&self, id: &str, pid: Option<u32>) -> Result<()>;
    /// #306 — record the child's exit info. The two arguments are mutually
    /// exclusive at the writer: a signal-
    /// killed child writes `exit_code = None, signal_killed = true`, an
    /// `exit()` child writes `exit_code = Some(_), signal_killed = false`.
    /// Callers must respect that invariant; the repo enforces neither.
    async fn terminal_set_exit(
        &self,
        id: &str,
        exit_code: Option<i32>,
        signal_killed: bool,
    ) -> Result<()>;
    /// Clear stale PID and exit markers immediately before spawning or
    /// reattaching a terminal child. Fresh terminal rows are already clean;
    /// recovered rows may carry a previous PID plus boot reconciliation exit
    /// markers.
    async fn terminal_clear_exit_for_spawn(&self, id: &str) -> Result<()>;
    /// Remove a terminal row by id. Surfaced on the trait so the sweeper
    /// can call it from inside its `write_with_event` closure via the
    /// `_tx`-suffixed helper.
    async fn terminal_delete(&self, id: &str) -> Result<()>;

    async fn shared_daemon_runtime_set(&self, update: SharedCodexDaemonUpdate) -> Result<()>;
    async fn shared_daemon_record_event(&self, action: &str, error: Option<&str>) -> Result<()>;

    // ---- spec harness item stream (#510 PR-ui C1)
    #[allow(clippy::too_many_arguments)]
    async fn harness_item_insert(
        &self,
        runtime_id: &str,
        card_id: &str,
        wave_id: &str,
        thread_id: &str,
        turn_id: Option<&str>,
        item_uuid: Option<&str>,
        item_type: Option<&str>,
        method: &str,
        params: &str,
    ) -> Result<i64>;

    // ---- worker message-flow capture (#695 PR2) ------------------------
    /// Append one captured worker-flow item, returning the new row id.
    ///
    /// Sibling of [`harness_item_insert`](Self::harness_item_insert):
    /// `card_id` is nullable so the row can outlive its worker card (the FK is
    /// `ON DELETE SET NULL`, #695), while `worker_session_id` is a required
    /// `worker_sessions(id)` FK as of migration 0049. `kind` is the
    /// `WorkerFlowItem` discriminant and `payload` is its JSON-serialized form
    /// (+ envelope / provider_extra / raw_ref). The repo method delegates to the
    /// [`worker_flow_item_insert_tx`](sqlite::worker_flow_item_insert_tx)
    /// free fn so PR3's sink can call the same insert from inside
    /// `commit_decision`'s transaction.
    #[allow(clippy::too_many_arguments)]
    async fn worker_flow_item_insert(
        &self,
        card_id: Option<&str>,
        runtime_id: Option<&str>,
        wave_id: Option<&str>,
        worker_session_id: Option<&str>,
        kind: &str,
        payload: &str,
        created_at_ms: i64,
    ) -> Result<i64>;

    /// Upsert the passive worker-flow capture cursor for one card/source.
    ///
    /// `record_index` may move down when a rollout file is rewritten during
    /// compaction; callers validate the source identity before taking that
    /// reset path.
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

    // ---- plugins (writes)
    //
    // M3 (Slice A) surface: install / enable / get-by-id / delete / list-all.
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
    async fn plugin_token_delete(&self, plugin_id: &str) -> Result<()>;

    // ---- per-plugin KV store (Slice C surfaces to plugins via `neige.kv.*`;
    // Slice A owns the bare CRUD). Values are arbitrary JSON; the kernel
    // does not parse semantics, but it does enforce per-plugin namespacing
    // at this trait layer (no method takes a global key).
    async fn plugin_kv_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()>;
    async fn plugin_kv_delete(&self, plugin_id: &str, key: &str) -> Result<()>;

    // ---- app-global settings (Settings page, codex spawn proxy override).
    //
    // Tiny KV. `settings_upsert` is per-key INSERT OR REPLACE; an empty
    // string is treated as a delete on the *route* boundary (callers can
    // still upsert an empty value if they have a reason to).
    async fn settings_upsert(&self, key: &str, value: &str) -> Result<()>;
    async fn settings_delete(&self, key: &str) -> Result<()>;

    // ---- cove_folders (issue #250 PR 1)
    //
    // Operational mapping table — not on the event-sourced sync domain
    // path (no `Event::CoveFolderAdded`-style variants land in PR 1).
    // Treated like terminals / plugins: server-private state, REST writes
    // straight against the row without an event-log entry.
    /// Insert a folder under `cove_id`. The caller is responsible for
    /// path normalization + conflict detection — both run at the route
    /// layer (`routes::cove_folders::create_folder`) so the structured
    /// 409 body can be assembled with the conflicting row's metadata.
    async fn cove_folder_create(&self, cove_id: &str, path: &str) -> Result<CoveFolder>;
    /// Delete a folder by integer id. Returns `NotFound` when no row
    /// exists. PR 2 will add a "has live wave referencing this path"
    /// guard at the route layer; the repo primitive stays narrow.
    async fn cove_folder_delete(&self, id: i64) -> Result<()>;
}

/// Prelude module that re-exports every sub-trait + `Repo` itself so test
/// modules and internal code can do `use calm_server::db::prelude::*` to
/// bring all the method names into scope at once. Production code is
/// expected to import the *narrowest* trait it actually needs (it
/// reinforces the capability gate); the prelude exists because test code
/// regularly seeds entity rows via every kind of write.
pub mod prelude {
    pub use super::{
        Repo, RepoEventWrite, RepoOutOfDomain, RepoRead, RepoSyncDomainRaw, RouteRepo,
    };
    pub use crate::runtime_repo::RuntimeRepo;
    pub use crate::session_repo::SessionRepo;
}

/// The trait object route handlers actually see via `AppState::repo`.
/// Excludes [`RepoSyncDomainRaw`] — that's the gate. Reads come in via
/// the `RepoRead` supertrait, and the only writes reachable here are
/// the event-sourced ones plus the out-of-domain operational writes.
///
/// Implemented blanket for any type that combines the route-facing
/// supertraits, so `SqlxRepo` (and any future repo impl) picks it up
/// automatically.
pub trait RouteRepo: RepoEventWrite + RepoOutOfDomain + RuntimeRepo {}
impl<T> RouteRepo for T where T: RepoEventWrite + RepoOutOfDomain + RuntimeRepo + ?Sized {}

/// Full repo capability. Declared as a supertrait of [`RouteRepo`] +
/// [`RepoSyncDomainRaw`] so `Arc<dyn Repo>` upcasts to `Arc<dyn RouteRepo>`
/// via stable trait-object upcasting (Rust 1.86+). The blanket impl picks
/// up `SqlxRepo` automatically once it implements the required sub-traits.
///
/// `&dyn Repo` is the internal-access escape hatch used by db-internal
/// helpers, the replay lib, terminal_sweeper, and tests. Production route
/// handlers see the narrower [`RouteRepo`] trait object instead — see
/// `AppState::repo`.
pub trait Repo: RouteRepo + RepoSyncDomainRaw + RuntimeRepo + SessionRepo {
    /// Internal sqlite escape hatch for infrastructure that owns tables
    /// outside the route-facing sync-domain traits. Kept off `RouteRepo` so
    /// ordinary handlers cannot bypass the existing write gates.
    fn sqlite_pool(&self) -> Option<SqlitePool> {
        None
    }
}

// ---------------------------------------------------------------------------
// `write_with_event_typed` — ergonomic generic wrapper around the
// dyn-compatible trait method.
// ---------------------------------------------------------------------------

/// Generic convenience wrapper over `RepoEventWrite::write_with_event` for
/// callers who want to return a typed row to their REST / plugin-host
/// caller. The closure returns `(R, Event)`; we capture `R` in an outer
/// mutex so the trait method's `WriteWithEventFn` (which only knows about
/// `Event`) can stay dyn-compatible.
///
/// The `&dyn RepoEventWrite` bound (rather than `&dyn Repo`) is the
/// capability gate: a route handler whose `s.repo: Arc<dyn RouteRepo>`
/// upcasts cleanly here, because `RouteRepo` is itself a `RepoEventWrite`.
/// But the same handler can **not** reach raw sync-domain writes through
/// `s.repo` — those live on `RepoSyncDomainRaw`, which `RouteRepo` does
/// not extend.
///
/// This is *purely sugar* — the underlying invariants (single transaction,
/// commit-then-emit) come from the trait method.
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

/// PR6 (#136) — generic plural counterpart to
/// [`write_with_event_typed`]. Same `R`-capture trick (the trait
/// method's closure can't return a typed row directly without
/// breaking dyn-compatibility), now batched across multiple events.
///
/// The closure returns `(R, Vec<(EventScope, Event)>)` — one typed
/// row + one or more `(scope, event)` pairs. Each event is
/// independently authorized via `enforce_role` against the supplied
/// `actor`; any violation rolls the whole transaction back.
///
/// Use this when a single mutation must emit multiple events tagged
/// with different scopes — e.g. `routes::waves::create_wave`'s
/// atomic spec-card binding emits a wave-scoped `WaveUpdated` plus a
/// card-scoped `CardAdded` from the same tx so per-wave and per-card
/// subscribers each see the relevant frame at first hand. For the
/// usual single-event case stay on [`write_with_event_typed`].
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

/// #597 typed counterpart to [`RepoEventWrite::write_with_actor_events`].
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

/// Issue #310 — typed counterpart to [`RepoEventWrite::write_in_tx`].
/// Same `R`-capture trick as [`write_with_event_typed`]: the trait
/// method's closure returns `()` (no event, no row) so a typed row
/// has to be carried out via an `Arc<Mutex<Option<R>>>` set inside
/// the closure. The free function below does that capture so callers
/// can stay on the ergonomic `(R) → typed-R out` shape.
///
/// Used by the dispatcher to mint a worker card + terminal row in one tx
/// without broadcasting `CardAdded`; the post-spawn `log_pure_event(CardAdded)`
/// then carries the row to subscribers after the renderer is attachable.
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
