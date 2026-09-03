//! SQLite-backed `Repo` implementation. **Owned by Track A.**
//!
//! Implements every method on the `Repo` trait against a `sqlx::SqlitePool`.
//! The pool is opened with `PRAGMA foreign_keys = ON` per-connection, the
//! bundled migrations under `migrations/` are run on `open()`, and every
//! observable behavior of `MockRepo` (cascades, sort defaulting, not-found
//! semantics, overlay upsert by unique key) is replicated here.
//!
//! ## Sync engine — internal layout
//!
//! Every entity write the trait exposes (`area_create`, `track_update`,
//! `card_create`, ...) is implemented as a thin wrapper around a `_tx`-
//! suffixed free function that takes `&mut Transaction<'_, Sqlite>` and
//! does the actual SQL. The wrappers each open their own one-shot
//! transaction (the existing single-call semantics), but the `_tx`
//! functions can also be **composed inside** `Repo::write_with_event`'s
//! closure so the entity write and the `INSERT INTO events ...` run in
//! the same transaction. See `db::mod`'s sync-engine comment.

use sqlx::ConnectOptions;
use sqlx::Connection;
use sqlx::Executor;
use sqlx::SqlitePool;
use sqlx::TransactionManager as _;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteConnection, SqlitePoolOptions, SqliteTransactionManager,
};
use std::str::FromStr;
use std::time::Duration;

use super::Repo;
use crate::card_role_cache::CardRoleCache;
use crate::error::{CalmError, Result};
use crate::track_area_cache::TrackAreaCache;
use crate::track_vcs;
use calm_types::model::AreaFolder;

/// Per-connection SQLite busy-handler budget installed by [`SqlxRepo::open`].
pub const SQLITE_BUSY_TIMEOUT_MS: u64 = 5_000;

/// Pool-acquisition budget installed by [`SqlxRepo::open`].
///
/// This deliberately equals sqlx 0.8.6's 30 s default, so making it explicit
/// changes no behavior today; owning the value lets our composition gates pin
/// it and prevents a future sqlx default change from silently invalidating
/// those bounds.
pub const SQLITE_ACQUIRE_TIMEOUT_MS: u64 = 30_000;

// ---------------------------------------------------------------------------
// Sub-trait impls — thin pool-wrapping wrappers around the `_tx` helpers,
// plus the read-side methods that don't need transaction composition.
//
// `Repo` (and `RouteRepo`) are picked up via the blanket impls in `db/mod`
// once all four sub-traits are implemented.
// ---------------------------------------------------------------------------

mod area;
mod card;
mod card_composite;
mod events;
mod infra;
mod out_of_domain;
mod overlay;
mod read;
mod session_mirror;
mod session_projection;
mod session_repo_impl;
mod session_row;
mod task;
mod task_projection;
mod track;
mod track_recipe;
mod track_tree;
mod track_workspace;

pub use area::{
    area_create_system_tx, area_create_tx, area_delete_tx, area_folder_create_tx,
    area_folders_list_all_tx, area_update_tx,
};
pub use card::{
    card_body_crdt_get_tx, card_create_tx, card_create_with_id_tx, card_delete_tx, card_update_tx,
    card_update_with_crdt_tx, terminal_create_tx, terminal_delete_tx, terminal_get_by_card_tx,
};
pub use card_composite::{
    card_mcp_token_set_tx, card_with_claude_create_tx, card_with_claude_worker_create_tx,
    card_with_codex_create_tx, card_with_terminal_create_tx, card_with_terminal_rollback_tx,
};
pub use events::{append_decision_event_in_tx, append_decision_events_in_tx};
pub use infra::{begin_immediate_tx, is_sqlite_busy};
pub use out_of_domain::{
    HarnessTranscriptMeasure, harness_items_delete_by_card_tx, harness_items_measure_by_card_tx,
    worker_flow_item_insert_tx, worker_flow_items_delete_by_card_tx,
};
pub use overlay::{
    overlay_delete_by_entity_tx, overlay_delete_card_overlays_by_track_tx,
    overlay_delete_subtree_by_area_tx, overlay_delete_tx, overlay_upsert_tx,
    track_has_template_overlay_tx,
};
pub use session_mirror::{
    session_delete_tx, session_prepare_deferred_spec_tx, session_start_runtime_tx,
    session_supersede_active_tx, session_supersede_and_start_tx,
};
pub use session_projection::{
    session_bind_attribution_tx, session_clear_terminal_run_id_tx, session_complete_for_card_tx,
    session_complete_for_terminal_tx, session_complete_tx, session_fail_if_active_runtime_tx,
    session_mark_superseded_runtime_tx, session_projection_active_for_card_tx,
    session_projection_active_for_terminal_tx, session_projection_by_id_tx,
    session_restore_from_superseded_runtime_tx, session_set_active_turn_tx,
    session_set_handle_state_tx, session_set_harness_observation_runtime_tx,
    session_set_status_for_card_tx, session_set_status_tx,
};
pub(crate) use session_row::{derive_session_identity, worker_session_from_row};
pub use session_row::{
    session_commit_exit_tx, session_get_by_active_token_hash, session_get_by_id, session_get_tx,
    session_insert_tx, session_mark_track_root_tx, session_mcp_token_set_tx,
    session_record_activity_by_thread_tx, session_record_activity_tx, session_set_liveness_tx,
    session_state_transition_tx, worker_session_status_transition_allowed,
};
pub use task::{
    SuccessReportFlip, TaskReporter, require_track_exists_tx, status_detail_class,
    status_detail_with_reason, task_apply_gate_result_tx, task_cancel_tx, task_claim_pending_tx,
    task_complete_from_worker_tx, task_fail_from_worker_tx, task_gate_attempt_bump_tx, task_get_tx,
    task_mark_running_tx, task_mark_sub_track_running_tx, task_report_success_from_worker_tx,
    task_stamp_missing_running_deadline_tx, task_start_verifying_from_worker_tx,
    task_update_pending_tx, tasks_by_track_tx, track_lifecycle_and_budget_tx,
    track_require_task_gates_tx, worker_op_targets_card_tx,
};
pub use task_projection::{
    BlockVerdict, PROJECTION_DRIFT_TASK_FIELDS, TaskProjectionOutcome, WithdrawalEdge,
    evaluate_schedulability, mark_context_material_tx, project_tasks_tx,
    project_tasks_with_tree_term_tx, task_delete_pending_tx,
};
pub use track::{
    AttachedInheritedPath, TrackWorkspacePlan, track_create_tx, track_delete_tx,
    track_require_leaf_tx, track_update_tx,
};
pub use track_tree::{
    DEFAULT_TREE_TASK_BUDGET, MAX_TRACK_TREE_DEPTH, MAX_TREE_TASK_BUDGET, TRACK_BOUNDED_PATH_SQL,
    TRACK_ROOT_DEPTH_SQL, TRACK_TREE_MEMBERS_SQL, TRACK_TREE_SPEC_INVENTORY_SQL, TrackTreeTerm,
    TrackTreeTermOutcome, TreeShare, can_add_tree_member, deterministic_share, track_tree_budget,
    track_tree_member_count, track_tree_spec_inventory, track_tree_spec_inventory_by_member,
    track_tree_term,
};
pub use track_workspace::{
    track_workspace_freeze_tx, track_workspace_read_tx, track_workspace_write_tx,
};

use infra::check_no_unknown_future_migrations;

pub struct SqlxRepo {
    pool: SqlitePool,
    /// PR3 (#136) — write-through role cache local to the repo so the
    /// gated `RepoSyncDomainRaw` trait methods (`card_create` /
    /// `card_delete`) can call the `_tx` helpers without every test
    /// fixture having to hand a cache in. Production writes go through
    /// `AppState::card_role_cache` — a separate `Arc<DashMap<…>>`
    /// instance also kept in sync via the `_tx` helpers when the
    /// production `write_with_event` path runs. Both caches converge
    /// on whatever the `cards` table holds, since `seed_from_db`
    /// fully repopulates from sqlite. The duplication is intentional:
    /// `enforce_role` only ever consults the cache passed in at the
    /// call site, so AppState's view stays authoritative for
    /// production while the repo-local view backs the test-only raw
    /// path.
    card_role_cache: CardRoleCache,
    /// #234 — write-through `TrackId -> AreaId` cache, same rationale as
    /// `card_role_cache` above: the raw `RepoSyncDomainRaw` track write
    /// paths (`track_create` / `track_delete`) keep this in sync via the
    /// `_tx` helpers, while production `write_with_event` callers thread
    /// `AppState::track_area_cache` (a separate instance that
    /// `AppState::new` seeds from the same pool). Both converge on
    /// the persisted `tracks` table.
    track_area_cache: TrackAreaCache,
    /// #926 — process-lifetime keepalive for in-memory databases.
    ///
    /// sqlx maps `sqlite::memory:` / `mode=memory` URLs to a NAMED
    /// shared-cache database (`file:sqlx-in-memory-{seqno}?cache=shared`,
    /// seqno fixed per parsed `SqliteConnectOptions`); the cache — i.e.
    /// the entire database — lives only while at least one connection
    /// holds it. The `SqlitePoolOptions` fields `idle_timeout`,
    /// `max_lifetime`, and `min_connections` retain their defaults, so every
    /// POOL connection churns: the reaper closes connections idle > 600 s,
    /// `max_lifetime` (1800 s) hits the same-age connections together,
    /// and error paths `close_hard` — including the #920 `after_release`
    /// hook's fail-closed branch. If the pool's LAST connection closed,
    /// the database would be destroyed: the next acquire attaches a fresh
    /// EMPTY cache of the same name, migrations do NOT re-run, and every
    /// query fails "no such table" until process restart.
    ///
    /// This connection is acquired in `open()` — before migrations, so
    /// the cache provably cannot die between any two later steps — and
    /// `detach()`ed from the pool (the pool opens replacements as needed;
    /// capacity is unaffected). It is never used for queries: it exists
    /// solely to keep the cache alive so any pool churn is harmless.
    /// `None` for on-disk databases, whose reopen is lossless.
    ///
    /// In-memory detection asks the ENGINE, not the URL: `open()` probes
    /// `pragma_database_list` on the candidate connection — sqlite
    /// reports an empty `file` for in-memory and per-connection
    /// temp-file databases and the absolute path for on-disk ones
    /// (decades-stable behavior) — so detection is immune to URL
    /// spellings (e.g. percent-encoded params) and to future sqlx parse
    /// changes. Temp-file databases thus also get an anchor: useless
    /// (each connection has its own private temp DB, nothing shared to
    /// keep alive) but harmless. `cache=private` in-memory URLs are
    /// unsupported-by-construction at the sqlx level — every pool
    /// connection gets its OWN private empty database, anchor or not —
    /// the probe still anchors them (their `file` is empty too),
    /// equally useless and harmless.
    ///
    /// Dropped with the repo (never leaked): dropping ends the
    /// connection's worker thread and closes the sqlite handle, so tests
    /// building many repos don't accumulate threads.
    ///
    /// `SqliteConnection` is `Send + Sync` (all work is proxied to its
    /// worker thread over channels; compile-time assert in
    /// `pool_memory_anchor_tests`), so `SqlxRepo` stays shareable as
    /// `Arc<SqlxRepo>` with no lock around this field.
    _memory_cache_anchor: Option<SqliteConnection>,
}

impl SqlxRepo {
    /// Open / create the SQLite DB at `url`, run pending migrations, and
    /// enable foreign-key enforcement per-connection.
    ///
    /// Accepts both `sqlite::memory:` (used in tests) and on-disk
    /// `sqlite://path?mode=rwc` URLs. In-memory opens — detected by
    /// probing `pragma_database_list` on a live connection, not by
    /// parsing the URL — additionally pin the shared cache with a
    /// pool-external keepalive connection so pool churn can never
    /// destroy the database (#926 — see the `_memory_cache_anchor`
    /// field docs).
    pub async fn open(url: &str) -> Result<Self> {
        let mut opts = SqliteConnectOptions::from_str(url)
            .map_err(|e| CalmError::Internal(format!("invalid sqlite url {url:?}: {e}")))?
            .create_if_missing(true)
            .foreign_keys(true);
        // Reduce noise from sqlx's per-statement logging at info; keep debug.
        opts = opts.log_statements(tracing::log::LevelFilter::Debug);

        let pool = SqlitePoolOptions::new()
            .acquire_timeout(Duration::from_millis(SQLITE_ACQUIRE_TIMEOUT_MS))
            // Belt-and-braces: also re-issue the pragmas on every fresh
            // connection in case connect options are silently dropped for
            // some URL forms (e.g. memory).
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    conn.execute("PRAGMA foreign_keys = ON;").await?;
                    let busy_timeout_pragma =
                        format!("PRAGMA busy_timeout = {SQLITE_BUSY_TIMEOUT_MS};");
                    conn.execute(busy_timeout_pragma.as_str()).await?;
                    conn.execute("PRAGMA journal_mode = WAL;").await?;
                    Ok(())
                })
            })
            // #920 — self-heal connections released while still inside a
            // transaction. sqlx 0.8's `begin_with` has two awaits: the
            // worker executes BEGIN and bumps its transaction-depth
            // counter, then a second await verifies the state. A caller
            // future cancelled between them (e.g. an axum handler dropped
            // on client abort) leaks the open transaction: no
            // `Transaction` guard exists to roll it back, and the pool's
            // release path only pings. Every later `begin_with` on the
            // poisoned connection then fails at non-zero depth, plain
            // `begin()` silently nests a SAVEPOINT whose "commits" never
            // commit, and on shared-cache `sqlite::memory:` DBs every
            // OTHER connection's `BEGIN IMMEDIATE` parks in sqlite's
            // unlock_notify behind the leaked write lock. Repair here —
            // rolling back rather than discarding, because for in-memory
            // DBs dropping the connection can drop the database.
            .after_release(|conn, _meta| {
                Box::pin(async move {
                    if !Connection::is_in_transaction(conn) {
                        return Ok(true);
                    }
                    // A `Transaction` dropped on a normal error path has
                    // already queued its rollback on the worker's FIFO
                    // command queue; the depth counter only falls when the
                    // worker dequeues it. Round-trip a ping (same queue) so
                    // a pending drop-rollback completes before we judge the
                    // connection actually leaked.
                    Connection::ping(&mut *conn).await?;
                    if !Connection::is_in_transaction(conn) {
                        return Ok(true);
                    }
                    tracing::warn!(
                        "sqlite: connection released to pool inside an open transaction \
                         (cancelled begin?); rolling it back"
                    );
                    // Bounded unwind: one rollback per depth level covers
                    // nested savepoints without risking an infinite loop.
                    for _ in 0..8 {
                        SqliteTransactionManager::rollback(conn).await?;
                        if !Connection::is_in_transaction(conn) {
                            return Ok(true);
                        }
                    }
                    // Fail closed — still inside a transaction after the
                    // bounded unwind; the Err makes the pool close_hard the
                    // connection: one whose ROLLBACK fails (or whose depth
                    // counter has desynced from sqlite's real state) can
                    // never serve `begin_with` again, and recirculating it
                    // would re-poison the pool. The close_hard is safe even
                    // for in-memory repos: the `_memory_cache_anchor` keeps
                    // the shared cache alive, and the pool's replacement
                    // connections re-attach to it (see the field docs).
                    Err(sqlx::Error::Protocol(
                        "connection still inside a transaction after bounded rollback".into(),
                    ))
                })
            })
            .connect_with(opts)
            .await?;

        // #926 — for in-memory DBs, anchor the shared cache with one
        // pool-external connection BEFORE anything else touches the pool,
        // so the database provably cannot vanish between any two later
        // steps (migration check, migrations, backfill, cache seeding —
        // or any pool churn for the rest of the process lifetime). See
        // the `_memory_cache_anchor` field docs for the full mechanism.
        // Detection is an engine probe, not URL parsing:
        // `pragma_database_list.file` is empty for in-memory (and
        // temp-file) databases and the absolute path for on-disk ones.
        let mut candidate = pool.acquire().await?;
        let main_db_file: String =
            sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name = 'main'")
                .fetch_one(&mut *candidate)
                .await?;
        let memory_cache_anchor = if main_db_file.is_empty() {
            // In-memory (or temp-file) DB: pin the cache.
            Some(candidate.detach())
        } else {
            // On-disk: hand the connection back to the pool, no anchor.
            drop(candidate);
            None
        };

        // Tier-A upgrade stability boundary (`docs/upgrade-stability.md`):
        // refuse to boot when the DB carries a migration row that this
        // binary doesn't know about. Downgrade is unsupported — an older
        // binary opening a newer DB must fail loudly here rather than
        // continue against a schema it can't reason about. sqlx 0.8.x's
        // own `run()` would also refuse (via `MigrateError::VersionMissing`
        // unless `set_ignore_missing(true)` is set), but we check first so
        // (a) the error message wording is owned by us, not sqlx, and (b)
        // sqlx never gets a chance to apply any pending known migration
        // before we've rejected the open.
        check_no_unknown_future_migrations(&pool, &crate::MIGRATOR).await?;

        crate::MIGRATOR
            .run(&pool)
            .await
            .map_err(|e| CalmError::Internal(format!("migrate: {e}")))?;

        track_vcs::backfill_existing_tracks(&pool).await?;

        // PR3 (#136): seed the repo-local role cache from the freshly-
        // migrated table. This is the backing store for the gated raw
        // path's `card_create_tx` / `card_delete_tx` calls; the
        // production write path uses `AppState::card_role_cache`,
        // which `AppState::new` re-seeds from the same pool.
        let card_role_cache = CardRoleCache::new();
        card_role_cache.seed_from_db(&pool).await?;
        let track_area_cache = TrackAreaCache::new();
        track_area_cache.seed_from_db(&pool).await?;

        Ok(Self {
            pool,
            card_role_cache,
            track_area_cache,
            _memory_cache_anchor: memory_cache_anchor,
        })
    }

    /// #926 — test-only visibility: whether this repo holds the in-memory
    /// keepalive anchor. In-memory repos must; on-disk repos must not.
    #[cfg(test)]
    pub(crate) fn has_memory_cache_anchor(&self) -> bool {
        self._memory_cache_anchor.is_some()
    }

    /// Direct access to the pool for tests / fixtures / sync-engine
    /// integration tests that need to `SELECT` from the `events` table
    /// outside the `Repo` trait surface.
    ///
    /// Marked `#[doc(hidden)]` because production code must go through
    /// the trait (so a future swap to a non-sqlite backend stays
    /// possible). Integration tests under `tests/` need real access for
    /// replay / atomicity assertions; that's what this surface is for.
    #[doc(hidden)]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// PR3 (#136) — borrow the repo's role cache. `AppState::new` clones
    /// this into its own field so the production write path's `enforce_role`
    /// lookup sees the same map as the repo's `_tx` write-through.
    /// `CardRoleCache: Clone` is cheap (`Arc<DashMap<…>>` under the hood).
    pub fn card_role_cache(&self) -> &CardRoleCache {
        &self.card_role_cache
    }

    /// #234 — borrow the repo's track→area cache. Mirrors
    /// [`card_role_cache`](Self::card_role_cache). `AppState::new`
    /// re-seeds its own clone from the same pool.
    pub fn track_area_cache(&self) -> &TrackAreaCache {
        &self.track_area_cache
    }
}

pub async fn assert_worker_sessions_card_id_complete(pool: &SqlitePool) -> Result<()> {
    let count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM worker_sessions
            WHERE card_id IS NULL
              AND state IN ('starting','running','idle','turn_pending')"#,
    )
    .fetch_one(pool)
    .await?;

    if count > 0 {
        return Err(CalmError::Internal(format!(
            "worker_sessions.card_id boot assertion failed: {count} active worker_sessions rows have NULL card_id"
        )));
    }

    Ok(())
}

/// Boot fence: refuse to serve when `area_folders` holds two rows where
/// one is an ancestor of (or equal to) the other.
///
/// **Why a fence and not a repair.** #275 made overlap unreachable going
/// forward — every writer now classifies and inserts inside one
/// `BEGIN IMMEDIATE` transaction — and
/// [`crate::area_folder_claim::find_owner`] leans on that: it takes the
/// *first* matching row instead of the longest-prefix match, because at
/// most one row can match. Databases written before that landed can
/// still carry overlap (the track-attach path's in-tx insert was gated on
/// the request flag, never on the scan result). On such a table
/// `find_owner` answers `/a` where the old longest-prefix rule answered
/// `/a/b`, i.e. it silently re-owns a real user directory — and every
/// automatic repair has the same defect in a different place: deleting
/// the ancestor or the descendant also silently re-owns one. So the
/// ambiguity is refused, not guessed at, and a human decides which claim
/// is the real one.
///
/// **Why boot-fatal rather than degraded.** This mirrors
/// [`assert_worker_sessions_card_id_complete`]: an unresolvable truth
/// table stops the process before it serves a request. Refusing to boot
/// costs the operator nothing they need — the fix is a `DELETE` against
/// `area_folders` via sqlite3 or the admin CLI, neither of which needs
/// calm-server running — whereas booting anyway would route tracks and
/// `GET /api/areas/resolve` to an arbitrarily-chosen area for as long as
/// nobody notices.
///
/// The error names every offending pair (`id`, `area_id`, `path` on both
/// sides) so the operator can act on it without re-deriving the overlap
/// set by hand.
pub async fn assert_area_folders_disjoint(pool: &SqlitePool) -> Result<()> {
    let rows = sqlx::query_as::<_, crate::db::rows::AreaFolderRow>(
        r#"SELECT id, area_id, path, created_at
           FROM area_folders ORDER BY path ASC"#,
    )
    .fetch_all(pool)
    .await?;
    let folders: Vec<AreaFolder> = rows.into_iter().map(AreaFolder::from).collect();

    let pairs = crate::area_folder_claim::overlapping_pairs(&folders);
    if pairs.is_empty() {
        return Ok(());
    }

    let detail = pairs
        .iter()
        .map(|(row, conflict)| {
            format!(
                "id={} area_id={} path=`{}` {:?}-of id={} area_id={} path=`{}`",
                row.id,
                row.area_id.as_str(),
                row.path,
                conflict.conflict_kind,
                conflict.folder_id,
                conflict.area_id.as_str(),
                conflict.conflict_path,
            )
        })
        .collect::<Vec<_>>()
        .join("; ");

    Err(CalmError::Internal(format!(
        "area_folders boot fence failed: {} overlapping claim pair(s) — no single area owns \
         these paths, so folder resolution would silently pick an arbitrary winner. Delete the \
         wrong claim(s) from `area_folders` (sqlite3 / admin CLI) and restart. Offending pairs: {}",
        pairs.len(),
        detail
    )))
}

impl Repo for SqlxRepo {
    fn sqlite_pool(&self) -> Option<SqlitePool> {
        Some(self.pool.clone())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod sub_track_tree_tests;
#[cfg(test)]
mod task_context_migration_tests;
#[cfg(test)]
mod task_liveness_deadline_tests;
#[cfg(test)]
mod track_tree_budget_tests;

#[cfg(test)]
mod workspace_lease_lookup_tests;

#[cfg(test)]
mod write_path_gate_wiring_tests;

#[cfg(test)]
mod runtime_read_flip_parity_tests;
#[cfg(test)]
mod runtime_read_flip_projection_tests;
#[cfg(test)]
mod runtime_read_flip_support;

#[cfg(test)]
mod worker_flow_items_tests;

#[cfg(test)]
mod worker_flow_cursor_tests;

#[cfg(test)]
mod session_record_activity_tests;

#[cfg(test)]
mod track_template_input_tests;

#[cfg(test)]
mod track_plugin_scope_migration_tests;

#[cfg(test)]
mod track_workspace_migration_tests;

#[cfg(test)]
mod track_template_rename_migration_tests;

#[cfg(test)]
mod pool_tx_repair_tests;

// #930 — pins the upstream shared-cache deadlock semantics (unlock_notify
// registration order, autocommit unwind, retry shape, #920-hook interplay)
// that the "writing transactions always BEGIN IMMEDIATE" rule rests on.
#[cfg(test)]
mod deadlock_semantics_tests;

#[cfg(test)]
mod pool_memory_anchor_tests;

#[cfg(test)]
mod proposal_withdraw_upgrade_tests;

// #1016 — `track_detail` ships `cards` / `overlays` as
// `json_group_array(json_object(…))`. The shape file pins what that buys
// (escaping, NULL, bool, empty group, and the fact that a corrupt `payload`
// errors instead of becoming card structure); the precision file pins
// `cards.sort`, a REAL that `json_object` renders with only 15 significant
// digits unless it goes through `printf('%!.17g', …)`.
#[cfg(test)]
mod track_detail_json_shape_tests;
#[cfg(test)]
mod track_detail_sort_precision_tests;
// …and the order file pins what the aggregate does NOT give for free: its
// input order is arbitrary, so both arrays state their own.
#[cfg(test)]
mod track_detail_order_tests;
