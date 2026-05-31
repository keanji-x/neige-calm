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
//! `Repo` is split into four sub-traits along the *capability* axis. The
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
//!
//! [`Repo`] is the marker that requires all four — implemented blanket
//! for any `T: RepoEventWrite + RepoSyncDomainRaw + RepoOutOfDomain`,
//! so `SqlxRepo` picks it up automatically once it implements the four
//! sub-traits.
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
use crate::ids::ActorId;
use crate::model::*;
use crate::wave_cove_cache::WaveCoveCache;
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
///   * The terminal-sweeper will NOT reap the orphan: its SQL excludes
///     terminals still referenced by a card (`terminals_orphaned` only
///     returns rows with no matching `cards.payload.terminal_id`), and
///     the card row IS pointing at this terminal.
///   * The `idempotency_key` short-circuits future retries — a user
///     who re-dispatches with the same key gets the silent-skip path
///     in `find_card_by_idempotency_key_tx`.
///
/// The dispatcher's `TaskFailed` emission only fires on a returned
/// error from a live spawn, not on a process death mid-spawn, so the
/// requesting spec card's `wait_for_events` loop never wakes up
/// either. Net effect: an undead card that nothing knows about.
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

    // ---- cards
    async fn cards_by_wave(&self, wave_id: &str) -> Result<Vec<Card>>;
    async fn card_get(&self, id: &str) -> Result<Option<Card>>;

    // ---- overlays
    async fn overlays_for(&self, entity_kind: &str, entity_id: &str) -> Result<Vec<Overlay>>;
    /// List every overlay attached to entities of the given `entity_kind`
    /// (e.g. `"wave"`), regardless of `entity_id`. Used by the sidebar so
    /// wave status indicators stay accurate without per-wave detail fetches.
    async fn overlays_by_kind(&self, entity_kind: &str) -> Result<Vec<Overlay>>;

    // ---- terminals (read-only)
    async fn terminal_get(&self, id: &str) -> Result<Option<Terminal>>;
    async fn terminal_get_by_card(&self, card_id: &str) -> Result<Option<Terminal>>;
    /// Return every terminal row that has no card pointing at it via
    /// `cards.payload.terminal_id`, and whose `created_at` is older than
    /// `grace_seconds` ago. The grace window absorbs the 3-step
    /// terminal-card create race (see `web/src/app/eventBridge.tsx:60-70`).
    /// Used exclusively by the `terminal_sweeper` background task.
    async fn terminals_orphaned(&self, grace_seconds: i64) -> Result<Vec<Terminal>>;
    /// Return every terminal row whose child has not recorded an exit yet.
    /// Used by boot-time supervisor reconciliation after #388 Phase 3b.
    async fn terminals_running(&self) -> Result<Vec<Terminal>>;

    /// #313 problem #1 (boot takeover) — return every spec card whose
    /// payload carries a `codex_thread_id` whose parent wave is not in a
    /// terminal lifecycle state (`done` / `canceled` / `failed`), as
    /// `(card_id, wave_id, codex_thread_id, appserver_pgid, appserver_sock,
    /// appserver_start_time, appserver_boot_id, push_watermark)`.
    ///
    /// `appserver_pgid` / `appserver_sock` / `appserver_start_time` /
    /// `appserver_boot_id` may be missing if a prior boot only persisted
    /// a subset (defensive — every code path that writes
    /// `codex_thread_id` today also writes the first two, and #318 added
    /// the start_time + boot_id identity stamp); `push_watermark`
    /// defaults to 0 when absent (waves persisted before #313 didn't have
    /// the field, and 0 means "replay every event for this wave on
    /// recovery" — the correct conservative default).
    ///
    /// `(appserver_start_time, appserver_boot_id)` is the identity stamp
    /// captured at spawn:
    ///   * `appserver_start_time` — field 22 (1-indexed) of
    ///     `/proc/<pid>/stat`, clock-ticks since boot.
    ///   * `appserver_boot_id` — `/proc/sys/kernel/random/boot_id`, a
    ///     per-boot UUID. Distinguishes "host rebooted, every prior pid
    ///     is dead" from "same boot, possible mid-boot pid recycle".
    ///
    /// The boot-recovery path verifies BOTH against the live `/proc`
    /// entries before signaling the persisted pgid (#318 INV-5 / R3-B1).
    ///
    /// Used exclusively by [`crate::takeover_spec_appservers_on_boot`] during
    /// startup to re-establish the push channel for in-flight waves (boot
    /// takeover replaces today's boot kill).
    async fn spec_cards_for_boot_takeover(
        &self,
    ) -> Result<
        Vec<(
            String,
            String,
            String,
            Option<i32>,
            Option<String>,
            Option<u64>,
            Option<String>,
            i64,
        )>,
    >;

    /// Empty-goal spec cards whose app-server created a thread but
    /// intentionally issued no initial `turn/start`. They cannot be
    /// recovered with `thread/resume` because no rollout exists yet; boot
    /// should spawn a fresh idle app-server and replace the runtime fields.
    async fn spec_cards_for_initial_prompt_bootstrap(
        &self,
    ) -> Result<
        Vec<(
            String,
            String,
            String,
            Option<i32>,
            Option<String>,
            Option<u64>,
            Option<String>,
            i64,
        )>,
    >;

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
    /// `card_role_cache` is the [`CardRoleCache`] used by PR3's
    /// `role_gate::enforce_role` to check that the actor is authorized
    /// to emit this event under the supplied scope. The gate runs
    /// *inside* the transaction, after the closure produces an event
    /// and before the event row is appended. Violations roll the txn
    /// back — entity write, event row, and broadcast all disappear.
    ///
    /// `wave_cove_cache` is the parallel [`WaveCoveCache`] the gate
    /// uses to cross-check `scope.cove` against the Worker card's
    /// home cove (#234). Same write-through invariant as the role
    /// cache; lives on a separate field because wave-count is much
    /// smaller than card-count and the two caches answer different
    /// questions.
    async fn write_with_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &crate::event::EventBus,
        card_role_cache: &CardRoleCache,
        wave_cove_cache: &WaveCoveCache,
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
        card_role_cache: &CardRoleCache,
        wave_cove_cache: &WaveCoveCache,
        f: WriteWithEventsFn<'_>,
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
    /// filters on `scope_wave = ?`.
    async fn events_for_wave(&self, wave_id: &str, kinds: &[&str]) -> Result<Vec<WaveEvent>>;

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

    /// #318 INV-1 (b) — largest `events.id` whose `scope_wave` matches
    /// the given wave, or `None` when no wave-scoped row exists for
    /// it.
    ///
    /// Used by [`crate::try_takeover_one_wave`] when the inert
    /// classifier fires: the helper stamps this value onto
    /// `Event::SpecPushAbandoned.last_envelope_id` so SRE / future
    /// re-run code sees an upper bound on the stranded set
    /// (`(push_watermark, last_envelope_id]` for this wave). Returning
    /// `None` is mapped to `0` at the call site — the same sentinel
    /// `events.id` reserves for "no row".
    ///
    /// Scope filter is `scope_wave = ?1` — `scope_kind` may be either
    /// `'wave'` or `'card'` (cards under this wave count too — they
    /// carry `scope_wave` per `EventScope::from_row`). Rows that
    /// predate migration 0007 default to `scope_kind='system'` with
    /// NULL ancestor cols and never appear in this query, which is
    /// correct: pre-PR2 rows have no wave attribution.
    async fn events_latest_id_for_wave(&self, wave_id: &str) -> Result<Option<i64>>;
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
    /// Remove a terminal row by id. Surfaced on the trait so the sweeper
    /// can call it from inside its `write_with_event` closure via the
    /// `_tx`-suffixed helper.
    async fn terminal_delete(&self, id: &str) -> Result<()>;

    /// #313 problem #1 (boot takeover) — persist a spec card's push
    /// watermark (`payload.push_watermark`) as a single-field merge,
    /// without emitting a `CardUpdated` event.
    ///
    /// The dispatcher's push path calls this on every push, right after
    /// bumping the in-memory [`crate::event_cursor::EventCursorCache`].
    /// Going through `write_with_event` would emit one `CardUpdated` per
    /// push — pure noise nothing subscribes to (the dispatcher's filter
    /// doesn't watch `CardUpdated`, and the field is server-private
    /// bookkeeping). Treating it like the terminal PID / handle /exit
    /// sidecars (which use this same trait for the same reason) keeps the
    /// hot path narrow.
    ///
    /// The write is a JSON merge so it never clobbers `codex_thread_id` /
    /// `appserver_sock` / `appserver_pgid` / other payload fields. A
    /// missing card row is a no-op (the wave was deleted between the bump
    /// and the persist).
    ///
    // TODO(runtime-state-table): push_watermark — along with appserver_pgid,
    // appserver_sock, codex_thread_id — is kernel-private runtime bookkeeping
    // living on the card payload via OutOfDomain (no CardUpdated event). When
    // the dedicated runtime-state table lands, migrate these fields out of
    // card payload into it. Acceptable short-term per #315 review.
    async fn spec_card_set_push_watermark(&self, card_id: &str, watermark: i64) -> Result<()>;

    /// Clear the empty-goal bootstrap marker once any observed turn
    /// lifecycle proves the codex side has created a resumable rollout.
    /// Idempotent and eventless: this is kernel-private runtime
    /// bookkeeping, same as `spec_card_set_push_watermark`.
    async fn spec_card_clear_needs_initial_prompt(&self, card_id: &str) -> Result<()>;

    /// #313 problem #1 — clear a spec card's `codex_thread_id` (and the
    /// related push fields) when boot takeover finds the persisted thread
    /// can no longer be resumed (`-32600 "no rollout found"` from
    /// `thread/resume`). Leaves the rest of the payload intact. The wave
    /// stays in its current lifecycle (matches today's "inert wave"
    /// posture — issue #313 problem #2 covers re-running these; not in
    /// this PR).
    async fn spec_card_clear_push_state(&self, card_id: &str) -> Result<()>;

    /// #313 problem #1 — after boot takeover RESPAWNS a fresh codex
    /// app-server for a spec card, persist the new launcher pgid + sock
    /// (+ #318 INV-5 `(start_time, boot_id)` identity stamp) so the NEXT
    /// boot cycle (or a graceful teardown) targets the right process.
    /// Single-statement JSON-merge; touches only `appserver_pgid` +
    /// `appserver_sock` + `appserver_start_time` + `appserver_boot_id`.
    /// Does NOT touch `codex_thread_id` or `push_watermark`.
    ///
    /// `start_time` / `boot_id` are `Option` because non-Linux targets /
    /// transient `/proc` read failures yield no stamp; in that case the
    /// field is removed from the payload (NULL), and boot-recovery
    /// conservatively skips the kill (same posture as a mismatch).
    async fn spec_card_set_appserver_after_takeover(
        &self,
        card_id: &str,
        pgid: i32,
        sock: &str,
        start_time: Option<u64>,
        boot_id: Option<&str>,
    ) -> Result<()>;

    /// Persist the fresh runtime state for an empty-goal bootstrap. Unlike
    /// normal takeover this replaces `codex_thread_id`, because the previous
    /// thread had no rollout and must never be resumed.
    #[allow(clippy::too_many_arguments)]
    async fn spec_card_set_empty_goal_bootstrap_state(
        &self,
        card_id: &str,
        thread_id: &str,
        pgid: i32,
        sock: &str,
        start_time: Option<u64>,
        boot_id: Option<&str>,
        watermark: i64,
    ) -> Result<()>;

    // ---- spec push queue (#318 INV-3 / R2-B1)
    //
    // Durable backing store for `spec_appserver::PushQueue`. Before this
    // surface, the queue lived only as `Arc<Mutex<VecDeque<…>>>` —
    // `push_observation` returning `Ok(Enqueued)` would lose the buffered
    // observation on a kernel crash before the next `turn/completed`
    // flush. The only thing that re-delivered it was the events-log
    // replay (gated by the dispatcher cooperatively withholding the
    // `push_watermark` on `Enqueued`, PR #315 PR4 B1) — incidental
    // durability that INV-3 says the queue should not lean on.
    //
    // These methods are server-private operational state (no event
    // emission, no sync-domain entry) — they live on `RepoOutOfDomain`
    // alongside `spec_card_set_push_watermark` for the same reason.
    //
    // TODO(runtime-state-table): along with `push_watermark`,
    // `appserver_pgid`, `appserver_sock`, `codex_thread_id`, this is
    // kernel-private runtime bookkeeping. When the dedicated runtime-
    // state table lands, `spec_push_queue` can move into the same
    // surface (or stay separate — it's row-shaped, not card-payload
    // shaped, so a separate table is already correct).

    /// #318 INV-3 — persist one observation onto the durable spec push
    /// queue for `card_id`. Called from `SpecPusher::push_observation`'s
    /// `Enqueue` arm BEFORE the in-memory `VecDeque::push_back` and
    /// BEFORE returning `Ok(PushOutcome::Enqueued)`. Returns the row id
    /// (`spec_push_queue.id`) so the consumer task's `flush_push_queue`
    /// can `spec_card_dequeue_observations(&[id, …])` the right rows
    /// after a successful coalesced `turn/start`.
    ///
    /// `envelope_id` is the `events.id` from the originating push; the
    /// flush path reports `max(envelope_id)` back to the dispatcher via
    /// the `WatermarkSink` callback so the durable `push_watermark`
    /// advances past every item in the flushed turn (#313 B1).
    async fn spec_card_enqueue_observation(
        &self,
        card_id: &str,
        envelope_id: i64,
        text: &str,
    ) -> Result<i64>;

    /// #318 INV-3 — read every pending row for `card_id` in id order,
    /// as `(row_id, envelope_id, text)`. Used by boot-takeover's
    /// `register_and_catch_up` to rehydrate the in-memory queue from
    /// disk before catch-up replay starts (so any observation a prior
    /// process enqueued but didn't flush is re-delivered on the next
    /// `turn/completed`).
    async fn spec_card_queued_observations(&self, card_id: &str)
    -> Result<Vec<(i64, i64, String)>>;

    /// #318 INV-3 — delete the named queue rows. Called by
    /// `flush_push_queue` (and the `StartTurnNow` winner's drain) AFTER
    /// the coalesced `turn/start` resolves successfully. On `turn/start`
    /// failure the caller does NOT call this — the rows remain so a
    /// later flush (or the next boot's replay) retries.
    ///
    /// A batch delete keeps the flush hot path one round-trip; an empty
    /// `ids` slice is a no-op.
    async fn spec_card_dequeue_observations(&self, ids: &[i64]) -> Result<()>;

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
}

/// The trait object route handlers actually see via `AppState::repo`.
/// Excludes [`RepoSyncDomainRaw`] — that's the gate. Reads come in via
/// the `RepoRead` supertrait, and the only writes reachable here are
/// the event-sourced ones plus the out-of-domain operational writes.
///
/// Implemented blanket for any type that combines the two supertraits,
/// so `SqlxRepo` (and any future repo impl) picks it up automatically.
pub trait RouteRepo: RepoEventWrite + RepoOutOfDomain {}
impl<T> RouteRepo for T where T: RepoEventWrite + RepoOutOfDomain + ?Sized {}

/// Full repo capability. Declared as a supertrait of [`RouteRepo`] +
/// [`RepoSyncDomainRaw`] so `Arc<dyn Repo>` upcasts to `Arc<dyn RouteRepo>`
/// via stable trait-object upcasting (Rust 1.86+). The blanket impl picks
/// up `SqlxRepo` automatically once it implements all four sub-traits.
///
/// `&dyn Repo` is the internal-access escape hatch used by db-internal
/// helpers, the replay lib, terminal_sweeper, and tests. Production route
/// handlers see the narrower [`RouteRepo`] trait object instead — see
/// `AppState::repo`.
pub trait Repo: RouteRepo + RepoSyncDomainRaw {}
impl<T> Repo for T where T: RouteRepo + RepoSyncDomainRaw + ?Sized {}

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
    card_role_cache: &CardRoleCache,
    wave_cove_cache: &WaveCoveCache,
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
        .write_with_event(
            actor,
            scope,
            correlation,
            bus,
            card_role_cache,
            wave_cove_cache,
            boxed,
        )
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
    card_role_cache: &CardRoleCache,
    wave_cove_cache: &WaveCoveCache,
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
        .write_with_events(
            actor,
            correlation,
            bus,
            card_role_cache,
            wave_cove_cache,
            boxed,
        )
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
