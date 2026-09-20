use async_trait::async_trait;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::Transaction;

use super::{SqlxRepo, begin_immediate_tx};
use crate::area_folder_claim::AreaFolderClaim;
use crate::db::{RepoOutOfDomain, RepoRead, SharedCodexDaemonUpdate};
use crate::error::{CalmError, Result};
use crate::model::*;

/// What a card's harness transcript held, measured before it is destroyed;
/// `params_bytes` is the summed byte length of the JSON-RPC `params` payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HarnessTranscriptMeasure {
    pub item_count: i64,
    pub params_bytes: i64,
}

/// Call immediately before [`harness_items_delete_by_card_tx`] in the same transaction.
pub async fn harness_items_measure_by_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<HarnessTranscriptMeasure> {
    // `LENGTH(CAST(params AS BLOB))` is byte length; bare `LENGTH` on TEXT
    // counts characters, which would undercount every non-ASCII transcript.
    let row = sqlx::query(
        r#"SELECT COUNT(*) AS item_count,
                  COALESCE(SUM(LENGTH(CAST(params AS BLOB))), 0) AS params_bytes
           FROM harness_items
           WHERE card_id = ?1"#,
    )
    .bind(card_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(HarnessTranscriptMeasure {
        item_count: row.get::<i64, _>("item_count"),
        params_bytes: row.get::<i64, _>("params_bytes"),
    })
}

pub async fn harness_items_delete_by_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<()> {
    sqlx::query("DELETE FROM harness_items WHERE card_id = ?1")
        .bind(card_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn worker_flow_item_insert_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: Option<&str>,
    captured_session_id: Option<&str>,
    track_id: Option<&str>,
    worker_session_id: Option<&str>,
    kind: &str,
    payload: &str,
    created_at_ms: i64,
) -> Result<i64> {
    let row = sqlx::query(
        r#"INSERT INTO worker_flow_items (
               card_id, captured_session_id, track_id, worker_session_id,
               kind, payload, created_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
           RETURNING id"#,
    )
    .bind(card_id)
    .bind(captured_session_id)
    .bind(track_id)
    .bind(worker_session_id)
    .bind(kind)
    .bind(payload)
    .bind(created_at_ms)
    .fetch_one(&mut **tx)
    .await?;
    Ok(row.get::<i64, _>("id"))
}

pub async fn worker_flow_items_delete_by_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<()> {
    sqlx::query("DELETE FROM worker_flow_items WHERE card_id = ?1")
        .bind(card_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[async_trait]
impl RepoOutOfDomain for SqlxRepo {
    async fn track_recipe_create(&self, p: NewTrackRecipe) -> Result<TrackRecipe> {
        // A deferred tx that reads then writes can lose the lock upgrade and surface
        // as SQLITE_BUSY with nothing safe to retry, so writers BEGIN IMMEDIATE.
        let mut tx = super::infra::begin_immediate_tx(&self.pool).await?;
        let out = super::track_recipe::track_recipe_create_tx(&mut tx, &p.title, &p.body).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn track_recipe_update(
        &self,
        id: &str,
        p: NewTrackRecipe,
        if_revision: i64,
    ) -> Result<TrackRecipe> {
        let mut tx = super::infra::begin_immediate_tx(&self.pool).await?;
        let out = super::track_recipe::track_recipe_update_tx(
            &mut tx,
            id,
            &p.title,
            &p.body,
            if_revision,
        )
        .await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn track_recipe_get(&self, id: &str) -> Result<Option<TrackRecipe>> {
        let row: Option<(String, String, String, i64, i64, i64)> = sqlx::query_as(
            "SELECT id, title, body, revision, created_at, updated_at \
             FROM track_recipes WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(id, title, body, revision, created_at, updated_at)| TrackRecipe {
                id,
                title,
                body,
                revision,
                created_at,
                updated_at,
            },
        ))
    }

    async fn track_create_idempotency_get(
        &self,
        area_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<super::TrackCreateBinding>> {
        super::track::track_create_idempotency_get_pool(&self.pool, area_id, idempotency_key).await
    }

    async fn track_recipe_delete(&self, id: &str) -> Result<()> {
        let mut tx = super::infra::begin_immediate_tx(&self.pool).await?;
        super::track_recipe::track_recipe_delete_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn track_recipe_list(&self) -> Result<Vec<TrackRecipe>> {
        let rows: Vec<(String, String, String, i64, i64, i64)> = sqlx::query_as(
            "SELECT id, title, body, revision, created_at, updated_at \
             FROM track_recipes ORDER BY created_at DESC, id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, title, body, revision, created_at, updated_at)| TrackRecipe {
                    id,
                    title,
                    body,
                    revision,
                    created_at,
                    updated_at,
                },
            )
            .collect())
    }

    async fn terminal_create(&self, p: NewTerminal) -> Result<Terminal> {
        let owner: Option<(String,)> = sqlx::query_as("SELECT id FROM cards WHERE id = ?1")
            .bind(p.card_id.as_str())
            .fetch_optional(&self.pool)
            .await?;
        if owner.is_none() {
            return Err(CalmError::NotFound(format!("card {}", p.card_id)));
        }
        let dup: Option<(String,)> = sqlx::query_as("SELECT id FROM terminals WHERE card_id = ?1")
            .bind(p.card_id.as_str())
            .fetch_optional(&self.pool)
            .await?;
        if dup.is_some() {
            return Err(CalmError::Conflict(format!(
                "terminal already exists for card {}",
                p.card_id
            )));
        }

        let now = now_ms();
        let id = new_id();
        let env_text = serde_json::to_string(&p.env)?;
        let theme_fg = p.theme.fg_arg();
        let theme_bg = p.theme.bg_arg();
        sqlx::query(
            r#"INSERT INTO terminals
                   (id, card_id, program, cwd, env, pid, theme_fg, theme_bg, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8)"#,
        )
        .bind(&id)
        .bind(p.card_id.as_str())
        .bind(&p.program)
        .bind(&p.cwd)
        .bind(&env_text)
        .bind(&theme_fg)
        .bind(&theme_bg)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(Terminal {
            id,
            card_id: p.card_id,
            program: p.program,
            cwd: p.cwd,
            env: p.env,
            pid: None,
            theme_fg,
            theme_bg,
            exit_code: None,
            signal_killed: false,
            pty_output: String::new(),
            pty_output_truncated: false,
            created_at: now,
        })
    }

    async fn terminal_set_pid(&self, id: &str, pid: Option<u32>) -> Result<()> {
        let pid_i64: Option<i64> = pid.map(|p| p as i64);
        let res = sqlx::query("UPDATE terminals SET pid = ?1 WHERE id = ?2")
            .bind(pid_i64)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("terminal {id}")));
        }
        Ok(())
    }

    async fn terminal_set_exit(
        &self,
        id: &str,
        exit_code: Option<i32>,
        signal_killed: bool,
    ) -> Result<()> {
        let output_unavailable = exit_code.is_some() || signal_killed;
        self.terminal_set_exit_with_output(id, exit_code, signal_killed, "", output_unavailable)
            .await
    }

    async fn terminal_set_exit_with_output(
        &self,
        id: &str,
        exit_code: Option<i32>,
        signal_killed: bool,
        pty_output: &str,
        pty_output_truncated: bool,
    ) -> Result<()> {
        // Single UPDATE so exit and output evidence land together; signal_killed=true
        // ⇒ exit_code=None is the writer's responsibility.
        let res = sqlx::query(
            "UPDATE terminals SET exit_code=?1,signal_killed=?2,pty_output=?3,\
             pty_output_truncated=?4 WHERE id=?5",
        )
        .bind(exit_code)
        .bind(if signal_killed { 1_i64 } else { 0_i64 })
        .bind(pty_output)
        .bind(if pty_output_truncated { 1_i64 } else { 0_i64 })
        .bind(id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("terminal {id}")));
        }
        Ok(())
    }

    async fn terminal_clear_exit_for_spawn(&self, id: &str) -> Result<()> {
        let res = sqlx::query(
            "UPDATE terminals SET pid=NULL,exit_code=NULL,signal_killed=0,pty_output='',\
             pty_output_truncated=0 WHERE id=?1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("terminal {id}")));
        }
        Ok(())
    }

    async fn terminal_delete(&self, id: &str) -> Result<()> {
        let res = sqlx::query("DELETE FROM terminals WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("terminal {id}")));
        }
        Ok(())
    }

    async fn shared_daemon_runtime_set(&self, update: SharedCodexDaemonUpdate) -> Result<()> {
        let now = now_ms();
        let start_time = update
            .process_start_time
            .and_then(|v| i64::try_from(v).ok());
        sqlx::query(
            r#"INSERT INTO shared_codex_daemon
                   (id, state, pid, pgid, sock_path, codex_home_path, process_start_time,
                    boot_id, started_at, updated_at, restart_count, last_error,
                    daemon_env_signature)
               VALUES
                   (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                    CASE WHEN ?10 THEN 1 ELSE 0 END, ?11, ?12)
               ON CONFLICT(id) DO UPDATE SET
                   state = excluded.state,
                   pid = excluded.pid,
                   pgid = excluded.pgid,
                   sock_path = excluded.sock_path,
                   codex_home_path = excluded.codex_home_path,
                   process_start_time = excluded.process_start_time,
                   boot_id = excluded.boot_id,
                   started_at = excluded.started_at,
                   updated_at = excluded.updated_at,
                   restart_count = shared_codex_daemon.restart_count
                       + CASE WHEN ?10 THEN 1 ELSE 0 END,
                   last_error = excluded.last_error,
                   daemon_env_signature = excluded.daemon_env_signature"#,
        )
        .bind(&update.state)
        .bind(update.pid)
        .bind(update.pgid)
        .bind(&update.sock_path)
        .bind(&update.codex_home_path)
        .bind(start_time)
        .bind(&update.boot_id)
        .bind(update.started_at)
        .bind(now)
        .bind(update.increment_restart_count)
        .bind(&update.last_error)
        .bind(&update.daemon_env_signature)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn shared_daemon_record_event(&self, action: &str, error: Option<&str>) -> Result<()> {
        let now = now_ms();
        let last_error = error.map(|e| format!("{action}: {e}"));
        sqlx::query(
            r#"UPDATE shared_codex_daemon
                  SET updated_at = ?1,
                      last_error = COALESCE(?2, last_error)
                WHERE id = 1"#,
        )
        .bind(now)
        .bind(last_error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn harness_item_insert(
        &self,
        worker_session_id: &str,
        card_id: &str,
        track_id: &str,
        thread_id: &str,
        turn_id: Option<&str>,
        item_uuid: Option<&str>,
        item_type: Option<&str>,
        method: &str,
        params: &str,
        input_segments: Option<&str>,
    ) -> Result<i64> {
        let row = sqlx::query(
            r#"INSERT INTO harness_items (
                   worker_session_id, card_id, track_id, thread_id, turn_id,
                   item_uuid, item_type, method, params, input_segments, created_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
               RETURNING id"#,
        )
        .bind(worker_session_id)
        .bind(card_id)
        .bind(track_id)
        .bind(thread_id)
        .bind(turn_id)
        .bind(item_uuid)
        .bind(item_type)
        .bind(method)
        .bind(params)
        .bind(input_segments)
        .bind(now_ms())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>("id"))
    }

    async fn harness_turn_outcome_put(
        &self,
        worker_session_id: &str,
        card_id: &str,
        track_id: &str,
        thread_id: &str,
        turn_id: &str,
        params: &str,
    ) -> Result<i64> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let existing:Option<i64>=sqlx::query_scalar(
            "SELECT id FROM harness_items WHERE worker_session_id=?1 AND card_id=?2 AND thread_id=?3 AND \
                turn_id=?4 AND method='turn/completed' ORDER BY id DESC LIMIT 1"
        ).bind(worker_session_id).bind(card_id).bind(thread_id).bind(turn_id).fetch_optional(&mut *tx).await?;
        let id=match existing {
            Some(id)=>id,
            None=>sqlx::query_scalar(
                "INSERT INTO \
                    harness_items(worker_session_id,card_id,track_id,thread_id,turn_id,method,params,created_at_ms) \
                    VALUES(?1,?2,?3,?4,?5,'turn/completed',?6,?7) RETURNING id"
            ).bind(worker_session_id).bind(card_id).bind(track_id).bind(thread_id).bind(turn_id).bind(params).bind(now_ms()).fetch_one(&mut *tx).await?,
        };
        tx.commit().await?;
        Ok(id)
    }

    async fn transcript_projection_id(
        &self,
        card_id: &str,
        client_id: &str,
    ) -> Result<Option<i64>> {
        let row = sqlx::query(
            r#"SELECT id FROM harness_items
               WHERE card_id = ?1 AND item_uuid = ?2 AND turn_id IS NULL
                 AND method = 'item/completed'
               ORDER BY id DESC LIMIT 1"#,
        )
        .bind(card_id)
        .bind(client_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| row.get::<i64, _>("id")))
    }

    async fn transcript_projection_upgrade(
        &self,
        card_id: &str,
        client_id: &str,
        turn_id: Option<&str>,
        item_uuid: &str,
        params: &str,
    ) -> Result<Option<i64>> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        // `LIMIT 1` is not available on UPDATE in the bundled sqlite build, so the
        // subquery chooses the row: the newest projection with this key.
        let row = sqlx::query(
            r#"UPDATE harness_items
               SET turn_id = ?3, item_uuid = ?4, params = ?5
               WHERE id = (
                   SELECT id FROM harness_items
                   WHERE card_id = ?1 AND item_uuid = ?2 AND turn_id IS NULL
                     AND method = 'item/completed'
                   ORDER BY id DESC LIMIT 1
               )
               RETURNING id"#,
        )
        .bind(card_id)
        .bind(client_id)
        .bind(turn_id)
        .bind(item_uuid)
        .bind(params)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row.map(|row| row.get::<i64, _>("id")))
    }

    async fn transcript_projection_delete(&self, card_id: &str, client_id: &str) -> Result<u64> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let done = sqlx::query(
            r#"DELETE FROM harness_items
               WHERE card_id = ?1 AND item_uuid = ?2 AND turn_id IS NULL
                 AND method = 'item/completed'"#,
        )
        .bind(card_id)
        .bind(client_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(done.rows_affected())
    }

    #[allow(clippy::too_many_arguments)]
    async fn worker_flow_item_insert(
        &self,
        card_id: Option<&str>,
        captured_session_id: Option<&str>,
        track_id: Option<&str>,
        worker_session_id: Option<&str>,
        kind: &str,
        payload: &str,
        created_at_ms: i64,
    ) -> Result<i64> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let id = worker_flow_item_insert_tx(
            &mut tx,
            card_id,
            captured_session_id,
            track_id,
            worker_session_id,
            kind,
            payload,
            created_at_ms,
        )
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    async fn worker_flow_cursor_upsert(
        &self,
        card_id: &str,
        source_kind: &str,
        source_path: &str,
        record_index: i64,
        byte_offset: i64,
        last_source_uuid: Option<&str>,
        last_line_hash: Option<&str>,
        updated_at_ms: i64,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO worker_flow_cursors (
                   card_id, source_kind, source_path, record_index,
                   byte_offset, last_source_uuid, last_line_hash, updated_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
               ON CONFLICT(card_id, source_kind) DO UPDATE SET
                   source_path = excluded.source_path,
                   record_index = excluded.record_index,
                   byte_offset = excluded.byte_offset,
                   last_source_uuid = excluded.last_source_uuid,
                   last_line_hash = excluded.last_line_hash,
                   updated_at_ms = excluded.updated_at_ms"#,
        )
        .bind(card_id)
        .bind(source_kind)
        .bind(source_path)
        .bind(record_index)
        .bind(byte_offset)
        .bind(last_source_uuid)
        .bind(last_line_hash)
        .bind(updated_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The `DO UPDATE` set deliberately omits `user_config`: only
    /// `PATCH /api/plugins/{id}/config` may change an installed plugin's config.
    async fn plugin_install(&self, p: NewPlugin) -> Result<Plugin> {
        let manifest_text = serde_json::to_string(&p.manifest)?;
        let user_config_text = serde_json::to_string(&p.user_config)?;
        let now = now_ms();
        let row = sqlx::query_as::<_, Plugin>(
            r#"INSERT INTO plugins
                   (id, version, install_path, manifest, enabled, user_config,
                    installed_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
               ON CONFLICT(id) DO UPDATE SET
                   version      = excluded.version,
                   install_path = excluded.install_path,
                   manifest     = excluded.manifest,
                   enabled      = excluded.enabled,
                   -- user_config is intentionally NOT in this set; see the
                   -- doc comment on this method (#1284 S1 P2-3).
                   updated_at   = excluded.updated_at
               RETURNING id, version, install_path, manifest, enabled, user_config,
                         installed_at, updated_at"#,
        )
        .bind(&p.id)
        .bind(&p.version)
        .bind(&p.install_path)
        .bind(&manifest_text)
        .bind(p.enabled)
        .bind(&user_config_text)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    async fn plugin_update_enabled(&self, id: &str, enabled: bool) -> Result<Plugin> {
        let now = now_ms();
        let res = sqlx::query(r#"UPDATE plugins SET enabled = ?1, updated_at = ?2 WHERE id = ?3"#)
            .bind(enabled)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("plugin {id}")));
        }
        self.plugin_get_by_id(id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))
    }

    async fn plugin_update_user_config(
        &self,
        id: &str,
        user_config: serde_json::Value,
    ) -> Result<Plugin> {
        let now = now_ms();
        let user_config_text = serde_json::to_string(&user_config)?;
        let res =
            sqlx::query(r#"UPDATE plugins SET user_config = ?1, updated_at = ?2 WHERE id = ?3"#)
                .bind(&user_config_text)
                .bind(now)
                .bind(id)
                .execute(&self.pool)
                .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("plugin {id}")));
        }
        self.plugin_get_by_id(id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))
    }

    async fn plugin_update_manifest(
        &self,
        id: &str,
        manifest: serde_json::Value,
    ) -> Result<Plugin> {
        let now = now_ms();
        let manifest_text = serde_json::to_string(&manifest)?;
        let res = sqlx::query(r#"UPDATE plugins SET manifest = ?1, updated_at = ?2 WHERE id = ?3"#)
            .bind(&manifest_text)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("plugin {id}")));
        }
        self.plugin_get_by_id(id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))
    }

    async fn plugin_delete(&self, id: &str) -> Result<()> {
        let res = sqlx::query("DELETE FROM plugins WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("plugin {id}")));
        }
        Ok(())
    }

    async fn overlays_clear_by_plugin(&self, plugin_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM overlays WHERE plugin_id = ?1")
            .bind(plugin_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn plugin_kv_clear(&self, plugin_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM plugin_kv WHERE plugin_id = ?1")
            .bind(plugin_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn plugin_token_set(
        &self,
        plugin_id: &str,
        hashed_token: &str,
        expires_at: i64,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO plugin_tokens (plugin_id, hashed_token, expires_at)
               VALUES (?1, ?2, ?3)
               ON CONFLICT(plugin_id) DO UPDATE SET
                   hashed_token = excluded.hashed_token,
                   expires_at   = excluded.expires_at"#,
        )
        .bind(plugin_id)
        .bind(hashed_token)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn plugin_token_delete(&self, plugin_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM plugin_tokens WHERE plugin_id = ?1")
            .bind(plugin_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn plugin_kv_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        let text = serde_json::to_string(value)?;
        let now = now_ms();
        sqlx::query(
            r#"INSERT INTO plugin_kv (plugin_id, key, value, updated_at)
               VALUES (?1, ?2, ?3, ?4)
               ON CONFLICT(plugin_id, key) DO UPDATE SET
                   value      = excluded.value,
                   updated_at = excluded.updated_at"#,
        )
        .bind(plugin_id)
        .bind(key)
        .bind(&text)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn plugin_kv_delete(&self, plugin_id: &str, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2")
            .bind(plugin_id)
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn settings_upsert(&self, key: &str, value: &str) -> Result<()> {
        let now = now_ms();
        sqlx::query(
            r#"INSERT INTO settings (key, value, updated_at)
               VALUES (?1, ?2, ?3)
               ON CONFLICT(key) DO UPDATE SET
                   value      = excluded.value,
                   updated_at = excluded.updated_at"#,
        )
        .bind(key)
        .bind(value)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn settings_delete(&self, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM settings WHERE key = ?1")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn area_folder_create(&self, area_id: &str, path: &str) -> Result<AreaFolder> {
        let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM areas WHERE id = ?1")
            .bind(area_id)
            .fetch_optional(&self.pool)
            .await?;
        if exists.is_none() {
            return Err(CalmError::NotFound(format!("area {area_id}")));
        }
        let now = now_ms();
        // Unchecked primitive: no overlap scan; UNIQUE(path) only rejects an *equal*
        // path. HTTP callers go through `area_folder_create_checked`.
        let res =
            sqlx::query("INSERT INTO area_folders (area_id, path, created_at) VALUES (?1, ?2, ?3)")
                .bind(area_id)
                .bind(path)
                .bind(now)
                .execute(&self.pool)
                .await;
        match res {
            Ok(out) => Ok(AreaFolder {
                id: out.last_insert_rowid(),
                area_id: area_id.to_string().into(),
                path: path.to_string(),
                created_at: now,
            }),
            Err(sqlx::Error::Database(dbe)) if dbe.message().contains("UNIQUE") => Err(
                CalmError::Conflict(format!("area_folders.path already claims `{path}`")),
            ),
            Err(e) => Err(e.into()),
        }
    }

    async fn area_folder_create_checked(
        &self,
        area_id: &str,
        path: &str,
    ) -> Result<AreaFolderClaim> {
        // Precondition: `path` is already normalized; `classify_conflict` is pure
        // string comparison, so a trailing slash would silently misclassify.
        debug_assert_eq!(
            path,
            crate::area_folder_claim::normalize_path(path),
            "area_folder_create_checked requires a normalized path; got `{path}`"
        );
        // BEGIN IMMEDIATE takes the writer lock before the scan so SELECT and INSERT
        // are one atomic step (UNIQUE(path) only rejects *equal* paths). Nothing but
        // these three statements belongs inside the lock.
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let existing = super::area_folders_list_all_tx(&mut tx).await?;
        if let Some(conflict) = crate::area_folder_claim::classify_conflict(&existing, path) {
            let _ = tx.rollback().await;
            return Ok(AreaFolderClaim::Conflict(conflict));
        }
        let folder = super::area_folder_create_tx(&mut tx, area_id, path).await?;
        tx.commit().await?;
        Ok(AreaFolderClaim::Created(folder))
    }

    async fn area_folder_delete(&self, id: i64) -> Result<()> {
        let res = sqlx::query("DELETE FROM area_folders WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("area_folder {id}")));
        }
        Ok(())
    }
}
