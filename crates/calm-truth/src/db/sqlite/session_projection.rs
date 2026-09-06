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
    // The active-runtime choice itself is not restated here: it is
    // `ACTIVE_CARD_RUNTIME_SELECT`, the same statement the
    // `harness.user_message.enqueued` predicate embeds, so the runtime this read
    // reports and the runtime that predicate scopes its evidence to cannot drift
    // apart (#1314).
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

/// Returns whether the row was written; see
/// [`session_set_handle_state_mirror_tx`] for why a caller has to care.
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

/// #1449 — record that this runtime's still-pending human sentences have left
/// the undelivered set, either because a successor inherited its whole queue or
/// because [`harvest_pending_user_messages_tx`] took them.
///
/// The `IS NULL` conjunct means the first stamp wins and a second one is a
/// no-op, so the timestamp answers *when the queue stopped being deliverable*.
///
/// This function writes the marker only. `harvest_pending_user_messages_tx`
/// edits the predecessor's snapshot in the same transaction, and
/// `session_restore_from_superseded_tx` clears the marker so a restored row can
/// be harvested again.
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

/// #1449 — the human sentences that never reached an agent, taken off this
/// card's superseded runtimes and handed to the successor being minted in THIS
/// transaction.
///
/// The predicate is `state = 'superseded'` ONLY, which is narrower than the
/// path table in the design's §3 reads: a dormant or `exited` predecessor is
/// not harvested, so the residual documented at
/// `user_message_enqueued_on_active_runtime` has a smaller membership than
/// "every replacement".
///
/// The exclusion of `'failed'` is deliberate. It covers the message a failed
/// mint carried: that caller got a non-2xx and re-sends the same text under a
/// `#N` retry key, so harvesting the row would deliver it twice.
///
/// KNOWN GAP (#1449): the argument above is about the mint's own first
/// message, and a `failed` row can hold sentences it does not cover. A
/// `POST /planner/input` that answered 200 is persisted on the row, and its
/// caller has been told the send succeeded; if that runtime later goes
/// `failed`, this predicate skips the row and nothing re-sends the sentence.
/// `routes/today_summary.rs` records a bootstrap case of the same shape, where
/// a predicate re-derives the missing work; a human sentence has no such
/// re-derivation.
///
/// Every row the read touched is stamped, including the ones that yielded
/// nothing — the stamp records "this queue has left the undelivered set", not
/// "this queue had something in it". Read, harvest and stamp share the caller's
/// transaction with the successor's insert, so they commit or roll back
/// together and a second restart can only ever see the stamp.
///
/// # Why the decoder is a parameter
///
/// `handle_state_json` holds a `HarnessSnapshot`, which is a `calm-server`
/// type; this crate cannot name it. Passing the decoder in keeps the read and
/// the stamp atomic *here* rather than handing the caller a row list it could
/// forget to stamp. `extract` receives the runtime id (for its own warn line)
/// and the raw snapshot text, and answers with the sentences to carry forward.
///
/// # Why the successor excludes itself
///
/// A deferred mint's placeholder row can be superseded by a runtime that raced
/// in during the deferred window, and the insert that follows this call revives
/// it under the SAME id. At this instant it is therefore `superseded` and
/// unstamped while the successor already holds its queue in memory — harvesting
/// it would hand the successor a second copy of its own sentences.
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
            // #1449 — the decoder mints ids for entries that have none, so it
            // must see each row once. `worker_sessions.id` is the table's
            // `TEXT PRIMARY KEY` and this is a single `SELECT` over it, so the
            // ids this loop walks are distinct.
            let outcome = extract(id.as_str(), state);
            // #1449 S2 — a MOVE, not a copy. The source row keeps whatever the
            // caller did not take and loses what it did, in this transaction.
            // Leaving the taken sentences behind is what let a second harvest,
            // or a re-driven operation carrying an older snapshot, deliver them
            // again.
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
    /// The row's snapshot with those sentences removed, to be written back.
    /// `None` means the decoder took nothing and the row is left byte-for-byte
    /// as it was; `taken` is empty whenever this is.
    pub remaining_snapshot: Option<serde_json::Value>,
}

/// One source row and what this harvest took off it, so a failed mint can put
/// it back where it came from rather than somewhere plausible.
#[derive(Debug, Default, Clone)]
pub struct HarvestedFrom {
    pub worker_session_id: String,
    pub messages: Vec<HarvestedMessage>,
}

/// #1449 S3 — a runtime's persisted snapshot, inside a transaction, by id.
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

/// #1449 S2 — write a runtime's `handle_state_json` whatever state its row is
/// in.
///
/// The ordinary writer refuses non-active rows and the retired-runtime writer
/// refuses active ones; the harvest needs neither restriction, because it is
/// the transaction that is taking the queue and it holds the row for the
/// duration. Kept separate from both so that neither of their predicates has to
/// be widened for this one caller.
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

/// What one [`harvest_pending_user_messages_tx`] call took, and from where.
///
/// The ids are not diagnostics: the caller's saga has to be able to give them
/// back. A mint that harvests and then fails leaves the harvested sentences on
/// a `failed` successor — a state the harvest predicate deliberately never
/// reads — while the rows they came from are stamped, so without an undo the
/// sentences are unreachable for good and nothing reports it. See
/// [`session_clear_queue_harvested_tx`].
#[derive(Debug, Default, Clone)]
pub struct HarvestedQueues {
    pub messages: Vec<HarvestedMessage>,
    pub stamped_worker_session_ids: Vec<String>,
    /// Per source row, what was taken off it. The undo journal: a mint that
    /// fails after this transaction commits has to put each sentence back on
    /// the row it came from.
    pub taken_from: Vec<HarvestedFrom>,
}

/// One queue entry taken off a retired row, with the identity of the instances
/// it carries.
///
/// `ids` is never empty: the decoder mints one for an entry that was enqueued
/// before the field existed, in the transaction that moves it, so everything
/// this carries can be identified and therefore given back.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HarvestedMessage {
    pub text: String,
    pub ids: Vec<String>,
    /// #1505 PR4 review — the addressable `QueueEntryId` this sentence already
    /// had, carried across the move so it keeps it.
    ///
    /// `None` means it had none: a pre-#1505 entry, which gains one on arrival
    /// and can then be edited for the first time. That is the case the old
    /// unconditional re-mint was written for, and it still behaves that way.
    ///
    /// What the re-mint also did — and must not — is take a live id away from
    /// an entry that had one. A browser holding that id after a harvest asks
    /// the server about an entry the server has renamed: `GET /planner/run`
    /// lists the new id, so the message is drawn twice; an edit or a delete
    /// against the old one 404s, which this UI reports as "already left the
    /// queue" about a message that is still queued and still going to be sent.
    /// Identity that changes under the holder is not identity.
    pub entry_id: Option<String>,
}

/// #1449 — give a harvested queue back, because the mint that took it did not
/// survive.
///
/// The compensating half of [`harvest_pending_user_messages_tx`]. The mint
/// transaction's own rollback covers only a failure *inside* that transaction;
/// a `thread/start` that fails afterwards is compensated in a DIFFERENT
/// transaction, and that compensation marks the successor `failed`. The
/// sentences would then be sitting on a row the harvest never reads, taken from
/// rows that are stamped: silent, permanent loss.
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

/// #1449 — record what a runtime still owes, on a row the ordinary snapshot
/// writer refuses to touch.
///
/// [`session_set_handle_state_tx`] carries
/// `AND state IN ('starting','running','idle','turn_pending')`, so the moment a
/// fence flips a row to `superseded` that runtime's snapshot writes silently
/// affect zero rows — and `persist_snapshot_inner` additionally returns early
/// once `shutting_down` is set. Both gates land BEFORE the run loop finishes
/// the turn it is issuing, so the last thing written about a retired runtime is
/// "the batch is still queued", whether or not the daemon has it.
///
/// That was harmless while nothing read an abandoned snapshot. It is not
/// harmless now that the successor harvests it. This writer is the exception,
/// and it is deliberately the narrowest one that closes the hole: it writes
/// `handle_state_json` and nothing else — no `state`, no `active_turn_id`, no
/// phase event — so it cannot revive a row the fence retired, which is what the
/// predicate on the ordinary writer exists to prevent.
///
/// It refuses ACTIVE rows for the mirror-image reason: a row that has been
/// revived under the same id (a refreshed deferred placeholder) belongs to a
/// different harness, and a dead run loop must not write its stale queue over
/// a live one.
/// Returns whether the row was written.
///
/// #1449 — the two handle-state writers have complementary predicates, so a row
/// that flips from retired back to active between them (`restore_old_runtime`)
/// matches NEITHER. The retired row then keeps its PRE-drain queue, and once
/// the restore clears its marker that queue is harvestable again: the same
/// sentence delivered twice. The caller has to know the write did not land.
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
        // #930 uniform rule: writing transactions always BEGIN IMMEDIATE.
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
        // #930 uniform rule: writing transactions always BEGIN IMMEDIATE.
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
        // #930 uniform rule: writing transactions always BEGIN IMMEDIATE.
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
