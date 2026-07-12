use async_trait::async_trait;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::Transaction;

use super::{SqlxRepo, begin_immediate_tx};
use crate::db::{RepoOutOfDomain, RepoRead, SharedCodexDaemonUpdate};
use crate::error::{CalmError, Result};
use crate::model::*;

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

/// #695 PR2 — append one `worker_flow_items` row inside an open transaction,
/// returning the new row id. Free fn (mirroring the harness `_tx` helpers) so
/// PR3's `WorkerFlowItemSink` can call it from inside `commit_decision`'s
/// closure. The `RepoOutOfDomain::worker_flow_item_insert` trait method wraps
/// this in its own short transaction for standalone callers.
#[allow(clippy::too_many_arguments)]
pub async fn worker_flow_item_insert_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: Option<&str>,
    runtime_id: Option<&str>,
    wave_id: Option<&str>,
    worker_session_id: Option<&str>,
    kind: &str,
    payload: &str,
    created_at_ms: i64,
) -> Result<i64> {
    let row = sqlx::query(
        r#"INSERT INTO worker_flow_items (
               card_id, runtime_id, wave_id, worker_session_id,
               kind, payload, created_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
           RETURNING id"#,
    )
    .bind(card_id)
    .bind(runtime_id)
    .bind(wave_id)
    .bind(worker_session_id)
    .bind(kind)
    .bind(payload)
    .bind(created_at_ms)
    .fetch_one(&mut **tx)
    .await?;
    Ok(row.get::<i64, _>("id"))
}

/// #695 PR2 — hard-delete every `worker_flow_items` row for a card. Mirror of
/// [`harness_items_delete_by_card_tx`]. Unlike the FK's `ON DELETE SET NULL`
/// (which preserves the transcript when the *card* is deleted), this is the
/// explicit "purge this card's captured flow" path a caller can invoke
/// directly inside a transaction.
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

// ---------------------------------------------------------------------------
// RepoOutOfDomain — operational writes that intentionally bypass the event
// log: terminal lifecycle, plugin install/config, app-global settings. See
// db/mod.rs module doc for the sync-domain vs. out-of-domain split.
// ---------------------------------------------------------------------------

#[async_trait]
impl RepoOutOfDomain for SqlxRepo {
    // ------------------------------------------------------------- terminals
    async fn terminal_create(&self, p: NewTerminal) -> Result<Terminal> {
        // Parent card must exist; surface as NotFound to mirror MockRepo.
        let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM cards WHERE id = ?1")
            .bind(p.card_id.as_str())
            .fetch_optional(&self.pool)
            .await?;
        if exists.is_none() {
            return Err(CalmError::NotFound(format!("card {}", p.card_id)));
        }
        // Per-card uniqueness — surface as Conflict to mirror MockRepo
        // (the schema also enforces this via UNIQUE on terminals.card_id).
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
        // #177 — render theme RGB once at row-creation; persisted in
        // comma-decimal form so every spawn-path read is a zero-alloc
        // string slice.
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
            created_at: now,
        })
    }

    async fn terminal_set_pid(&self, id: &str, pid: Option<u32>) -> Result<()> {
        // Cast to i64 for sqlite's INTEGER affinity; u32 is well within range.
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
        // #306 — single UPDATE; the two columns are written together so
        // a reader never sees a mismatched intermediate state. The
        // mutual-exclusion invariant (signal_killed=true ⇒ exit_code=None)
        // is the writer's responsibility — see daemon `spawn_child_waiter`.
        let res =
            sqlx::query("UPDATE terminals SET exit_code = ?1, signal_killed = ?2 WHERE id = ?3")
                .bind(exit_code)
                .bind(if signal_killed { 1_i64 } else { 0_i64 })
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
            "UPDATE terminals SET pid = NULL, exit_code = NULL, signal_killed = 0 WHERE id = ?1",
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

    // ---- spec harness item stream (#510 PR-ui C1) -----------------------

    #[allow(clippy::too_many_arguments)]
    async fn harness_item_insert(
        &self,
        runtime_id: &str,
        card_id: &str,
        wave_id: &str,
        thread_id: &str,
        turn_id: Option<&str>,
        item_uuid: Option<&str>,
        item_type: Option<&str>,
        method: &str,
        params: &str,
    ) -> Result<i64> {
        let row = sqlx::query(
            r#"INSERT INTO harness_items (
                   runtime_id, card_id, wave_id, thread_id, turn_id,
                   item_uuid, item_type, method, params, created_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
               RETURNING id"#,
        )
        .bind(runtime_id)
        .bind(card_id)
        .bind(wave_id)
        .bind(thread_id)
        .bind(turn_id)
        .bind(item_uuid)
        .bind(item_type)
        .bind(method)
        .bind(params)
        .bind(now_ms())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>("id"))
    }

    // ---- worker message-flow capture (#695 PR2) -------------------------

    #[allow(clippy::too_many_arguments)]
    async fn worker_flow_item_insert(
        &self,
        card_id: Option<&str>,
        runtime_id: Option<&str>,
        wave_id: Option<&str>,
        worker_session_id: Option<&str>,
        kind: &str,
        payload: &str,
        created_at_ms: i64,
    ) -> Result<i64> {
        // #930 uniform rule: writing transactions always BEGIN IMMEDIATE.
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let id = worker_flow_item_insert_tx(
            &mut tx,
            card_id,
            runtime_id,
            wave_id,
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

    // --------------------------------------------------------------- plugins
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
                   user_config  = excluded.user_config,
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

    // -------------------------------------------------------- plugin tokens
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

    // -------------------------------------------------------- plugin kv
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

    // -------------------------------------------------------------- settings
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

    // ----------------------------------------------------- cove_folders
    async fn cove_folder_create(&self, cove_id: &str, path: &str) -> Result<CoveFolder> {
        // Probe before the INSERT acquires SQLite's writer lock.
        let repo_identity = crate::repo_identity::probe_repo_identity(std::path::Path::new(path));
        let probed_at = now_ms();
        // Parent cove must exist; surface as NotFound to mirror the
        // terminal_create precedent above (FK error message would be
        // less actionable for the REST caller).
        let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM coves WHERE id = ?1")
            .bind(cove_id)
            .fetch_optional(&self.pool)
            .await?;
        if exists.is_none() {
            return Err(CalmError::NotFound(format!("cove {cove_id}")));
        }
        let now = now_ms();
        // The UNIQUE constraint on `path` is the backstop here. The
        // route layer has already done equality / ancestor / descendant
        // conflict detection so a real-world INSERT failing the
        // UNIQUE is a race (concurrent claim of the same path). Bubble
        // it up as the generic Conflict so the surface is honest.
        let res =
            sqlx::query("INSERT INTO cove_folders (cove_id, path, repo_identity, repo_identity_probed_at, created_at) VALUES (?1, ?2, ?3, ?4, ?5)")
                .bind(cove_id)
                .bind(path)
                .bind(repo_identity.as_deref())
                .bind(probed_at)
                .bind(now)
                .execute(&self.pool)
                .await;
        match res {
            Ok(out) => Ok(CoveFolder {
                id: out.last_insert_rowid(),
                cove_id: cove_id.to_string().into(),
                path: path.to_string(),
                repo_identity,
                repo_identity_probed_at: Some(probed_at),
                created_at: now,
            }),
            Err(sqlx::Error::Database(dbe)) if dbe.message().contains("UNIQUE") => Err(
                CalmError::Conflict(format!("cove_folders.path already claims `{path}`")),
            ),
            Err(e) => Err(e.into()),
        }
    }

    async fn cove_folder_refresh_repo_identity(&self, id: i64) -> Result<CoveFolder> {
        let folder = self
            .cove_folder_get(id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("cove_folder {id}")))?;
        // As above, all filesystem/git work completes before the write.
        let identity =
            crate::repo_identity::probe_repo_identity(std::path::Path::new(&folder.path));
        let probed_at = now_ms();
        sqlx::query(
            "UPDATE cove_folders SET repo_identity = ?1, repo_identity_probed_at = ?2 WHERE id = ?3",
        )
        .bind(identity.as_deref())
        .bind(probed_at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(CoveFolder {
            repo_identity: identity,
            repo_identity_probed_at: Some(probed_at),
            ..folder
        })
    }

    async fn cove_folder_delete(&self, id: i64) -> Result<()> {
        let res = sqlx::query("DELETE FROM cove_folders WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("cove_folder {id}")));
        }
        Ok(())
    }
}
