use async_trait::async_trait;
use sqlx::Row;

use super::{
    SqlxRepo, begin_immediate_tx, card_create_tx, card_delete_tx, card_update_tx, cove_create_tx,
    cove_delete_tx, cove_update_tx, overlay_delete_by_entity_tx,
    overlay_delete_card_overlays_by_wave_tx, overlay_delete_subtree_by_cove_tx, overlay_delete_tx,
    overlay_upsert_tx, session_commit_exit_tx, session_insert_tx,
    session_record_activity_by_thread_tx, session_record_activity_tx, session_set_liveness_tx,
    session_state_transition_tx, wave_create_tx, wave_delete_tx, wave_update_tx,
    worker_session_from_row,
};
use crate::db::RepoSyncDomainRaw;
use crate::error::{CalmError, Result};
use crate::ids::{CoveId, WaveId};
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
            r#"SELECT id, wave_id, provider, mode, contract, parent_session_id,
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
            r#"SELECT id, wave_id, provider, mode, contract, parent_session_id,
                      requester_session_id, state, mcp_token_hash, thread_id,
                      agent_session_id, active_turn_id, terminal_run_id, card_id,
                      handle_state_json, liveness, liveness_probed_at_ms,
                      exit_code, exit_interpretation, spawn_op_id,
                      last_activity_ms, last_thread_status, created_at_ms,
                      updated_at_ms, completed_at_ms
               FROM worker_sessions
               WHERE state IN ('starting', 'running', 'idle', 'turn_pending')
               ORDER BY wave_id ASC, created_at_ms ASC, id ASC"#,
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
    ) -> Result<()> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        session_record_activity_by_thread_tx(
            &mut tx,
            thread_id,
            last_activity_ms,
            last_thread_status,
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

    async fn session_list_by_wave(&self, wave_id: &WaveId) -> Result<Vec<WorkerSession>> {
        let rows = sqlx::query(
            r#"SELECT id, wave_id, provider, mode, contract, parent_session_id,
                      requester_session_id, state, mcp_token_hash, thread_id,
                      agent_session_id, active_turn_id, terminal_run_id, card_id,
                      handle_state_json, liveness, liveness_probed_at_ms,
                      exit_code, exit_interpretation, spawn_op_id,
                      last_activity_ms, last_thread_status, created_at_ms,
                      updated_at_ms, completed_at_ms
               FROM worker_sessions
               WHERE wave_id = ?1
               ORDER BY created_at_ms ASC, id ASC"#,
        )
        .bind(wave_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(worker_session_from_row).collect()
    }

    async fn dead_root_candidates(&self) -> Result<Vec<DeadRootCandidate>> {
        // The soundness predicate lives entirely here (#741-4 DR-4). Two arms,
        // both gated on a POSITIVE dead signal AND the mid-respawn exclusion
        // (no active planner-contract session). NEVER converges on absence or
        // a just-created wave.
        //
        //  * Failed-start (Draft): the wave is still `draft` AND its
        //    *most-recent* `spec-harness-start` operation resolved to
        //    `phase='failed'`. The op→wave link is the immutable
        //    `payload_json.wave_id` (`idempotency_key` is None and
        //    `target_type/id` is later rewritten to the spec card, so neither
        //    is a reliable key — the payload is stamped once at insert and
        //    never changes). Start/reset re-submit `spec-harness-start` with a
        //    FRESH op id, so a wave can carry a STALE `failed` start-op AND a
        //    NEWER retry (`pending`/`running`/`succeeded`) start-op at once;
        //    during the retry's setup window (new op submitted, planner session
        //    not yet created) `no_active_planner` is momentarily true. Keying
        //    on the LATEST start-op — `rowid = MAX(rowid)` over this wave's
        //    start-ops — closes that hole: `rowid` is SQLite's monotonic
        //    insertion order (the `operations` table is rowid-backed, not
        //    `WITHOUT ROWID`; `id` is a random uuid-v4 and `created_at_ms` is
        //    wall-clock ms that can tie, so neither orders insertions
        //    reliably). If the latest start-op is non-failed (retry in flight
        //    or a success), or there is no start-op row yet, the signal is NOT
        //    positive ⇒ left.
        //  * Lost-root (Planning): the wave is `planning` AND its root session
        //    is NULL or points at a terminal/missing session. A `Resumable`
        //    (codex) root that is still alive is `is_active_authority` ⇒ caught
        //    by the active-planner exclusion below, so a codex root is never
        //    declared dead on a bare PTY-`Exited` — only via its terminal
        //    `worker_sessions.state` (set by the worker reaper's S1/S2 arbiter).
        //
        // Dispatching/Blocked are intentionally OUT OF SCOPE (no DR-1 edge).
        let active = "('starting', 'running', 'idle', 'turn_pending')";
        let no_active_planner = format!(
            "NOT EXISTS (SELECT 1 FROM worker_sessions ws \
               WHERE ws.wave_id = w.id AND ws.contract = 'planner' \
                 AND ws.state IN {active})"
        );
        let sql = format!(
            r#"SELECT w.id AS wave_id, w.cove_id AS cove_id, w.lifecycle AS lifecycle
                 FROM waves w
                WHERE w.lifecycle = 'draft'
                  AND EXISTS (
                      SELECT 1 FROM operations o
                       WHERE o.kind = 'spec-harness-start'
                         AND o.phase = 'failed'
                         AND json_extract(o.payload_json, '$.wave_id') = w.id
                         AND o.rowid = (
                             SELECT MAX(o2.rowid) FROM operations o2
                              WHERE o2.kind = 'spec-harness-start'
                                AND json_extract(o2.payload_json, '$.wave_id') = w.id
                         )
                  )
                  AND {no_active_planner}
               UNION ALL
               SELECT w.id AS wave_id, w.cove_id AS cove_id, w.lifecycle AS lifecycle
                 FROM waves w
                WHERE w.lifecycle = 'planning'
                  AND (
                      w.root_session_id IS NULL
                      OR NOT EXISTS (
                          SELECT 1 FROM worker_sessions rs
                           WHERE rs.id = w.root_session_id
                             AND rs.state IN {active}
                      )
                  )
                  AND {no_active_planner}
               ORDER BY wave_id ASC"#
        );
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let wave_id: String = row.try_get("wave_id")?;
                let cove_id: String = row.try_get("cove_id")?;
                let lifecycle_raw: String = row.try_get("lifecycle")?;
                let lifecycle = WaveLifecycle::try_from(lifecycle_raw.clone()).map_err(|e| {
                    CalmError::Internal(format!(
                        "dead_root_candidates: unknown wave lifecycle {lifecycle_raw:?}: {e}"
                    ))
                })?;
                Ok(DeadRootCandidate {
                    wave_id: WaveId::from(wave_id),
                    cove_id: CoveId::from(cove_id),
                    lifecycle,
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// RepoSyncDomainRaw — raw entity writes for the in-scope sync domain.
// Gated: not reachable via the `RouteRepo` trait object that handlers see;
// only callable via the explicit `AppState::raw_repo()` escape hatch.
// ---------------------------------------------------------------------------

#[async_trait]
impl RepoSyncDomainRaw for SqlxRepo {
    // ---------------------------------------------------------------- coves
    async fn cove_create(&self, p: NewCove) -> Result<Cove> {
        let mut tx = self.pool.begin().await?;
        let out = cove_create_tx(&mut tx, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn cove_update(&self, id: &str, p: CovePatch) -> Result<Cove> {
        let mut tx = self.pool.begin().await?;
        let out = cove_update_tx(&mut tx, id, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn cove_delete(&self, id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        overlay_delete_subtree_by_cove_tx(&mut tx, id).await?;
        overlay_delete_by_entity_tx(&mut tx, "cove", id).await?;
        cove_delete_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(())
    }

    // ---------------------------------------------------------------- waves
    async fn wave_create(&self, p: NewWave) -> Result<Wave> {
        let mut tx = self.pool.begin().await?;
        let out = wave_create_tx(&mut tx, p, &self.wave_cove_cache).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn wave_update(&self, id: &str, p: WavePatch) -> Result<Wave> {
        let mut tx = self.pool.begin().await?;
        let out = wave_update_tx(&mut tx, id, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn wave_delete(&self, id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        overlay_delete_card_overlays_by_wave_tx(&mut tx, id).await?;
        overlay_delete_by_entity_tx(&mut tx, "wave", id).await?;
        overlay_delete_by_entity_tx(&mut tx, "view", id).await?;
        wave_delete_tx(&mut tx, id, &self.wave_cove_cache).await?;
        tx.commit().await?;
        Ok(())
    }

    // ---------------------------------------------------------------- cards
    async fn card_create(&self, p: NewCard) -> Result<Card> {
        let mut tx = self.pool.begin().await?;
        let out = card_create_tx(&mut tx, p, &self.card_role_cache).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn card_update(&self, id: &str, p: CardPatch) -> Result<Card> {
        let mut tx = self.pool.begin().await?;
        let out = card_update_tx(&mut tx, id, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn card_delete(&self, id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        card_delete_tx(&mut tx, id, &self.card_role_cache).await?;
        tx.commit().await?;
        Ok(())
    }

    // -------------------------------------------------------------- overlays
    async fn overlay_upsert(&self, p: NewOverlay) -> Result<Overlay> {
        let mut tx = self.pool.begin().await?;
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
        let mut tx = self.pool.begin().await?;
        overlay_delete_tx(&mut tx, plugin_id, entity_kind, entity_id, kind).await?;
        tx.commit().await?;
        Ok(())
    }
}
