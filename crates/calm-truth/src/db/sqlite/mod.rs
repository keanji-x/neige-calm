//! SQLite-backed `Repo` implementation. Every entity write is a `_tx` free
//! function so it can compose inside `write_with_event`'s transaction.

use sqlx::ConnectOptions;
use sqlx::Connection;
use sqlx::Executor;
use sqlx::SqlitePool;
use sqlx::TransactionManager as _;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteConnection, SqlitePoolOptions, SqliteTransactionManager,
};
use std::str::FromStr;
use std::sync::Arc;
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
pub const SQLITE_ACQUIRE_TIMEOUT_MS: u64 = 30_000;

mod area;
mod card;
mod card_composite;
mod database_identity;
mod events;
mod infra;
mod out_of_domain;
mod overlay;
mod read;
mod session_mirror;
mod session_projection;
mod session_repo_impl;
mod session_row;
mod session_system_error_recovery;
pub use session_system_error_recovery::{
    session_resume_system_error_tx, session_set_failed_harness_snapshot_tx,
    session_system_error_recovery_matches, session_system_error_recovery_matches_tx,
};
mod task;
mod task_attempt;
#[cfg(test)]
mod task_attempt_migration_tests;
#[cfg(test)]
mod task_attempt_tests;
mod task_projection;
mod task_recovery_projection;
mod track;
mod track_claude_permissions;
#[cfg(test)]
mod track_claude_permissions_tests;
mod track_recipe;
mod track_tree;
mod track_workspace;

pub use area::{
    area_create_bind_tx, area_create_replay_tx, area_create_system_tx, area_create_tx,
    area_delete_tx, area_folder_create_tx, area_folders_list_all_tx, area_update_tx,
};
pub use card::{
    CardExecutionShape, card_body_crdt_get_tx, card_create_tx, card_create_with_id_tx,
    card_delete_tx, card_execution_shape_tx, card_update_tx, card_update_with_crdt_tx,
    terminal_create_tx, terminal_delete_tx, terminal_get_by_card_tx,
};
pub use card_composite::{
    card_mcp_token_set_tx, card_stamp_claude_permissions_tx, card_with_claude_create_tx,
    card_with_claude_worker_create_tx, card_with_codex_create_tx, card_with_terminal_create_tx,
    card_with_terminal_rollback_tx,
};
#[cfg(any(test, feature = "test-helpers"))]
pub use events::append_probe;
pub use events::{append_decision_event_in_tx, append_decision_events_in_tx};
pub use infra::{begin_immediate_tx, is_sqlite_busy};
pub use out_of_domain::{
    HarnessTranscriptMeasure, harness_items_delete_by_card_tx, harness_items_measure_by_card_tx,
    worker_flow_item_insert_tx, worker_flow_items_delete_by_card_tx,
};
pub use overlay::{
    overlay_delete_by_entity_tx, overlay_delete_card_overlays_by_track_tx,
    overlay_delete_subtree_by_area_tx, overlay_delete_tx, overlay_upsert_tx,
};
pub use session_mirror::{
    session_delete_tx, session_prepare_deferred_planner_tx, session_start_runtime_tx,
    session_supersede_active_tx, session_supersede_and_start_tx,
};
pub use session_projection::{
    HarvestOutcome, HarvestedFrom, HarvestedMessage, HarvestedQueues,
    harvest_pending_user_messages_tx, session_bind_attribution_tx,
    session_clear_queue_harvested_tx, session_clear_terminal_run_id_tx,
    session_complete_for_card_tx, session_complete_for_terminal_tx, session_complete_tx,
    session_fail_if_active_runtime_tx, session_handle_state_by_id_tx,
    session_mark_queue_harvested_tx, session_mark_superseded_runtime_tx,
    session_projection_active_for_card_tx, session_projection_active_for_terminal_tx,
    session_projection_by_id_tx, session_restore_from_superseded_runtime_tx,
    session_set_active_turn_tx, session_set_handle_state_of_any_runtime_tx,
    session_set_handle_state_of_retired_runtime_tx, session_set_handle_state_tx,
    session_set_harness_observation_runtime_tx, session_set_status_for_card_tx,
    session_set_status_tx,
};
pub use session_row::{
    ClaudePlannerScope, claude_planner_revoke_tx, session_commit_exit_tx,
    session_get_by_active_token_hash, session_get_by_id, session_get_tx, session_insert_tx,
    session_mark_track_root_tx, session_mcp_token_set_if_active_tx, session_mcp_token_set_tx,
    session_record_activity_by_thread_tx, session_record_activity_tx, session_set_liveness_tx,
    session_state_transition_tx, worker_session_status_transition_allowed,
};
pub(crate) use session_row::{derive_session_identity, worker_session_from_row};
pub use task::{
    SuccessReportFlip, TASK_STATUS_DETAIL_DELIVERY_ABANDONED, TaskReporter,
    require_track_exists_tx, status_detail_class, status_detail_with_reason,
    task_abandon_delivery_tx, task_apply_gate_result_tx, task_cancel_pending_with_detail_tx,
    task_cancel_running_tx, task_cancel_tx, task_claim_pending_tx, task_complete_from_worker_tx,
    task_fail_from_worker_tx, task_gate_attempt_bump_tx, task_get_tx, task_mark_running_tx,
    task_mark_sub_track_running_tx, task_report_success_from_worker_tx,
    task_stamp_missing_running_deadline_tx, task_start_verifying_from_worker_tx,
    task_update_pending_tx, tasks_by_track_tx, track_lifecycle_and_budget_tx,
    track_require_task_gates_tx, worker_op_targets_card_tx,
};
pub use task_attempt::{
    task_attempt_current_by_track_pool, task_attempt_current_by_track_tx,
    task_attempt_current_pool, task_attempt_current_tx, task_attempt_get_tx, task_current_get_pool,
    task_current_get_tx, task_history_by_key_pool, task_recovery_allocate_tx,
    task_recovery_constraint_tx, task_recovery_lookup_tx,
};
pub use task_projection::{
    BlockVerdict, PROJECTION_DRIFT_TASK_FIELDS, TaskPendingReason, TaskProjectionOutcome,
    WithdrawalEdge, evaluate_schedulability, evaluate_schedulability_with_task_budget_default,
    mark_context_material_tx, project_tasks_tx, project_tasks_with_tree_term_tx,
    task_delete_pending_tx,
};
// The request-fingerprint enum is exported with its binding: route code must
// construct V1 on write and handle LegacyUnknown explicitly on read.
pub use track::{
    AttachedInheritedPath, TrackCreateBinding, TrackCreateBindingClaim,
    TrackCreateRequestFingerprint, TrackRecipeOrigin, TrackWorkspacePlan,
    track_create_idempotency_claim_tx, track_create_idempotency_get_pool, track_create_tx,
    track_delete_tx, track_require_candidate_verification_settled_tx, track_require_leaf_tx,
    track_update_tx,
};
pub use track_claude_permissions::track_claude_permissions_ceiling_read;
pub use track_recipe::track_recipe_get_tx;
pub use track_tree::{
    DEFAULT_TREE_TASK_BUDGET, MAX_TRACK_TREE_DEPTH, MAX_TREE_TASK_BUDGET, TRACK_BOUNDED_PATH_SQL,
    TRACK_ROOT_DEPTH_SQL, TRACK_TREE_MEMBERS_SQL, TRACK_TREE_MEMBERS_WITH_FIXED_PLANNER_SQL,
    TRACK_TREE_PLANNER_INVENTORY_SQL, TrackTreeTerm, TrackTreeTermOutcome, TreeShare,
    can_add_tree_member, deterministic_share, track_tree_budget, track_tree_member_count,
    track_tree_planner_inventory, track_tree_planner_inventory_by_member, track_tree_term,
    tree_share_from_member_inventory,
};
pub use track_workspace::{
    track_workspace_freeze_tx, track_workspace_read_tx, track_workspace_write_tx,
};

use infra::check_no_unknown_future_migrations;

pub struct SqlxRepo {
    pool: SqlitePool,
    /// Write-through role cache kept in sync by the `_tx` helpers; `AppState`
    /// holds its own instance seeded from the same pool.
    card_role_cache: CardRoleCache,
    /// Write-through `TrackId -> AreaId` cache, same shape as `card_role_cache`.
    track_area_cache: TrackAreaCache,
    /// Keepalive for in-memory databases: sqlx's shared-cache DB is destroyed with
    /// its last connection, so one pool-external connection pins it. `None` on-disk.
    _memory_cache_anchor: Option<SqliteConnection>,
    /// Stable database identity, read once from the one-row `database_identity` table.
    database_id: Arc<String>,
}

impl SqlxRepo {
    /// Open / create the SQLite DB at `url`, run pending migrations, and
    /// enable foreign-key enforcement per-connection.
    pub async fn open(url: &str) -> Result<Self> {
        let mut opts = SqliteConnectOptions::from_str(url)
            .map_err(|e| CalmError::Internal(format!("invalid sqlite url {url:?}: {e}")))?
            .create_if_missing(true)
            .foreign_keys(true);
        opts = opts.log_statements(tracing::log::LevelFilter::Debug);

        let pool = SqlitePoolOptions::new()
            .acquire_timeout(Duration::from_millis(SQLITE_ACQUIRE_TIMEOUT_MS))
            // Re-issue the pragmas per connection: connect options are dropped for some URL forms (e.g. memory).
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
            // Self-heal connections released while still inside a transaction (a cancelled
            // `begin_with` leaks one). Roll back rather than discard: dropping an in-memory
            // DB's connection can drop the database.
            .after_release(|conn, _meta| {
                Box::pin(async move {
                    if !Connection::is_in_transaction(conn) {
                        return Ok(true);
                    }
                    // A dropped `Transaction` has only queued its rollback; ping round-trips the
                    // worker queue so it completes before we judge the connection leaked.
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
                    // Fail closed: the Err makes the pool close_hard the connection. Safe for
                    // in-memory repos because `_memory_cache_anchor` keeps the cache alive.
                    Err(sqlx::Error::Protocol(
                        "connection still inside a transaction after bounded rollback".into(),
                    ))
                })
            })
            .connect_with(opts)
            .await?;

        // Anchor in-memory shared caches BEFORE anything else touches the pool.
        // `pragma_database_list.file` is empty for in-memory (and temp-file) DBs.
        let mut candidate = pool.acquire().await?;
        let main_db_file: String =
            sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name = 'main'")
                .fetch_one(&mut *candidate)
                .await?;
        let memory_cache_anchor = if main_db_file.is_empty() {
            Some(candidate.detach())
        } else {
            drop(candidate);
            None
        };

        // Refuse to boot when the DB carries a migration row this binary doesn't know
        // about, before sqlx can apply any pending known migration.
        check_no_unknown_future_migrations(&pool, &crate::MIGRATOR).await?;

        crate::MIGRATOR
            .run(&pool)
            .await
            .map_err(|e| CalmError::Internal(format!("migrate: {e}")))?;

        track_vcs::backfill_existing_tracks(&pool).await?;

        let card_role_cache = CardRoleCache::new();
        card_role_cache.seed_from_db(&pool).await?;
        let track_area_cache = TrackAreaCache::new();
        track_area_cache.seed_from_db(&pool).await?;

        let database_id = Arc::new(database_identity::ensure_database_identity(&pool).await?);

        Ok(Self {
            pool,
            card_role_cache,
            track_area_cache,
            _memory_cache_anchor: memory_cache_anchor,
            database_id,
        })
    }

    #[cfg(test)]
    pub(crate) fn has_memory_cache_anchor(&self) -> bool {
        self._memory_cache_anchor.is_some()
    }

    /// Pool access for tests / fixtures; production code must go through the trait.
    #[doc(hidden)]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn card_role_cache(&self) -> &CardRoleCache {
        &self.card_role_cache
    }

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

/// Boot fence: refuse to serve when `area_folders` holds two rows where one is
/// an ancestor of (or equal to) the other. `find_owner` takes the first match,
/// so overlap would silently re-own a user directory; a human resolves it.
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

    fn database_id(&self) -> Arc<String> {
        self.database_id.clone()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod area_defaults_migration_tests;

#[cfg(test)]
mod sub_track_tree_tests;
#[cfg(test)]
mod task_context_migration_tests;
#[cfg(test)]
mod task_liveness_deadline_tests;
#[cfg(test)]
mod terminal_output_migration_tests;
#[cfg(test)]
mod track_tree_budget_tests;

#[cfg(test)]
mod workspace_lease_lookup_tests;

#[cfg(test)]
mod write_path_gate_wiring_tests;

#[cfg(test)]
mod append_seam_gate_tests;

#[cfg(test)]
mod planner_provider_session_tests;
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
mod operations_keyed_rows_permanent_tests;

#[cfg(test)]
mod track_create_idempotency_tests;

#[cfg(test)]
mod track_create_request_fingerprint_migration_tests;

#[cfg(test)]
mod track_template_rename_migration_tests;

#[cfg(test)]
mod pool_tx_repair_tests;

#[cfg(test)]
mod deadlock_semantics_tests;

#[cfg(test)]
mod pool_memory_anchor_tests;

#[cfg(test)]
mod database_identity_tests;
#[cfg(test)]
mod transcript_index_tests;

#[cfg(test)]
mod task_projection_snapshot_tests;

#[cfg(test)]
mod proposal_withdraw_upgrade_tests;

#[cfg(test)]
mod track_detail_json_shape_tests;
#[cfg(test)]
mod track_detail_order_tests;
#[cfg(test)]
mod track_detail_sort_precision_tests;
