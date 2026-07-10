use std::collections::HashMap;

use async_trait::async_trait;
use sqlx::Row;
use sqlx::SqlitePool;

use super::session_mirror::{
    ensure_runtime_status_transition, session_bind_attribution_mirror_tx,
    session_clear_terminal_run_id_mirror_tx, session_complete_mirror_tx, session_fail_if_active_tx,
    session_mark_superseded_tx, session_repoint_current_links_tx,
    session_restore_from_superseded_tx, session_set_active_turn_mirror_tx,
    session_set_handle_state_mirror_tx, session_set_harness_observation_tx,
    session_set_status_mirror_tx,
};
use super::session_row::{agent_provider_to_db, runtime_message};
use super::{SqlxRepo, derive_session_identity};
use crate::model::*;
use crate::session_projection_repo::{
    AgentProvider, CardId as RuntimeCardId, Result as WorkerSessionProjectionResult, RuntimeId,
    ThreadAttribution, Tx as WorkerSessionProjectionTx, WorkerSessionKind, WorkerSessionProjection,
    WorkerSessionProjectionRepo, WorkerSessionProjectionRepoError,
};
use crate::session_projection_row::{
    WS_BACKED_CARD_RUNTIME_SELECT, WS_CARD_KEYED_RUNTIME_SELECT, card_runtime_from_ws_join_row,
    projectable_runtimes_for_cards_from_rows, projectable_runtimes_for_cards_query,
    run_status_from_db,
};
use calm_types::worker::WorkerSessionState;

pub(super) async fn runtime_current_status_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
) -> WorkerSessionProjectionResult<WorkerSessionState> {
    let row = sqlx::query(
        r#"SELECT state FROM worker_sessions ws
           WHERE ws.id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Err(runtime_message(format!("runtime {id} not found")));
    };
    run_status_from_db(row.try_get::<String, _>("state")?.as_str())
}

pub(super) async fn runtime_get_by_id_from_pool(
    pool: &SqlitePool,
    id: &RuntimeId,
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE ws.id = ?1"#
    );
    let row = sqlx::query(&sql).bind(id).fetch_optional(pool).await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

pub(super) async fn runtime_get_active_for_card_from_pool(
    pool: &SqlitePool,
    card_id: &str,
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE c.id = ?1
             AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY ws.updated_at_ms DESC, ws.created_at_ms DESC, ws.id DESC
           LIMIT 1"#,
    );
    let row = sqlx::query(&sql).bind(card_id).fetch_optional(pool).await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

pub(super) async fn runtime_get_projectable_for_card_from_pool(
    pool: &SqlitePool,
    card_id: &str,
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE c.id = ?1
             AND ws.state != 'superseded'
           LIMIT 1"#,
    );
    let row = sqlx::query(&sql).bind(card_id).fetch_optional(pool).await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

pub(super) async fn runtime_get_projectable_for_cards_from_pool(
    pool: &SqlitePool,
    card_ids: &[RuntimeCardId],
) -> WorkerSessionProjectionResult<HashMap<RuntimeCardId, WorkerSessionProjection>> {
    if card_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut query = projectable_runtimes_for_cards_query(card_ids);
    let rows = query.build().fetch_all(pool).await?;
    projectable_runtimes_for_cards_from_rows(rows)
}

pub(super) async fn runtime_get_active_by_thread_from_pool(
    pool: &SqlitePool,
    provider: AgentProvider,
    thread_id: &str,
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE ws.provider = ?1 AND ws.thread_id = ?2
             AND ws.state IN ('starting','running','idle','turn_pending')
           ORDER BY ws.updated_at_ms DESC, ws.created_at_ms DESC, ws.id DESC
           LIMIT 1"#,
    );
    let row = sqlx::query(&sql)
        .bind(agent_provider_to_db(&provider))
        .bind(thread_id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

pub(super) async fn runtime_get_active_by_session_from_pool(
    pool: &SqlitePool,
    provider: AgentProvider,
    session_id: &str,
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE ws.provider = ?1 AND ws.agent_session_id = ?2
             AND ws.state IN ('starting','running','idle','turn_pending')
           ORDER BY ws.updated_at_ms DESC, ws.created_at_ms DESC, ws.id DESC
           LIMIT 1"#,
    );
    let row = sqlx::query(&sql)
        .bind(agent_provider_to_db(&provider))
        .bind(session_id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

pub(super) async fn runtime_active_shared_thread_attribution_from_pool(
    pool: &SqlitePool,
) -> WorkerSessionProjectionResult<Vec<(String, String)>> {
    sqlx::query_as::<_, (String, String)>(
        r#"SELECT ws.thread_id, c.id AS card_id
           FROM worker_sessions ws JOIN cards c ON c.session_id = ws.id
           WHERE ws.provider = 'codex' AND ws.thread_id IS NOT NULL
             AND ws.state IN ('starting','running','idle','turn_pending')
           ORDER BY ws.created_at_ms ASC, c.id ASC"#,
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

pub(super) async fn runtimes_active_for_kind_from_pool(
    pool: &SqlitePool,
    kind: WorkerSessionKind,
) -> WorkerSessionProjectionResult<Vec<WorkerSessionProjection>> {
    let (provider, _mode, contract) = derive_session_identity(&kind);
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE ws.provider = ?1
             AND ws.contract = ?2
             AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY ws.created_at_ms ASC, c.id ASC"#
    );
    let rows = sqlx::query(&sql)
        .bind(provider.as_db_str())
        .bind(contract.as_db_str())
        .fetch_all(pool)
        .await?;
    rows.iter().map(card_runtime_from_ws_join_row).collect()
}

pub async fn session_projection_by_id_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
    let sql = format!(
        r#"{WS_CARD_KEYED_RUNTIME_SELECT}
           WHERE ws.id = ?1"#
    );
    let row = sqlx::query(&sql).bind(id).fetch_optional(&mut **tx).await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

pub async fn session_projection_active_for_card_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    card_id: &str,
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
    let sql = format!(
        r#"{WS_CARD_KEYED_RUNTIME_SELECT}
           WHERE ws.card_id = ?1
             AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY ws.updated_at_ms DESC, ws.created_at_ms DESC, ws.id DESC
           LIMIT 1"#,
    );
    let row = sqlx::query(&sql)
        .bind(card_id)
        .fetch_optional(&mut **tx)
        .await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

pub async fn session_set_status_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    status: WorkerSessionState,
) -> WorkerSessionProjectionResult<()> {
    if status == WorkerSessionState::Superseded {
        return Err(WorkerSessionProjectionRepoError::IllegalStatusTransition {
            id: id.clone(),
            attempted: status,
        });
    }

    let current = runtime_current_status_tx(tx, id).await?;
    ensure_runtime_status_transition(id, &current, &status)?;

    let now = now_ms();
    session_set_status_mirror_tx(tx, id, status, now).await?;
    Ok(())
}

pub async fn session_set_status_for_card_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    card_id: &str,
    status: WorkerSessionState,
) -> WorkerSessionProjectionResult<()> {
    let Some(runtime) = session_projection_active_for_card_tx(tx, card_id).await? else {
        return Ok(());
    };
    session_set_status_tx(tx, &runtime.id, status).await
}

pub async fn session_bind_attribution_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    attr: ThreadAttribution,
) -> WorkerSessionProjectionResult<()> {
    if &attr.runtime_id != id {
        return Err(runtime_message(format!(
            "runtime attribution id mismatch: arg={id}, attr={}",
            attr.runtime_id
        )));
    }

    let now = now_ms();
    session_bind_attribution_mirror_tx(tx, id, &attr, now).await?;
    Ok(())
}

pub async fn session_clear_terminal_run_id_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    session_clear_terminal_run_id_mirror_tx(tx, id, now).await?;
    Ok(())
}

pub async fn session_set_handle_state_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    state: Option<serde_json::Value>,
) -> WorkerSessionProjectionResult<()> {
    let state_text = state.as_ref().map(serde_json::to_string).transpose()?;
    let now = now_ms();
    session_set_handle_state_mirror_tx(tx, id, &state_text, now).await?;
    Ok(())
}

pub async fn session_set_active_turn_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    turn_id: Option<&str>,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    session_set_active_turn_mirror_tx(tx, id, turn_id, now).await?;
    Ok(())
}

/// Tolerant harness phase-mirror / compensation write; deliberately skips the
/// runtime status matrix and emits no event.
pub async fn session_set_harness_observation_runtime_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    status: WorkerSessionState,
    thread_id: Option<&str>,
    active_turn_id: Option<&str>,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    session_set_harness_observation_tx(tx, id, status, thread_id, active_turn_id, now).await?;
    Ok(())
}

/// Tolerant harness phase-mirror / compensation write; deliberately skips the
/// runtime status matrix and emits no event.
pub async fn session_fail_if_active_runtime_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    session_fail_if_active_tx(tx, id, now).await?;
    Ok(())
}

/// Tolerant harness phase-mirror / compensation write; deliberately skips the
/// runtime status matrix and emits no event.
pub async fn session_mark_superseded_runtime_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    session_mark_superseded_tx(tx, id, now).await?;
    Ok(())
}

/// Tolerant harness phase-mirror / compensation write; deliberately skips the
/// runtime status matrix and emits no event.
pub async fn session_restore_from_superseded_runtime_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    status: WorkerSessionState,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    let session = session_restore_from_superseded_tx(tx, id, status, now).await?;
    let runtime = session_projection_by_id_tx(tx, id)
        .await?
        .ok_or_else(|| runtime_message(format!("worker session {id} missing after restore")))?;
    session_repoint_current_links_tx(tx, &runtime.card_id, &session).await
}

pub async fn session_complete_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    terminal_status: WorkerSessionState,
) -> WorkerSessionProjectionResult<()> {
    if !matches!(
        terminal_status,
        WorkerSessionState::Failed | WorkerSessionState::Exited
    ) {
        return Err(WorkerSessionProjectionRepoError::IllegalStatusTransition {
            id: id.clone(),
            attempted: terminal_status,
        });
    }

    let current = runtime_current_status_tx(tx, id).await?;
    ensure_runtime_status_transition(id, &current, &terminal_status)?;

    let now = now_ms();
    session_complete_mirror_tx(tx, id, terminal_status, now).await?;
    Ok(())
}

pub async fn session_complete_for_card_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    card_id: &str,
    terminal_status: WorkerSessionState,
) -> WorkerSessionProjectionResult<()> {
    let Some(runtime) = session_projection_active_for_card_tx(tx, card_id).await? else {
        return Ok(());
    };
    session_complete_tx(tx, &runtime.id, terminal_status).await
}

pub async fn session_projection_active_for_terminal_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    terminal_id: &str,
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE ws.terminal_run_id = ?1
             AND ws.state IN ('starting','running','idle','turn_pending')
           ORDER BY ws.updated_at_ms DESC, ws.created_at_ms DESC, ws.id DESC
           LIMIT 1"#,
    );
    let row = sqlx::query(&sql)
        .bind(terminal_id)
        .fetch_optional(&mut **tx)
        .await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

pub async fn session_complete_for_terminal_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    terminal_id: &str,
    terminal_status: WorkerSessionState,
) -> WorkerSessionProjectionResult<()> {
    let Some(runtime) = session_projection_active_for_terminal_tx(tx, terminal_id).await? else {
        return Ok(());
    };
    session_complete_tx(tx, &runtime.id, terminal_status).await
}

#[async_trait]
impl WorkerSessionProjectionRepo for SqlxRepo {
    async fn session_projection_active_by_thread(
        &self,
        provider: AgentProvider,
        thread_id: &str,
    ) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
        runtime_get_active_by_thread_from_pool(&self.pool, provider, thread_id).await
    }

    async fn session_projection_active_by_session(
        &self,
        provider: AgentProvider,
        session_id: &str,
    ) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
        runtime_get_active_by_session_from_pool(&self.pool, provider, session_id).await
    }

    async fn session_projection_active_for_card(
        &self,
        card_id: &crate::session_projection_repo::CardId,
    ) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
        runtime_get_active_for_card_from_pool(&self.pool, card_id).await
    }

    async fn session_projection_projectable_for_card(
        &self,
        card_id: &crate::session_projection_repo::CardId,
    ) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
        runtime_get_projectable_for_card_from_pool(&self.pool, card_id).await
    }

    async fn session_projection_projectable_for_cards(
        &self,
        card_ids: &[crate::session_projection_repo::CardId],
    ) -> WorkerSessionProjectionResult<
        HashMap<crate::session_projection_repo::CardId, WorkerSessionProjection>,
    > {
        runtime_get_projectable_for_cards_from_pool(&self.pool, card_ids).await
    }

    async fn session_projection_active_shared_thread_attribution(
        &self,
    ) -> WorkerSessionProjectionResult<Vec<(String, String)>> {
        runtime_active_shared_thread_attribution_from_pool(&self.pool).await
    }

    async fn session_projection_active_for_kind(
        &self,
        kind: WorkerSessionKind,
    ) -> WorkerSessionProjectionResult<Vec<WorkerSessionProjection>> {
        runtimes_active_for_kind_from_pool(&self.pool, kind).await
    }

    async fn session_projection_by_id(
        &self,
        id: &RuntimeId,
    ) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
        runtime_get_by_id_from_pool(&self.pool, id).await
    }

    async fn session_projection_set_status_for_card(
        &self,
        card_id: &str,
        status: WorkerSessionState,
    ) -> WorkerSessionProjectionResult<()> {
        let mut tx = self.pool.begin().await?;
        session_set_status_for_card_tx(&mut tx, card_id, status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn session_projection_complete_for_card(
        &self,
        card_id: &str,
        terminal_status: WorkerSessionState,
    ) -> WorkerSessionProjectionResult<()> {
        let mut tx = self.pool.begin().await?;
        session_complete_for_card_tx(&mut tx, card_id, terminal_status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn session_projection_complete_for_terminal(
        &self,
        terminal_id: &str,
        terminal_status: WorkerSessionState,
    ) -> WorkerSessionProjectionResult<()> {
        let mut tx = self.pool.begin().await?;
        session_complete_for_terminal_tx(&mut tx, terminal_id, terminal_status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn session_projection_recover_harnesses_on_boot(
        &self,
    ) -> WorkerSessionProjectionResult<Vec<WorkerSessionProjection>> {
        let (provider, _mode, contract) = derive_session_identity(&WorkerSessionKind::SharedSpec);
        let sql = format!(
            r#"{WS_BACKED_CARD_RUNTIME_SELECT}
               JOIN waves w ON w.id = c.wave_id
               WHERE ws.provider = ?1
                 AND ws.contract = ?2
                 AND ws.state IN ('starting','running','idle','turn_pending')
                 AND ws.thread_id IS NOT NULL
                 AND ws.handle_state_json IS NOT NULL
                 AND json_extract(ws.handle_state_json, '$.mode') = 'harness'
                 -- Keep harness boot recovery aligned with the legacy
                 -- takeover filters above: terminal waves must stay inert.
                 AND w.lifecycle NOT IN ('done', 'canceled', 'failed')
               ORDER BY ws.created_at_ms ASC, c.id ASC"#
        );
        let rows = sqlx::query(&sql)
            .bind(provider.as_db_str())
            .bind(contract.as_db_str())
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(card_runtime_from_ws_join_row)
            .collect::<WorkerSessionProjectionResult<Vec<_>>>()
    }
}
