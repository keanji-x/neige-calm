use async_trait::async_trait;

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
use crate::ids::TrackId;
use crate::model::*;
use crate::session_repo::{CommitExitOutcome, SessionRepo, Tx as SessionTx};
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

    async fn codex_threads_ended(&self, thread_ids: &[String]) -> Result<Vec<String>> {
        if thread_ids.is_empty() {
            return Ok(Vec::new());
        }
        let wanted = serde_json::to_string(thread_ids)?;
        // POSITIVE end only: an exited/failed row must exist. Anything else counts as live,
        // `superseded` included: a failed forced-new-thread start restores the superseded row.
        let rows: Vec<String> = sqlx::query_scalar(
            r#"SELECT DISTINCT ended.thread_id
                 FROM json_each(?1) wanted
                 JOIN worker_sessions ended
                   ON ended.provider = 'codex' AND ended.thread_id = wanted.value
                WHERE ended.state IN ('exited', 'failed')
                  AND NOT EXISTS (
                      SELECT 1 FROM worker_sessions live
                       WHERE live.provider = 'codex' AND live.thread_id = wanted.value
                         AND live.state NOT IN ('exited', 'failed'))
                ORDER BY ended.thread_id ASC"#,
        )
        .bind(wanted)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
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
