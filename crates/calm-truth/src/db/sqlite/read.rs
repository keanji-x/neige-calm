use async_trait::async_trait;
use sqlx::Row;

use super::task::TASK_COLUMNS;
use super::{
    SqlxRepo, derive_session_identity, session_get_by_active_token_hash, session_get_by_id,
};
use crate::card_role_cache::CardRoleCache;
use crate::db::rows::{TRACK_SELECT_COLUMNS, TRACK_SELECT_COLUMNS_W};
use crate::db::{RepoRead, SessionCardIdentity, SharedCodexDaemonRecord, WorkspaceLease};
use crate::error::{CalmError, Result};
use crate::ids::{AreaId, CardId, TrackId};
use crate::model::*;
use crate::session_projection_repo::WorkerSessionKind;
use crate::track_area_cache::TrackAreaCache;
use calm_types::claude_permissions::ClaudePermissionsScope;
use calm_types::worker::{WorkerSession, WorkerSessionId};

/// Row shape of the single-statement `track_detail` read.
#[derive(sqlx::FromRow)]
struct TrackDetailRow {
    #[sqlx(flatten)]
    track: crate::db::rows::TrackRow,
    referenced_as_child: bool,
    cards_json: String,
    overlays_json: String,
}

/// Explicit allowlist of `harness_items` methods a transcript can render.
/// Spliced, not bound: a fixed literal with no caller input.
const TRANSCRIPT_METHOD_PREDICATE: &str =
    " AND method IN ('item/started', 'item/completed', 'turn/completed')";

impl SqlxRepo {
    async fn harness_item_page(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
        method_predicate: &str,
    ) -> Result<Vec<HarnessItem>> {
        const COLUMNS: &str = "id, worker_session_id, card_id, track_id, thread_id, turn_id, \
                               item_uuid, item_type, method, params, input_segments, \
                               created_at_ms";
        let (comparison, order, cursor) = if descending {
            ("<", "DESC", if after_id == 0 { i64::MAX } else { after_id })
        } else {
            (">", "ASC", after_id)
        };
        let sql = format!(
            "SELECT {COLUMNS} FROM harness_items \
             WHERE card_id = ?1 AND id {comparison} ?2{method_predicate} \
             ORDER BY id {order} LIMIT ?3"
        );
        let mut rows = sqlx::query_as::<_, crate::db::rows::HarnessItemRow>(&sql)
            .bind(card_id)
            .bind(cursor)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        if descending {
            rows.reverse();
        }
        rows.into_iter()
            .map(HarnessItem::try_from)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| {
                CalmError::Internal(format!("transcript input_segments decode: {error}"))
            })
    }
}

#[async_trait]
impl RepoRead for SqlxRepo {
    async fn areas_list(&self) -> Result<Vec<Area>> {
        let rows = sqlx::query_as::<_, crate::db::rows::AreaRow>(
            r#"SELECT id, name, color, sort, kind, default_template_id, default_cwd,
                      created_at, updated_at
               FROM areas ORDER BY sort ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Area::from).collect())
    }

    async fn areas_list_user_visible(&self) -> Result<Vec<Area>> {
        let rows = sqlx::query_as::<_, crate::db::rows::AreaRow>(
            r#"SELECT id, name, color, sort, kind, default_template_id, default_cwd,
                      created_at, updated_at
               FROM areas WHERE kind = 'user' ORDER BY sort ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Area::from).collect())
    }

    async fn area_get(&self, id: &str) -> Result<Option<Area>> {
        let row = sqlx::query_as::<_, crate::db::rows::AreaRow>(
            r#"SELECT id, name, color, sort, kind, default_template_id, default_cwd,
                      created_at, updated_at
               FROM areas WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Area::from))
    }

    async fn area_get_system(&self) -> Result<Option<Area>> {
        // At most one system row: partial unique index on `areas(kind) WHERE kind = 'system'`.
        let row = sqlx::query_as::<_, crate::db::rows::AreaRow>(
            r#"SELECT id, name, color, sort, kind, default_template_id, default_cwd,
                      created_at, updated_at
               FROM areas WHERE kind = 'system' LIMIT 1"#,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Area::from))
    }

    async fn area_folders_by_area(&self, area_id: &str) -> Result<Vec<AreaFolder>> {
        let rows = sqlx::query_as::<_, crate::db::rows::AreaFolderRow>(
            r#"SELECT id, area_id, path, created_at
               FROM area_folders WHERE area_id = ?1 ORDER BY path ASC"#,
        )
        .bind(area_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(AreaFolder::from).collect())
    }

    async fn area_folders_list_all(&self) -> Result<Vec<AreaFolder>> {
        let rows = sqlx::query_as::<_, crate::db::rows::AreaFolderRow>(
            r#"SELECT id, area_id, path, created_at
               FROM area_folders ORDER BY path ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(AreaFolder::from).collect())
    }

    async fn area_folder_get(&self, id: i64) -> Result<Option<AreaFolder>> {
        let row = sqlx::query_as::<_, crate::db::rows::AreaFolderRow>(
            r#"SELECT id, area_id, path, created_at
               FROM area_folders WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(AreaFolder::from))
    }

    async fn tracks_by_area(&self, area_id: &str) -> Result<Vec<Track>> {
        let rows = sqlx::query_as::<_, crate::db::rows::TrackRow>(&format!(
            "SELECT {TRACK_SELECT_COLUMNS} FROM tracks WHERE area_id = ?1 ORDER BY sort ASC"
        ))
        .bind(area_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Track::from).collect())
    }

    async fn track_get_launchpad(&self) -> Result<Option<Track>> {
        // Single-valued by the partial unique index; `ORDER BY id` keeps the answer
        // stable even on a hand-broken database holding two launchpad rows.
        let row = sqlx::query_as::<_, crate::db::rows::TrackRow>(&format!(
            "SELECT {TRACK_SELECT_COLUMNS} FROM tracks WHERE purpose = 'launchpad' ORDER BY id LIMIT 1"
        ))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Track::from))
    }

    async fn track_get(&self, id: &str) -> Result<Option<Track>> {
        let row = sqlx::query_as::<_, crate::db::rows::TrackRow>(&format!(
            "SELECT {TRACK_SELECT_COLUMNS} FROM tracks WHERE id = ?1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Track::from))
    }

    async fn track_claude_permissions_ceiling(
        &self,
        id: &str,
    ) -> Result<Option<ClaudePermissionsScope>> {
        // No transaction: the terminal adapter's in-tx re-check makes the open's verdict exact.
        let mut conn = self.pool.acquire().await?;
        super::track_claude_permissions_ceiling_read(&mut conn, id).await
    }

    async fn tracks_window(
        &self,
        area_id: Option<&str>,
        since: Option<i64>,
        until: Option<i64>,
    ) -> Result<Vec<Track>> {
        // WHERE built dynamically: sqlx has no optional-bind ergonomics.
        let mut sql = format!("SELECT {TRACK_SELECT_COLUMNS} FROM tracks");
        let mut where_clauses: Vec<&str> = Vec::new();
        if area_id.is_some() {
            where_clauses.push("area_id = ?");
        }
        if until.is_some() {
            where_clauses.push("created_at <= ?");
        }
        if since.is_some() {
            where_clauses.push("(terminal_at IS NULL OR terminal_at >= ?)");
        }
        if !where_clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&where_clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY created_at ASC, id ASC");

        let mut q = sqlx::query_as::<_, crate::db::rows::TrackRow>(&sql);
        if let Some(c) = area_id {
            q = q.bind(c);
        }
        if let Some(u) = until {
            q = q.bind(u);
        }
        if let Some(s) = since {
            q = q.bind(s);
        }
        Ok(q.fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(Track::from)
            .collect())
    }

    async fn track_detail(&self, id: &str) -> Result<Option<TrackDetail>> {
        // ONE autocommit statement: a deferred read tx deadlocks with the IMMEDIATE
        // track-delete writer on shared-cache DBs, and separate statements lose snapshot
        // consistency. `sort` goes through `printf('%!.17g')` because `json_object`
        // renders REAL with only 15 significant digits; `json(c.payload)` re-renders
        // so corrupt text raises instead of becoming card structure.
        let row = sqlx::query_as::<_, TrackDetailRow>(&format!(
            r#"SELECT {TRACK_SELECT_COLUMNS_W},
                      EXISTS(SELECT 1 FROM tasks parent_task
                              WHERE parent_task.child_track_id = w.id) AS referenced_as_child,
                      (SELECT json_group_array(json_object(
                           'id', c.id, 'track_id', c.track_id, 'kind', c.kind,
                           'sort', json(printf('%!.17g', c.sort)),
                           'payload', json(c.payload), 'title', c.title,
                           'deletable', json(CASE WHEN c.deletable THEN 'true' ELSE 'false' END),
                           'created_at', c.created_at, 'updated_at', c.updated_at))
                       FROM cards c WHERE c.track_id = w.id) AS cards_json,
                      (SELECT json_group_array(json_object(
                           'id', o.id, 'plugin_id', o.plugin_id, 'entity_kind', o.entity_kind,
                           'entity_id', o.entity_id, 'kind', o.kind, 'payload', json(o.payload),
                           'updated_at', o.updated_at))
                       FROM overlays o
                       WHERE (o.entity_kind = 'track' AND o.entity_id = w.id)
                          OR (o.entity_kind = 'card'
                              AND o.entity_id IN
                                  (SELECT c2.id FROM cards c2 WHERE c2.track_id = w.id)))
                          AS overlays_json
               FROM tracks w WHERE w.id = ?1"#
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };

        // `json_group_array` input order is unspecified, so both arrays are sorted
        // here by a TOTAL key: cards `(sort, id)`, overlays the table's UNIQUE key.
        let mut cards: Vec<Card> = serde_json::from_str(&row.cards_json)?;
        cards.sort_by(|a, b| {
            a.sort
                .total_cmp(&b.sort)
                .then_with(|| a.id.as_str().cmp(b.id.as_str()))
        });
        let mut overlays: Vec<Overlay> = serde_json::from_str(&row.overlays_json)?;
        overlays.sort_by(|a, b| {
            (&a.entity_kind, &a.entity_id, &a.plugin_id, &a.kind).cmp(&(
                &b.entity_kind,
                &b.entity_id,
                &b.plugin_id,
                &b.kind,
            ))
        });

        // Child ownership constrains only a terminal reopen: Blocked and
        // Reviewing children may legally return to Working, while a terminal
        // child has already resolved its parent task and cannot be reopened.
        let can_resume = calm_types::track_lifecycle::user_can_resume(row.track.lifecycle)
            && row.track.purpose.as_deref() != Some(calm_types::model::AREA_CHAT_PURPOSE)
            && (!row.track.lifecycle.is_terminal() || !row.referenced_as_child);
        Ok(Some(TrackDetail {
            track: Track::from(row.track),
            can_resume,
            cards,
            overlays,
        }))
    }

    async fn tasks_by_track(&self, track_id: &str) -> Result<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM current_tasks WHERE track_id = ?1 \
             ORDER BY priority DESC, created_at_ms ASC, key ASC"
        );
        let rows = sqlx::query_as::<_, Task>(&sql)
            .bind(track_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    async fn task_for_worker_card(&self, card_id: &str) -> Result<Option<Task>> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE worker_card_id=?1 LIMIT 2");
        let mut tasks = sqlx::query_as::<_, Task>(&sql)
            .bind(card_id)
            .fetch_all(&self.pool)
            .await?;
        if tasks.len() > 1 {
            return Err(CalmError::Conflict(
                "ambiguous task ownership for worker card",
            ));
        }
        Ok(tasks.pop())
    }

    async fn task_current_get(&self, track_id: &str, key: &str) -> Result<Option<Task>> {
        super::task_attempt::task_current_get_pool(&self.pool, track_id, key).await
    }

    async fn task_history_by_key(&self, track_id: &str, key: &str) -> Result<Vec<Task>> {
        super::task_attempt::task_history_by_key_pool(&self.pool, track_id, key).await
    }

    async fn task_get(&self, id: &str) -> Result<Option<Task>> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1");
        let row = sqlx::query_as::<_, Task>(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }

    async fn tasks_nonterminal(&self) -> Result<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM current_tasks \
             WHERE status IN ('pending', 'dispatched', 'running', 'verifying') \
             ORDER BY track_id ASC, priority DESC, created_at_ms ASC, key ASC"
        );
        let rows = sqlx::query_as::<_, Task>(&sql)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    async fn task_contexts_by_dst_track(
        &self,
        dst_track_id: &str,
    ) -> Result<Vec<crate::db::TaskContextRow>> {
        let rows = sqlx::query_as::<_, (String, String, Option<String>, i64)>(
            r#"SELECT DISTINCT t.id, t.track_id, t.claim_context_json, t.context_closure_truncated
               FROM task_ref_index i
               JOIN tasks t ON t.id = i.task_id
               WHERE i.dst_track_id = ?1
                 AND t.status IN ('dispatched','running','verifying')
                 AND t.context_stale_at_ms IS NULL
               ORDER BY t.id"#,
        )
        .bind(dst_track_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(task_id, track_id, claim_context_json, closure_truncated)| {
                    crate::db::TaskContextRow {
                        task_id,
                        track_id,
                        claim_context_json,
                        closure_truncated: closure_truncated != 0,
                    }
                },
            )
            .collect())
    }

    async fn stale_task_contexts_by_dst_track(
        &self,
        dst_track_id: &str,
    ) -> Result<Vec<crate::db::TaskContextRow>> {
        let rows = sqlx::query_as::<_, (String, String, Option<String>, i64)>(
            r#"SELECT DISTINCT t.id, t.track_id, t.claim_context_json, t.context_closure_truncated
               FROM task_ref_index i
               JOIN tasks t ON t.id = i.task_id
               WHERE i.dst_track_id = ?1
                 AND t.status IN ('dispatched','running','verifying')
                 AND t.context_stale_at_ms IS NOT NULL
               ORDER BY t.id"#,
        )
        .bind(dst_track_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(task_id, track_id, claim_context_json, closure_truncated)| {
                    crate::db::TaskContextRow {
                        task_id,
                        track_id,
                        claim_context_json,
                        closure_truncated: closure_truncated != 0,
                    }
                },
            )
            .collect())
    }

    async fn task_contexts_inflight_fresh(&self) -> Result<Vec<crate::db::TaskContextRow>> {
        let rows = sqlx::query_as::<_, (String, String, Option<String>, i64)>(
            r#"SELECT id, track_id, claim_context_json, context_closure_truncated
               FROM tasks
               WHERE status IN ('dispatched','running','verifying')
                 AND context_stale_at_ms IS NULL
               ORDER BY id"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(task_id, track_id, claim_context_json, closure_truncated)| {
                    crate::db::TaskContextRow {
                        task_id,
                        track_id,
                        claim_context_json,
                        closure_truncated: closure_truncated != 0,
                    }
                },
            )
            .collect())
    }

    async fn task_contexts_inflight_stale(&self) -> Result<Vec<crate::db::TaskContextRow>> {
        let rows = sqlx::query_as::<_, (String, String, Option<String>, i64)>(
            r#"SELECT id, track_id, claim_context_json, context_closure_truncated
               FROM tasks
               WHERE status IN ('dispatched','running','verifying')
                 AND context_stale_at_ms IS NOT NULL
               ORDER BY id"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(task_id, track_id, claim_context_json, closure_truncated)| {
                    crate::db::TaskContextRow {
                        task_id,
                        track_id,
                        claim_context_json,
                        closure_truncated: closure_truncated != 0,
                    }
                },
            )
            .collect())
    }

    async fn operation_idempotency_key_by_id(&self, op_id: &str) -> Result<Option<String>> {
        let row: Option<Option<String>> =
            sqlx::query_scalar("SELECT idempotency_key FROM operations WHERE id = ?1")
                .bind(op_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.flatten())
    }

    async fn cards_by_track(&self, track_id: &str) -> Result<Vec<Card>> {
        // ORDER BY must stay aligned with track_vcs::cards_for_track_tx.
        let rows = sqlx::query_as::<_, crate::db::rows::CardRow>(
            r#"SELECT id, track_id, kind, sort, payload, title, deletable, created_at, updated_at
               FROM cards WHERE track_id = ?1 ORDER BY sort ASC, id ASC"#,
        )
        .bind(track_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Card::from).collect())
    }

    async fn track_report_cards_by_area(&self, area_id: &str) -> Result<Vec<Card>> {
        let rows = sqlx::query_as::<_, crate::db::rows::CardRow>(
            r#"SELECT id, track_id, kind, sort, payload, title, deletable, created_at, updated_at
               FROM cards
               WHERE kind = 'track-report'
                 AND track_id IN (SELECT id FROM tracks WHERE area_id = ?1)
               ORDER BY track_id ASC, id ASC"#,
        )
        .bind(area_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Card::from).collect())
    }

    async fn card_get(&self, id: &str) -> Result<Option<Card>> {
        let row = sqlx::query_as::<_, crate::db::rows::CardRow>(
            r#"SELECT id, track_id, kind, sort, payload, title, deletable, created_at, updated_at
               FROM cards WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Card::from))
    }

    async fn card_get_with_body_crdt(&self, id: &str) -> Result<Option<(Card, Option<Vec<u8>>)>> {
        #[derive(sqlx::FromRow)]
        struct CardWithCrdtRow {
            #[sqlx(flatten)]
            card: crate::db::rows::CardRow,
            body_crdt: Option<Vec<u8>>,
        }
        // Single SELECT = a self-consistent row snapshot: payload and
        // body_crdt can never tear against each other.
        let row: Option<CardWithCrdtRow> = sqlx::query_as(
            r#"SELECT id, track_id, kind, sort, payload, title, deletable, created_at, updated_at,
                      body_crdt
               FROM cards WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| (Card::from(row.card), row.body_crdt)))
    }

    async fn task_diagnostics(
        &self,
        track_id: &str,
        blocks: &[calm_types::track_report::ReportBlock],
        task_budget_default: i64,
    ) -> Result<Vec<super::BlockVerdict>> {
        // One autocommit statement supplies a point-in-time fact set; Rust owns the
        // verdict predicate the write path runs inside its IMMEDIATE transaction.
        let mut conn = self.pool.acquire().await?;
        let (declarations, local) =
            calm_types::report_blocks::tasks::project_task_declarations(blocks);
        let diagnostics = super::task_projection::evaluate_schedulability_with_task_budget_default(
            &mut conn,
            track_id,
            &declarations,
            &local,
            task_budget_default,
        )
        .await?;
        Ok(diagnostics)
    }

    async fn card_role_get(&self, id: &str) -> Result<Option<CardRole>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT role FROM cards WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|(role,)| {
            CardRole::try_from(role)
                .map_err(|e| CalmError::Internal(format!("cards.role decode: {e}")))
        })
        .transpose()
    }

    async fn harness_item_list_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<HarnessItem>> {
        self.harness_item_page(card_id, after_id, limit, descending, "")
            .await
    }

    async fn harness_item_list_transcript_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<HarnessItem>> {
        self.harness_item_page(
            card_id,
            after_id,
            limit,
            descending,
            TRANSCRIPT_METHOD_PREDICATE,
        )
        .await
    }

    async fn worker_flow_item_list_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<crate::db::rows::WorkerFlowItemRow>> {
        // Clamp so a huge (or non-positive) limit cannot scan the whole table.
        let limit = limit.clamp(1, 500);
        let (sql, cursor) = if descending {
            (
                r#"SELECT id, card_id, captured_session_id, track_id, worker_session_id,
                          kind, payload, created_at_ms
                   FROM worker_flow_items
                   WHERE card_id = ?1 AND id < ?2
                   ORDER BY id DESC
                   LIMIT ?3"#,
                if after_id == 0 { i64::MAX } else { after_id },
            )
        } else {
            (
                r#"SELECT id, card_id, captured_session_id, track_id, worker_session_id,
                          kind, payload, created_at_ms
                   FROM worker_flow_items
                   WHERE card_id = ?1 AND id > ?2
                   ORDER BY id ASC
                   LIMIT ?3"#,
                after_id,
            )
        };
        let mut rows = sqlx::query_as::<_, crate::db::rows::WorkerFlowItemRow>(sql)
            .bind(card_id)
            .bind(cursor)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        if descending {
            rows.reverse();
        }
        Ok(rows)
    }

    async fn worker_flow_cursor_get(
        &self,
        card_id: &str,
        source_kind: &str,
    ) -> Result<Option<crate::db::rows::WorkerFlowCursor>> {
        let row = sqlx::query_as::<_, crate::db::rows::WorkerFlowCursor>(
            r#"SELECT card_id, source_kind, source_path, record_index,
                      byte_offset, last_source_uuid, last_line_hash, updated_at_ms
               FROM worker_flow_cursors
               WHERE card_id = ?1 AND source_kind = ?2"#,
        )
        .bind(card_id)
        .bind(source_kind)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn shared_daemon_runtime_get(&self) -> Result<SharedCodexDaemonRecord> {
        let row = sqlx::query_as::<
            _,
            (
                String,
                Option<i32>,
                Option<i32>,
                Option<String>,
                Option<String>,
                Option<i64>,
                Option<String>,
                Option<i64>,
                i64,
                i64,
                Option<String>,
                Option<String>,
            ),
        >(
            r#"SELECT state, pid, pgid, sock_path, codex_home_path, process_start_time,
                      boot_id, started_at, updated_at, restart_count, last_error,
                      daemon_env_signature
               FROM shared_codex_daemon
               WHERE id = 1"#,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(SharedCodexDaemonRecord {
            state: row.0,
            pid: row.1,
            pgid: row.2,
            sock_path: row.3,
            codex_home_path: row.4,
            process_start_time: row.5.and_then(|v| u64::try_from(v).ok()),
            boot_id: row.6,
            started_at: row.7,
            updated_at: row.8,
            restart_count: row.9,
            last_error: row.10,
            daemon_env_signature: row.11,
        })
    }

    async fn overlays_for(&self, entity_kind: &str, entity_id: &str) -> Result<Vec<Overlay>> {
        let rows = sqlx::query_as::<_, crate::db::rows::OverlayRow>(
            r#"SELECT id, plugin_id, entity_kind, entity_id, kind, payload, updated_at
               FROM overlays WHERE entity_kind = ?1 AND entity_id = ?2"#,
        )
        .bind(entity_kind)
        .bind(entity_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Overlay::from).collect())
    }

    async fn overlays_by_kind(&self, entity_kind: &str) -> Result<Vec<Overlay>> {
        let rows = sqlx::query_as::<_, crate::db::rows::OverlayRow>(
            r#"SELECT id, plugin_id, entity_kind, entity_id, kind, payload, updated_at
               FROM overlays WHERE entity_kind = ?1"#,
        )
        .bind(entity_kind)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Overlay::from).collect())
    }

    async fn terminal_get(&self, id: &str) -> Result<Option<Terminal>> {
        let row = sqlx::query_as::<_, Terminal>(
            r#"SELECT id, card_id, program, cwd, env, pid,
                      theme_fg, theme_bg, exit_code, signal_killed,
                      pty_output, pty_output_truncated, created_at
               FROM terminals WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn terminal_get_by_card(&self, card_id: &str) -> Result<Option<Terminal>> {
        let row = sqlx::query_as::<_, Terminal>(
            r#"SELECT id, card_id, program, cwd, env, pid,
                      theme_fg, theme_bg, exit_code, signal_killed,
                      pty_output, pty_output_truncated, created_at
               FROM terminals WHERE card_id = ?1"#,
        )
        .bind(card_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn terminals_orphaned(&self, grace_seconds: i64) -> Result<Vec<Terminal>> {
        // Orphan: no active worker_session AND older than `grace_seconds` (created_at
        // is unix ms). A Terminal card's terminal with a recorded exit is not residue:
        // it follows its card so its final screen and exit code stay observable.
        let cutoff = now_ms() - grace_seconds.saturating_mul(1000);
        let rows = sqlx::query_as::<_, Terminal>(
            r#"SELECT t.id, t.card_id, t.program, t.cwd, t.env,
                      t.pid,
                      t.theme_fg, t.theme_bg,
                      t.exit_code, t.signal_killed,
                      t.pty_output, t.pty_output_truncated,
                      t.created_at
               FROM terminals t
               WHERE NOT EXISTS (
                   SELECT 1 FROM worker_sessions ws
                   WHERE ws.card_id = t.card_id
                     AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
               )
               AND t.created_at < ?1
               AND NOT (
                   (t.exit_code IS NOT NULL OR t.signal_killed = 1)
                   AND EXISTS (
                       SELECT 1 FROM cards c
                       WHERE c.id = t.card_id AND c.kind = 'terminal'
                   )
               )"#,
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn terminals_running(&self) -> Result<Vec<Terminal>> {
        let rows = sqlx::query_as::<_, Terminal>(
            r#"SELECT id, card_id, program, cwd, env,
                      pid,
                      theme_fg, theme_bg,
                      exit_code, signal_killed,
                      pty_output, pty_output_truncated,
                      created_at
               FROM terminals
               WHERE exit_code IS NULL AND signal_killed = 0"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn shared_planner_cards_for_initial_prompt_takeover(
        &self,
    ) -> Result<Vec<(String, String, String, i64)>> {
        let (provider, _mode, contract) =
            derive_session_identity(&WorkerSessionKind::SharedPlanner);
        // Require a LIVE terminal row: a reaped TUI can never emit thread/started, so
        // re-registering it would strand the FIFO entry and absorb a later attribution.
        let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
            r#"SELECT c.id,
                      c.track_id,
                      ws.terminal_run_id,
                      0
               FROM cards c
               JOIN tracks w ON w.id = c.track_id
               JOIN worker_sessions ws ON ws.id = c.session_id
                   AND ws.provider = ?1
                   AND ws.contract = ?2
                   AND ws.thread_id IS NULL
                   AND ws.state IN ('starting','running','idle','turn_pending')
               JOIN terminals t ON t.id = ws.terminal_run_id
               WHERE c.role = 'planner'
                 AND t.exit_code IS NULL
                 AND COALESCE(t.signal_killed, 0) = 0
                 AND NOT EXISTS (
                       SELECT 1
                         FROM worker_sessions hws
                         JOIN cards hc ON hc.session_id = hws.id
                        WHERE hc.id = c.id
                          AND hws.provider = ?3
                          AND hws.contract = ?4
                          AND hws.state IN ('starting','running','idle','turn_pending')
                          AND hws.handle_state_json IS NOT NULL
                          AND json_extract(hws.handle_state_json, '$.mode') = 'harness'
                 )
                 AND w.lifecycle NOT IN ('done', 'canceled', 'failed')
               ORDER BY c.created_at ASC, c.id ASC"#,
        )
        .bind(provider.as_db_str())
        .bind(contract.as_db_str())
        .bind(provider.as_db_str())
        .bind(contract.as_db_str())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn plugins_list(&self) -> Result<Vec<Plugin>> {
        self.plugins_list_all().await
    }

    async fn plugins_list_all(&self) -> Result<Vec<Plugin>> {
        let rows = sqlx::query_as::<_, Plugin>(
            r#"SELECT id, version, install_path, manifest, enabled, user_config,
                      installed_at, updated_at
               FROM plugins
               ORDER BY id ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn plugin_get_by_id(&self, id: &str) -> Result<Option<Plugin>> {
        let row = sqlx::query_as::<_, Plugin>(
            r#"SELECT id, version, install_path, manifest, enabled, user_config,
                      installed_at, updated_at
               FROM plugins WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn plugin_token_get(&self, plugin_id: &str) -> Result<Option<(String, i64)>> {
        let row: Option<(String, i64)> = sqlx::query_as(
            r#"SELECT hashed_token, expires_at FROM plugin_tokens WHERE plugin_id = ?1"#,
        )
        .bind(plugin_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn plugin_kv_get(&self, plugin_id: &str, key: &str) -> Result<Option<serde_json::Value>> {
        let row: Option<(String,)> =
            sqlx::query_as(r#"SELECT value FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2"#)
                .bind(plugin_id)
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some((text,)) => Ok(Some(serde_json::from_str(&text)?)),
            None => Ok(None),
        }
    }

    async fn plugin_kv_list(
        &self,
        plugin_id: &str,
        prefix: &str,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        let mut escaped = String::with_capacity(prefix.len() + 2);
        for ch in prefix.chars() {
            if ch == '%' || ch == '_' || ch == '\\' {
                escaped.push('\\');
            }
            escaped.push(ch);
        }
        escaped.push('%');
        let rows: Vec<(String, String)> = sqlx::query_as(
            r#"SELECT key, value FROM plugin_kv
               WHERE plugin_id = ?1 AND key LIKE ?2 ESCAPE '\'
               ORDER BY key ASC"#,
        )
        .bind(plugin_id)
        .bind(&escaped)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for (k, v) in rows {
            out.push((k, serde_json::from_str(&v)?));
        }
        Ok(out)
    }

    async fn settings_get_all(&self) -> Result<Vec<(String, String)>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as(r#"SELECT key, value FROM settings ORDER BY key ASC"#)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
    }

    async fn seed_card_role_cache(&self, cache: &CardRoleCache) -> Result<()> {
        cache.seed_from_db(&self.pool).await
    }

    async fn seed_track_area_cache(&self, cache: &TrackAreaCache) -> Result<()> {
        cache.seed_from_db(&self.pool).await
    }

    async fn card_mcp_token_lookup_by_hash(
        &self,
        hashed_token: &str,
    ) -> Result<Option<(String, String)>> {
        // Echo the stored `hashed_token` so the handshake constant-time compares
        // against the stored representation, not the caller's input.
        let row: Option<(String, String)> = sqlx::query_as(
            r#"SELECT card_id, hashed_token FROM card_mcp_tokens WHERE hashed_token = ?1"#,
        )
        .bind(hashed_token)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn card_identity_get_by_session(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionCardIdentity>> {
        let rows = sqlx::query(
            r#"SELECT c.id, c.role, c.track_id, w.area_id
               FROM cards c
               JOIN tracks w ON w.id = c.track_id
              WHERE c.session_id = ?1
              ORDER BY c.updated_at DESC, c.created_at DESC, c.id DESC
              LIMIT 2"#,
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => {
                let role = CardRole::try_from(row.try_get::<String, _>("role")?)
                    .map_err(|e| CalmError::Internal(format!("cards.role decode: {e}")))?;
                Ok(Some(SessionCardIdentity {
                    card_id: CardId(row.try_get("id")?),
                    role,
                    track_id: TrackId(row.try_get("track_id")?),
                    area_id: AreaId(row.try_get("area_id")?),
                }))
            }
            _ => Err(CalmError::Internal(format!(
                "multiple cards linked to worker session {session_id}"
            ))),
        }
    }

    async fn workspace_lease_for_card(&self, card_id: &str) -> Result<Option<WorkspaceLease>> {
        let row = sqlx::query(
            r#"SELECT lease_id, card_id, track_id, path, state
               FROM workspace_leases
               WHERE card_id = ?1
                 AND state = 'held'
               ORDER BY created_at_ms DESC, lease_id DESC
               LIMIT 1"#,
        )
        .bind(card_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(WorkspaceLease {
                lease_id: row.try_get("lease_id")?,
                card_id: row.try_get("card_id")?,
                track_id: row.try_get("track_id")?,
                path: row.try_get("path")?,
                state: row.try_get("state")?,
            })
        })
        .transpose()
    }

    async fn session_get_by_active_token_hash(
        &self,
        hashed_token: &str,
    ) -> Result<Option<WorkerSession>> {
        session_get_by_active_token_hash(&self.pool, hashed_token).await
    }

    async fn session_get_by_id(&self, id: &WorkerSessionId) -> Result<Option<WorkerSession>> {
        session_get_by_id(&self.pool, id).await
    }

    async fn card_mcp_token_exists_for_card(&self, card_id: &str) -> Result<bool> {
        let row: Option<(i64,)> =
            sqlx::query_as(r#"SELECT 1 FROM card_mcp_tokens WHERE card_id = ?1 LIMIT 1"#)
                .bind(card_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some())
    }
}
