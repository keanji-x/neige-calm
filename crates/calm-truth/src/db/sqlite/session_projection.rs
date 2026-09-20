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
use super::{SqlxRepo, begin_immediate_tx, derive_session_identity};
use crate::model::*;
use crate::session_projection_repo::{
    AgentProvider, CardId as RuntimeCardId, Result as WorkerSessionProjectionResult,
    ThreadAttribution, Tx as WorkerSessionProjectionTx, WorkerSessionKind, WorkerSessionProjection,
    WorkerSessionProjectionRepo, WorkerSessionProjectionRepoError,
};
use crate::session_projection_row::{
    ACTIVE_CARD_RUNTIME_SELECT, WS_BACKED_CARD_RUNTIME_SELECT, WS_CARD_KEYED_RUNTIME_SELECT,
    card_runtime_from_ws_join_row, projectable_runtimes_for_cards_from_rows,
    projectable_runtimes_for_cards_query, run_status_from_db,
};
use calm_types::worker::WorkerSessionState;

pub(super) async fn runtime_current_status_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &String,
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
    id: &str,
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
    // Uses `ACTIVE_CARD_RUNTIME_SELECT` so this read and the enqueued predicate
    // cannot drift apart.
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE ws.id = ({ACTIVE_CARD_RUNTIME_SELECT})
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
    id: &String,
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
    id: &String,
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
    id: &String,
    attr: ThreadAttribution,
) -> WorkerSessionProjectionResult<()> {
    if &attr.worker_session_id != id {
        return Err(runtime_message(format!(
            "runtime attribution id mismatch: arg={id}, attr={}",
            attr.worker_session_id
        )));
    }

    let now = now_ms();
    session_bind_attribution_mirror_tx(tx, id, &attr, now).await?;
    Ok(())
}

pub async fn session_clear_terminal_run_id_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &String,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    session_clear_terminal_run_id_mirror_tx(tx, id, now).await?;
    Ok(())
}

/// Returns whether the row was written; a caller promising durability must check.
pub async fn session_set_handle_state_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &String,
    state: Option<serde_json::Value>,
) -> WorkerSessionProjectionResult<bool> {
    let state_text = state.as_ref().map(serde_json::to_string).transpose()?;
    let now = now_ms();
    session_set_handle_state_mirror_tx(tx, id, &state_text, now).await
}

pub async fn session_set_active_turn_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &String,
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
    id: &String,
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
    id: &String,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    session_fail_if_active_tx(tx, id, now).await?;
    Ok(())
}

/// Records that this runtime's pending human sentences have left the
/// undelivered set; the `IS NULL` conjunct means the first stamp wins.
pub async fn session_mark_queue_harvested_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &str,
    now: i64,
) -> WorkerSessionProjectionResult<()> {
    sqlx::query(
        r#"UPDATE worker_sessions
              SET queue_harvested_at_ms = ?1
            WHERE id = ?2
              AND queue_harvested_at_ms IS NULL"#,
    )
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Move the never-delivered human sentences off this card's `superseded`
/// runtimes to the successor minted in THIS transaction. `failed` rows are
/// excluded (their caller re-sends under a retry key); the successor excludes
/// itself because a revived placeholder already holds its queue in memory.
pub async fn harvest_pending_user_messages_tx<F>(
    tx: &mut WorkerSessionProjectionTx<'_>,
    card_id: &str,
    successor_id: &str,
    now: i64,
    extract: F,
) -> WorkerSessionProjectionResult<HarvestedQueues>
where
    F: Fn(&str, &str) -> HarvestOutcome,
{
    let rows = sqlx::query(
        r#"SELECT id, handle_state_json
             FROM worker_sessions
            WHERE card_id = ?1
              AND state = 'superseded'
              AND queue_harvested_at_ms IS NULL
              AND id != ?2
            ORDER BY created_at_ms ASC, id ASC"#,
    )
    .bind(card_id)
    .bind(successor_id)
    .fetch_all(&mut **tx)
    .await?;

    let mut harvested = HarvestedQueues::default();
    for row in &rows {
        let id: String = row.try_get("id")?;
        let state: Option<String> = row.try_get("handle_state_json")?;
        if let Some(state) = state.as_deref() {
            // The decoder mints ids for entries that have none, so it must see each row exactly once.
            let outcome = extract(id.as_str(), state);
            // A MOVE, not a copy: sentences left behind would be delivered again by a second harvest.
            if let Some(remaining) = outcome.remaining_snapshot {
                session_set_handle_state_of_any_runtime_tx(tx, &id, Some(remaining), now).await?;
            }
            if !outcome.taken.is_empty() {
                harvested.taken_from.push(HarvestedFrom {
                    worker_session_id: id.clone(),
                    messages: outcome.taken.clone(),
                });
            }
            harvested.messages.extend(outcome.taken);
        }
        session_mark_queue_harvested_tx(tx, &id, now).await?;
        harvested.stamped_worker_session_ids.push(id);
    }
    Ok(harvested)
}

/// What the caller's decoder made of one retired row.
#[derive(Debug, Default, Clone)]
pub struct HarvestOutcome {
    /// The human sentences taken off this row.
    pub taken: Vec<HarvestedMessage>,
    /// `None` means the decoder took nothing and the row is left as it was.
    pub remaining_snapshot: Option<serde_json::Value>,
}

/// One source row and what this harvest took off it, so a failed mint can put
/// it back where it came from rather than somewhere plausible.
#[derive(Debug, Default, Clone)]
pub struct HarvestedFrom {
    pub worker_session_id: String,
    pub messages: Vec<HarvestedMessage>,
}

pub async fn session_handle_state_by_id_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &str,
) -> WorkerSessionProjectionResult<Option<serde_json::Value>> {
    let row: Option<Option<String>> =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id = ?1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(row
        .flatten()
        .map(|text| serde_json::from_str(&text))
        .transpose()?)
}

/// Writes `handle_state_json` whatever state the row is in: the harvest holds
/// the row for its transaction and needs neither writer's restriction.
pub async fn session_set_handle_state_of_any_runtime_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &str,
    state: Option<serde_json::Value>,
    now: i64,
) -> WorkerSessionProjectionResult<()> {
    let state_text = state.as_ref().map(serde_json::to_string).transpose()?;
    sqlx::query(
        r#"UPDATE worker_sessions
              SET handle_state_json = ?1,
                  updated_at_ms = ?2
            WHERE id = ?3"#,
    )
    .bind(&state_text)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// What one harvest took, and from where. The ids let a mint that fails after
/// harvesting give the sentences back.
#[derive(Debug, Default, Clone)]
pub struct HarvestedQueues {
    pub messages: Vec<HarvestedMessage>,
    pub stamped_worker_session_ids: Vec<String>,
    /// The undo journal: per source row, what was taken off it.
    pub taken_from: Vec<HarvestedFrom>,
}

/// One queue entry taken off a retired row. `ids` is never empty: the decoder
/// mints one for entries enqueued before the field existed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HarvestedMessage {
    pub text: String,
    pub ids: Vec<String>,
    /// The `QueueEntryId` this sentence already had, kept across the move so a
    /// browser holding it is not left addressing a renamed entry. `None`: a legacy
    /// entry, which gains one on arrival.
    pub entry_id: Option<String>,
}

/// Compensating half of [`harvest_pending_user_messages_tx`]: a `thread/start`
/// failing after the mint commits leaves sentences on a `failed` row the
/// harvest never reads.
pub async fn session_clear_queue_harvested_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &str,
) -> WorkerSessionProjectionResult<()> {
    sqlx::query(
        r#"UPDATE worker_sessions
              SET queue_harvested_at_ms = NULL
            WHERE id = ?1"#,
    )
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Writes `handle_state_json` of a retired row and nothing else, so it cannot
/// revive a row the fence retired; refuses ACTIVE rows because a revived id
/// belongs to a different harness. Returns whether the row was written: a row
/// that flipped back to active matches NEITHER writer, and the caller must know.
pub async fn session_set_handle_state_of_retired_runtime_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &str,
    state: Option<serde_json::Value>,
    now: i64,
) -> WorkerSessionProjectionResult<bool> {
    let state_text = state.as_ref().map(serde_json::to_string).transpose()?;
    let res = sqlx::query(
        r#"UPDATE worker_sessions
              SET handle_state_json = ?1,
                  updated_at_ms = ?2
            WHERE id = ?3
              AND state NOT IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(&state_text)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// Tolerant harness phase-mirror / compensation write; deliberately skips the
/// runtime status matrix and emits no event.
pub async fn session_mark_superseded_runtime_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &String,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    session_mark_superseded_tx(tx, id, now).await?;
    Ok(())
}

/// Tolerant harness phase-mirror / compensation write; deliberately skips the
/// runtime status matrix and emits no event.
pub async fn session_restore_from_superseded_runtime_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &String,
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
    id: &String,
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

    async fn session_projection_system_error_recovery_matches(
        &self,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
    ) -> WorkerSessionProjectionResult<bool> {
        let Some(snapshot) = runtime.handle_state_json.as_ref() else {
            return Ok(false);
        };
        Ok(super::session_system_error_recovery_matches(
            &self.pool,
            &runtime.card_id,
            &runtime.id,
            thread_id,
            snapshot,
        )
        .await?)
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

    async fn session_projection_state_by_id(
        &self,
        id: &str,
    ) -> WorkerSessionProjectionResult<Option<WorkerSessionState>> {
        let row: Option<String> =
            sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id = ?1")
                .bind(id)
                .fetch_optional(self.pool())
                .await?;
        row.as_deref().map(run_status_from_db).transpose()
    }

    async fn session_projection_handle_state_by_id(
        &self,
        id: &str,
    ) -> WorkerSessionProjectionResult<Option<serde_json::Value>> {
        let row: Option<Option<String>> =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id = ?1")
                .bind(id)
                .fetch_optional(self.pool())
                .await?;
        Ok(row
            .flatten()
            .map(|text| serde_json::from_str(&text))
            .transpose()?)
    }

    async fn session_projection_by_id(
        &self,
        id: &str,
    ) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
        runtime_get_by_id_from_pool(&self.pool, id).await
    }

    async fn session_projection_set_status_for_card(
        &self,
        card_id: &str,
        status: WorkerSessionState,
    ) -> WorkerSessionProjectionResult<()> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        session_set_status_for_card_tx(&mut tx, card_id, status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn session_projection_complete_for_card(
        &self,
        card_id: &str,
        terminal_status: WorkerSessionState,
    ) -> WorkerSessionProjectionResult<()> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        session_complete_for_card_tx(&mut tx, card_id, terminal_status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn session_projection_complete_for_terminal(
        &self,
        terminal_id: &str,
        terminal_status: WorkerSessionState,
    ) -> WorkerSessionProjectionResult<()> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        session_complete_for_terminal_tx(&mut tx, terminal_id, terminal_status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn session_projection_recover_harnesses_on_boot(
        &self,
    ) -> WorkerSessionProjectionResult<Vec<WorkerSessionProjection>> {
        let (provider, _mode, contract) =
            derive_session_identity(&WorkerSessionKind::SharedPlanner);
        let sql = format!(
            r#"{WS_BACKED_CARD_RUNTIME_SELECT}
               JOIN tracks w ON w.id = c.track_id
               WHERE ws.provider = ?1
                 AND (
                       ws.contract = ?2
                       -- #1098 — an area chat. Executor contract, worker role,
                       -- `plain_chat` marker.
                       OR (ws.contract = 'executor'
                           AND c.role = 'worker'
                           AND c.kind = 'codex'
                           AND json_extract(c.payload, '$.harness_profile') = 'plain_chat')
                       -- #1189 — a track assistant. Structurally a sibling of the
                       -- clause above (same executor contract, its own role +
                       -- marker pair) and NOT covered by it: an assistant
                       -- matches neither `contract = 'planner'` nor
                       -- `role = 'worker'` nor the `plain_chat` marker, so
                       -- without this arm a kernel restart mid-turn leaves the
                       -- `worker_sessions` row running with no harness in
                       -- memory: `GET /planner/run` answers dormant, and the reply
                       -- to the in-flight turn is lost for good because no run
                       -- loop is behind it any more. The conversation itself is
                       -- not permanently stranded — the next `POST /planner/input`
                       -- goes through `ensure_live_planner_harness`, which does not
                       -- consult this selector and lazily rebuilds the harness —
                       -- so the damage is one silently dropped turn plus a
                       -- dormant-looking card until the user pokes it again.
                       -- The literals below are pinned from the Rust side by
                       -- `planner_harness_start_adapter::tests::
                       -- boot_recovery_sql_literals_track_the_minted_card_shape`.
                       OR (ws.contract = 'executor'
                           AND c.role = 'assistant'
                           AND c.kind = 'codex'
                           AND json_extract(c.payload, '$.harness_profile') = 'assistant')
                 )
                 AND ws.state IN ('starting','running','idle','turn_pending')
                 AND ws.thread_id IS NOT NULL
                 AND ws.handle_state_json IS NOT NULL
                 AND json_extract(ws.handle_state_json, '$.mode') = 'harness'
                 -- Keep harness boot recovery aligned with the legacy
                 -- takeover filters above: terminal tracks must stay inert.
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
