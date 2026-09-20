use async_trait::async_trait;
use sqlx::Row;

use super::{
    SqlxRepo, area_create_tx, area_delete_tx, area_update_tx, begin_immediate_tx, card_create_tx,
    card_create_with_id_tx, card_delete_tx, card_update_tx, overlay_delete_by_entity_tx,
    overlay_delete_card_overlays_by_track_tx, overlay_delete_subtree_by_area_tx, overlay_delete_tx,
    overlay_upsert_tx, session_commit_exit_tx, session_insert_tx,
    session_record_activity_by_thread_tx, session_record_activity_tx, session_set_liveness_tx,
    session_state_transition_tx, track_create_tx, track_delete_tx, track_update_tx,
    worker_session_from_row,
};
use crate::db::RepoSyncDomainRaw;
use crate::error::{CalmError, Result};
use crate::ids::{AreaId, TrackId};
use crate::model::*;
use crate::session_repo::{CommitExitOutcome, DeadRootCandidate, SessionRepo, Tx as SessionTx};
use calm_types::worker::{Liveness, WorkerSession, WorkerSessionId, WorkerSessionState};

fn is_session_conflict(err: &CalmError) -> bool {
    matches!(
        err,
        CalmError::Core(calm_types::error::CoreError::Conflict(_))
    )
}

#[async_trait]
impl SessionRepo for SqlxRepo {
    async fn session_insert_tx(
        &self,
        tx: &mut SessionTx<'_>,
        session: WorkerSession,
    ) -> Result<WorkerSession> {
        session_insert_tx(tx, session).await
    }

    async fn session_get(&self, id: &WorkerSessionId) -> Result<Option<WorkerSession>> {
        let row = sqlx::query(
            r#"SELECT id, track_id, provider, mode, contract, parent_session_id,
                      requester_session_id, state, mcp_token_hash, thread_id,
                      agent_session_id, active_turn_id, terminal_run_id, card_id,
                      handle_state_json, liveness, liveness_probed_at_ms,
                      exit_code, exit_interpretation, spawn_op_id,
                      last_activity_ms, last_thread_status, created_at_ms,
                      updated_at_ms, completed_at_ms
               FROM worker_sessions
               WHERE id = ?1"#,
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(worker_session_from_row).transpose()
    }

    async fn sessions_nonterminal(&self) -> Result<Vec<WorkerSession>> {
        let rows = sqlx::query(
            r#"SELECT id, track_id, provider, mode, contract, parent_session_id,
                      requester_session_id, state, mcp_token_hash, thread_id,
                      agent_session_id, active_turn_id, terminal_run_id, card_id,
                      handle_state_json, liveness, liveness_probed_at_ms,
                      exit_code, exit_interpretation, spawn_op_id,
                      last_activity_ms, last_thread_status, created_at_ms,
                      updated_at_ms, completed_at_ms
               FROM worker_sessions
               WHERE state IN ('starting', 'running', 'idle', 'turn_pending')
               ORDER BY track_id ASC, created_at_ms ASC, id ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(worker_session_from_row).collect()
    }

    async fn session_set_liveness(
        &self,
        id: &WorkerSessionId,
        liveness: &Liveness,
        probed_at_ms: i64,
    ) -> Result<Option<WorkerSession>> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let out = session_set_liveness_tx(&mut tx, id, liveness, probed_at_ms).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn session_record_activity(
        &self,
        id: &WorkerSessionId,
        last_activity_ms: i64,
        last_thread_status: &str,
    ) -> Result<()> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        session_record_activity_tx(&mut tx, id, last_activity_ms, last_thread_status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn session_record_activity_by_thread(
        &self,
        thread_id: &str,
        last_activity_ms: i64,
        last_thread_status: &str,
        turn_completed_ms: Option<i64>,
    ) -> Result<()> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        session_record_activity_by_thread_tx(
            &mut tx,
            thread_id,
            last_activity_ms,
            last_thread_status,
            turn_completed_ms,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn session_state_transition_tx(
        &self,
        tx: &mut SessionTx<'_>,
        id: &WorkerSessionId,
        to: WorkerSessionState,
    ) -> Result<WorkerSession> {
        session_state_transition_tx(tx, id, to).await
    }

    async fn session_commit_exit(
        &self,
        id: &WorkerSessionId,
        to: WorkerSessionState,
        liveness_probed_at_ms: i64,
        exit_code: Option<i32>,
        exit_interpretation: &str,
    ) -> Result<CommitExitOutcome> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let session = match session_commit_exit_tx(
            &mut tx,
            id,
            to,
            liveness_probed_at_ms,
            exit_code,
            exit_interpretation,
        )
        .await
        {
            Ok(session) => session,
            Err(err) if is_session_conflict(&err) => return Ok(CommitExitOutcome::Absorbed),
            Err(err) => return Err(err),
        };

        tx.commit().await?;
        Ok(CommitExitOutcome::Committed(session))
    }

    async fn session_list_by_track(&self, track_id: &TrackId) -> Result<Vec<WorkerSession>> {
        let rows = sqlx::query(
            r#"SELECT id, track_id, provider, mode, contract, parent_session_id,
                      requester_session_id, state, mcp_token_hash, thread_id,
                      agent_session_id, active_turn_id, terminal_run_id, card_id,
                      handle_state_json, liveness, liveness_probed_at_ms,
                      exit_code, exit_interpretation, spawn_op_id,
                      last_activity_ms, last_thread_status, created_at_ms,
                      updated_at_ms, completed_at_ms
               FROM worker_sessions
               WHERE track_id = ?1
               ORDER BY created_at_ms ASC, id ASC"#,
        )
        .bind(track_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(worker_session_from_row).collect()
    }

    async fn dead_root_candidates(&self) -> Result<Vec<DeadRootCandidate>> {
        // Both arms need a POSITIVE dead signal AND no active planner session; never
        // converges on absence or a just-created track. Failed-start keys on the
        // LATEST start-op by `rowid` (ids are random, `created_at_ms` can tie), so a
        // stale failed op next to an in-flight retry is not positive.
        let active = "('starting', 'running', 'idle', 'turn_pending')";
        let no_active_planner = format!(
            "NOT EXISTS (SELECT 1 FROM worker_sessions ws \
               WHERE ws.track_id = w.id AND ws.contract = 'planner' \
                 AND ws.state IN {active})"
        );
        let sql = format!(
            r#"SELECT w.id AS track_id, w.area_id AS area_id, w.lifecycle AS lifecycle
                FROM tracks w
               WHERE w.lifecycle = 'draft'
                  -- Keep in sync with calm_server::AREA_CHAT_PURPOSE.
                  AND (w.purpose IS NULL OR w.purpose <> 'area-chat')
                  AND EXISTS (
                      SELECT 1 FROM operations o
                       WHERE o.kind = 'planner-harness-start'
                         AND o.phase = 'failed'
                         -- `$.wave_id` / `$.spec_card_id` are the FROZEN keys of
                         -- `PlannerHarnessStartOperationPayload`: that payload is
                         -- hashed into `operations.payload_hash`, so #1316 kept its
                         -- serialization stable while renaming the Rust fields. A
                         -- query written against the Rust spelling matches zero
                         -- rows — silently, at runtime.
                         AND json_extract(o.payload_json, '$.wave_id') = w.id
                         -- The inner MAX subquery limits candidates to start ops
                         -- for this track's real planner card. Equality to that MAX
                         -- therefore implies o is a planner op; repeating the join
                         -- here would create an unverifiable third-defense illusion.
                         AND o.rowid = (
                             SELECT MAX(o2.rowid) FROM operations o2
                              WHERE o2.kind = 'planner-harness-start'
                                AND json_extract(o2.payload_json, '$.wave_id') = w.id
                                AND json_type(o2.payload_json, '$.spec_card_id') = 'text'
                                AND EXISTS (
                                    SELECT 1 FROM cards c2
                                     WHERE c2.id = json_extract(o2.payload_json, '$.spec_card_id')
                                       AND c2.track_id = w.id
                                       AND c2.role = 'planner'
                                )
                         )
                  )
                  AND {no_active_planner}
               UNION ALL
               SELECT w.id AS track_id, w.area_id AS area_id, w.lifecycle AS lifecycle
                 FROM tracks w
                WHERE w.lifecycle = 'planning'
                  -- Keep in sync with calm_server::AREA_CHAT_PURPOSE.
                  AND (w.purpose IS NULL OR w.purpose <> 'area-chat')
                  AND (
                      w.root_session_id IS NULL
                      OR NOT EXISTS (
                          SELECT 1 FROM worker_sessions rs
                           WHERE rs.id = w.root_session_id
                             AND rs.state IN {active}
                      )
                  )
                  AND {no_active_planner}
               ORDER BY track_id ASC"#
        );
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let track_id: String = row.try_get("track_id")?;
                let area_id: String = row.try_get("area_id")?;
                let lifecycle_raw: String = row.try_get("lifecycle")?;
                let lifecycle = TrackLifecycle::try_from(lifecycle_raw.clone()).map_err(|e| {
                    CalmError::Internal(format!(
                        "dead_root_candidates: unknown track lifecycle {lifecycle_raw:?}: {e}"
                    ))
                })?;
                Ok(DeadRootCandidate {
                    track_id: TrackId::from(track_id),
                    area_id: AreaId::from(area_id),
                    lifecycle,
                })
            })
            .collect()
    }
}

// RepoSyncDomainRaw is gated: only reachable via `AppState::raw_repo()`.

#[async_trait]
impl RepoSyncDomainRaw for SqlxRepo {
    async fn area_create(&self, p: NewArea) -> Result<Area> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let out = area_create_tx(&mut tx, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn area_update(&self, id: &str, p: AreaPatch) -> Result<Area> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let out = area_update_tx(&mut tx, id, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn area_delete(&self, id: &str) -> Result<()> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        overlay_delete_subtree_by_area_tx(&mut tx, id).await?;
        overlay_delete_by_entity_tx(&mut tx, "area", id).await?;
        area_delete_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn track_create(&self, p: NewTrack) -> Result<Track> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let out = track_create_tx(
            &mut tx,
            p,
            None,
            &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
            None,
            &self.track_area_cache,
        )
        .await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn track_update(&self, id: &str, p: TrackPatch) -> Result<Track> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let out = track_update_tx(&mut tx, id, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn track_delete(&self, id: &str) -> Result<()> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        overlay_delete_card_overlays_by_track_tx(&mut tx, id).await?;
        overlay_delete_by_entity_tx(&mut tx, "track", id).await?;
        overlay_delete_by_entity_tx(&mut tx, "view", id).await?;
        track_delete_tx(&mut tx, id, &self.track_area_cache).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn card_create(&self, p: NewCard) -> Result<Card> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let out = if p.kind == "track-report" {
            card_create_with_id_tx(
                &mut tx,
                new_id(),
                p,
                CardRole::ReportCard,
                false,
                &self.card_role_cache,
            )
            .await?
        } else {
            card_create_tx(&mut tx, p, &self.card_role_cache).await?
        };
        tx.commit().await?;
        Ok(out)
    }

    async fn card_update(&self, id: &str, p: CardPatch) -> Result<Card> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let out = card_update_tx(&mut tx, id, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn card_delete(&self, id: &str) -> Result<()> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        card_delete_tx(&mut tx, id, &self.card_role_cache).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn overlay_upsert(&self, p: NewOverlay) -> Result<Overlay> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let out = overlay_upsert_tx(&mut tx, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn overlay_delete(
        &self,
        plugin_id: &str,
        entity_kind: &str,
        entity_id: &str,
        kind: &str,
    ) -> Result<()> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        overlay_delete_tx(&mut tx, plugin_id, entity_kind, entity_id, kind).await?;
        tx.commit().await?;
        Ok(())
    }
}
