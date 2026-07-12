use sqlx::Sqlite;
use sqlx::Transaction;

use super::infra::next_sort_scoped_in_tx;
use super::session_row::{
    WorkerSessionDeleteScope, clear_wave_root_session_refs_for_worker_session_delete_tx,
};
use crate::error::{CalmError, Result};
use crate::ids::WaveId;
use crate::model::*;
use crate::wave_cove_cache::WaveCoveCache;

pub async fn wave_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    p: NewWave,
    wave_cove_cache: &WaveCoveCache,
) -> Result<Wave> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM coves WHERE id = ?1")
        .bind(p.cove_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("cove {}", p.cove_id)));
    }

    let sort = match p.sort {
        Some(s) => s,
        None => {
            next_sort_scoped_in_tx(tx, "waves", "WHERE cove_id = ?1", Some(p.cove_id.as_ref()))
                .await?
        }
    };
    let now = now_ms();
    let id = new_id();
    // Issue #145 — new waves seed at `lifecycle = 'draft'`. The DB
    // DEFAULT in migration 0012 also pins this, but stamping it
    // explicitly here matches the "required field, no Option" model:
    // every wave-create path declares the seed lifecycle in code so a
    // future change to the seed value can't be reached by skipping
    // the column from the INSERT list.
    let lifecycle = crate::model::WaveLifecycle::Draft;
    // Issue #250 PR 2 — `cwd` lands on the row verbatim from `NewWave`.
    // The route layer (`POST /api/waves`) already validated absolute-
    // path shape + cove-folder ownership; this writer stays mechanical.
    // `terminal_at` is `NULL` on every fresh wave (Draft is non-terminal
    // by construction; `WaveLifecycle::is_terminal` returns false for it).
    sqlx::query(
        r#"INSERT INTO waves
           (id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, workflow_id, purpose, workflow_input, terminal_at, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6, ?7, NULL, ?8, NULL, ?9, ?10)"#,
    )
    .bind(&id)
    .bind(p.cove_id.as_str())
    .bind(&p.title)
    .bind(sort)
    .bind(lifecycle.as_db_str())
    .bind(&p.cwd)
    .bind(p.workflow_id.as_deref())
    .bind(p.workflow_input.as_ref().map(|v| v.to_string()))
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    // #234 — write-through into the wave→cove cache. Same semantics as
    // the `card_role_cache` write-through in `card_create_with_id_tx`: a
    // follow-up emit inside the same `write_with_event` closure can
    // see the freshly-minted binding via `enforce_role`'s lookup.
    let wave_id: WaveId = id.clone().into();
    wave_cove_cache.insert(wave_id.clone(), p.cove_id.clone());
    Ok(Wave {
        id: wave_id,
        cove_id: p.cove_id,
        title: p.title,
        sort,
        archived_at: None,
        pinned_at: None,
        lifecycle,
        cwd: p.cwd,
        workflow_id: p.workflow_id,
        purpose: None,
        workflow_input: p.workflow_input,
        terminal_at: None,
        created_at: now,
        updated_at: now,
    })
}

pub async fn wave_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: WavePatch,
) -> Result<Wave> {
    let mut w = sqlx::query_as::<_, crate::db::rows::WaveRow>(
        r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, workflow_id, purpose, workflow_input, terminal_at, created_at, updated_at
           FROM waves WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .map(Wave::from)
    .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;

    if let Some(v) = p.title {
        w.title = v;
    }
    if let Some(v) = p.sort {
        w.sort = v;
    }
    if let Some(v) = p.archived_at {
        w.archived_at = v;
    }
    if let Some(v) = p.pinned_at {
        w.pinned_at = v;
    }
    // Issue #145 — `WavePatch.lifecycle` is applied here, but the
    // transition is validated by `validate_transition` at the call
    // site (REST handler / MCP tool), *outside* the DB layer. Routing
    // the validator through the route boundary (rather than this
    // function) keeps `wave_update_tx` a pure mechanical row write
    // and avoids threading `ActorId` through every call site that
    // patches the row. Production code paths that mutate
    // `lifecycle` must call `validate_transition` first.
    //
    // Issue #250 PR 2 — `terminal_at` rides on the lifecycle column:
    // when this patch advances the wave into a terminal state we
    // stamp the current time; when it reopens a terminal wave
    // (terminal → planning, the only legal reopen edge today) we
    // clear `terminal_at` back to NULL. A patch that doesn't touch
    // `lifecycle` leaves `terminal_at` alone — that matches the
    // archive precedent (changing `title` doesn't bump `archived_at`).
    // The stamp happens inside the same transaction as the wave row
    // update and the caller's `WaveLifecycleChanged` event, so a
    // mid-tx crash leaves none of them behind.
    if let Some(new_lifecycle) = p.lifecycle {
        if new_lifecycle != w.lifecycle {
            if new_lifecycle.is_terminal() {
                w.terminal_at = Some(now_ms());
            } else if w.lifecycle.is_terminal() {
                // Reopen (terminal → non-terminal). Today the only
                // legal edge here is `terminal → planning` (user-
                // driven, gated by `validate_transition`). Clearing
                // the stamp ensures a reopened wave doesn't render
                // with a stale terminal date on the calendar.
                w.terminal_at = None;
            }
        }
        w.lifecycle = new_lifecycle;
    }
    w.updated_at = now_ms();

    sqlx::query(
        r#"UPDATE waves
           SET title = ?1, sort = ?2, archived_at = ?3, pinned_at = ?4,
               lifecycle = ?5, terminal_at = ?6, updated_at = ?7
           WHERE id = ?8"#,
    )
    .bind(&w.title)
    .bind(w.sort)
    .bind(w.archived_at)
    .bind(w.pinned_at)
    .bind(w.lifecycle.as_db_str())
    .bind(w.terminal_at)
    .bind(w.updated_at)
    .bind(w.id.as_str())
    .execute(&mut **tx)
    .await?;

    // Issue #644 — scheduler budget + gate policy (migration 0041).
    // These columns deliberately do NOT live on the `Wave` struct while
    // the plan is inert (PR-A): keeping them off the struct leaves every
    // `SELECT` column list, the `WaveUpdated` wire payload, and the
    // ts-rs export untouched. Targeted single-column writes here are the
    // whole PATCH surface; the PR-B scheduler reads the columns by SQL.
    if let Some(budget) = p.task_budget {
        sqlx::query("UPDATE waves SET task_budget = ?1 WHERE id = ?2")
            .bind(budget)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
    }
    if let Some(require_gates) = p.require_task_gates {
        sqlx::query("UPDATE waves SET require_task_gates = ?1 WHERE id = ?2")
            .bind(require_gates)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
    }
    Ok(w)
}

pub async fn wave_delete_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    wave_cove_cache: &WaveCoveCache,
) -> Result<()> {
    sqlx::query("DELETE FROM wave_vcs_refs WHERE wave_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM wave_vcs_commits WHERE wave_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    // #644 — `tasks.wave_id` has no FK to `waves` (events-outlive-rows
    // convention, design §2), so plan rows must be deleted explicitly
    // alongside the other no-FK wave-owned tables above.
    sqlx::query("DELETE FROM tasks WHERE wave_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    clear_wave_root_session_refs_for_worker_session_delete_tx(
        tx,
        WorkerSessionDeleteScope::Wave { wave_id: id },
    )
    .await?;
    // `worker_sessions.wave_id` is a required FK. Card/runtime rows may
    // cascade below, but sessions must leave before the wave row itself.
    sqlx::query("DELETE FROM worker_sessions WHERE wave_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    let res = sqlx::query("DELETE FROM waves WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("wave {id}")));
    }
    // #234 — keep the wave→cove cache in lockstep with the table. Mirror
    // of the card-delete-side write-through in `card_delete_tx`.
    wave_cove_cache.remove(&WaveId::from(id));
    Ok(())
}
