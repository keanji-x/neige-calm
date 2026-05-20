//! SQLite-backed `Repo` implementation. **Owned by Track A.**
//!
//! Implements every method on the `Repo` trait against a `sqlx::SqlitePool`.
//! The pool is opened with `PRAGMA foreign_keys = ON` per-connection, the
//! bundled migrations under `migrations/` are run on `open()`, and every
//! observable behavior of `MockRepo` (cascades, sort defaulting, not-found
//! semantics, overlay upsert by unique key) is replicated here.

use std::str::FromStr;

use async_trait::async_trait;
use sqlx::ConnectOptions;
use sqlx::Executor;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::Repo;
use crate::error::{CalmError, Result};
use crate::model::*;

pub struct SqlxRepo {
    pool: SqlitePool,
}

impl SqlxRepo {
    /// Open / create the SQLite DB at `url`, run pending migrations, and
    /// enable foreign-key enforcement per-connection.
    ///
    /// Accepts both `sqlite::memory:` (used in tests) and on-disk
    /// `sqlite://path?mode=rwc` URLs.
    pub async fn open(url: &str) -> Result<Self> {
        let mut opts = SqliteConnectOptions::from_str(url)
            .map_err(|e| CalmError::Internal(format!("invalid sqlite url {url:?}: {e}")))?
            .create_if_missing(true)
            .foreign_keys(true);
        // Reduce noise from sqlx's per-statement logging at info; keep debug.
        opts = opts.log_statements(tracing::log::LevelFilter::Debug);

        let pool = SqlitePoolOptions::new()
            // Belt-and-braces: also re-issue the pragma on every fresh
            // connection in case `foreign_keys(true)` is silently dropped
            // for some URL forms (e.g. memory).
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    conn.execute("PRAGMA foreign_keys = ON;").await?;
                    Ok(())
                })
            })
            .connect_with(opts)
            .await?;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| CalmError::Internal(format!("migrate: {e}")))?;

        Ok(Self { pool })
    }
}

// ---- helpers -----------------------------------------------------------------

/// Compute the next sort value (max + 1) within a scoped table.
///
/// `scope_sql` is appended verbatim after `FROM <table>`; supply `""` for
/// global scope, or `"WHERE cove_id = ?1"` etc. Bind a single optional
/// scope parameter via `scope_id`.
async fn next_sort_scoped(
    pool: &SqlitePool,
    table: &str,
    scope_sql: &str,
    scope_id: Option<&str>,
) -> Result<f64> {
    let sql = format!("SELECT COALESCE(MAX(sort), 0.0) + 1.0 AS s FROM {table} {scope_sql}");
    let mut q = sqlx::query(&sql);
    if let Some(id) = scope_id {
        q = q.bind(id);
    }
    let row = q.fetch_one(pool).await?;
    Ok(row.try_get::<f64, _>("s")?)
}

#[async_trait]
impl Repo for SqlxRepo {
    // ---------------------------------------------------------------- coves
    async fn coves_list(&self) -> Result<Vec<Cove>> {
        let rows = sqlx::query_as::<_, Cove>(
            r#"SELECT id, name, color, sort, created_at, updated_at
               FROM coves ORDER BY sort ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn cove_get(&self, id: &str) -> Result<Option<Cove>> {
        let row = sqlx::query_as::<_, Cove>(
            r#"SELECT id, name, color, sort, created_at, updated_at
               FROM coves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn cove_create(&self, p: NewCove) -> Result<Cove> {
        let sort = match p.sort {
            Some(s) => s,
            None => next_sort_scoped(&self.pool, "coves", "", None).await?,
        };
        let now = now_ms();
        let id = new_id();
        sqlx::query(
            r#"INSERT INTO coves (id, name, color, sort, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6)"#,
        )
        .bind(&id)
        .bind(&p.name)
        .bind(&p.color)
        .bind(sort)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(Cove {
            id,
            name: p.name,
            color: p.color,
            sort,
            created_at: now,
            updated_at: now,
        })
    }

    async fn cove_update(&self, id: &str, p: CovePatch) -> Result<Cove> {
        let mut tx = self.pool.begin().await?;
        let mut c = sqlx::query_as::<_, Cove>(
            r#"SELECT id, name, color, sort, created_at, updated_at
               FROM coves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("cove {id}")))?;

        if let Some(v) = p.name {
            c.name = v;
        }
        if let Some(v) = p.color {
            c.color = v;
        }
        if let Some(v) = p.sort {
            c.sort = v;
        }
        c.updated_at = now_ms();

        sqlx::query(
            r#"UPDATE coves SET name = ?1, color = ?2, sort = ?3, updated_at = ?4
               WHERE id = ?5"#,
        )
        .bind(&c.name)
        .bind(&c.color)
        .bind(c.sort)
        .bind(c.updated_at)
        .bind(&c.id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(c)
    }

    async fn cove_delete(&self, id: &str) -> Result<()> {
        // ON DELETE CASCADE handles waves/cards once foreign_keys=ON.
        let res = sqlx::query("DELETE FROM coves WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("cove {id}")));
        }
        Ok(())
    }

    // ---------------------------------------------------------------- waves
    async fn waves_by_cove(&self, cove_id: &str) -> Result<Vec<Wave>> {
        let rows = sqlx::query_as::<_, Wave>(
            r#"SELECT id, cove_id, title, sort, archived_at, created_at, updated_at
               FROM waves WHERE cove_id = ?1 ORDER BY sort ASC"#,
        )
        .bind(cove_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn wave_get(&self, id: &str) -> Result<Option<Wave>> {
        let row = sqlx::query_as::<_, Wave>(
            r#"SELECT id, cove_id, title, sort, archived_at, created_at, updated_at
               FROM waves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn wave_detail(&self, id: &str) -> Result<Option<WaveDetail>> {
        let mut tx = self.pool.begin().await?;
        let wave = sqlx::query_as::<_, Wave>(
            r#"SELECT id, cove_id, title, sort, archived_at, created_at, updated_at
               FROM waves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(wave) = wave else {
            return Ok(None);
        };

        let cards = sqlx::query_as::<_, Card>(
            r#"SELECT id, wave_id, kind, sort, payload, created_at, updated_at
               FROM cards WHERE wave_id = ?1 ORDER BY sort ASC"#,
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;

        // Overlays scoped to this wave or any of its cards. One query: a
        // wave-scoped row plus an IN-list on card ids built at the SQL level
        // using a `cards` subquery so we avoid a parameter explosion.
        let overlays = sqlx::query_as::<_, Overlay>(
            r#"SELECT id, plugin_id, entity_kind, entity_id, kind, payload, updated_at
               FROM overlays
               WHERE (entity_kind = 'wave' AND entity_id = ?1)
                  OR (entity_kind = 'card'
                      AND entity_id IN (SELECT id FROM cards WHERE wave_id = ?1))"#,
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(Some(WaveDetail {
            wave,
            cards,
            overlays,
        }))
    }

    async fn wave_create(&self, p: NewWave) -> Result<Wave> {
        // Validate parent cove exists so we can return NotFound (matching
        // MockRepo) instead of letting a foreign-key failure bubble as Db.
        let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM coves WHERE id = ?1")
            .bind(&p.cove_id)
            .fetch_optional(&self.pool)
            .await?;
        if exists.is_none() {
            return Err(CalmError::NotFound(format!("cove {}", p.cove_id)));
        }

        let sort = match p.sort {
            Some(s) => s,
            None => {
                next_sort_scoped(&self.pool, "waves", "WHERE cove_id = ?1", Some(&p.cove_id))
                    .await?
            }
        };
        let now = now_ms();
        let id = new_id();
        sqlx::query(
            r#"INSERT INTO waves
               (id, cove_id, title, sort, archived_at, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)"#,
        )
        .bind(&id)
        .bind(&p.cove_id)
        .bind(&p.title)
        .bind(sort)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(Wave {
            id,
            cove_id: p.cove_id,
            title: p.title,
            sort,
            archived_at: None,
            created_at: now,
            updated_at: now,
        })
    }

    async fn wave_update(&self, id: &str, p: WavePatch) -> Result<Wave> {
        let mut tx = self.pool.begin().await?;
        let mut w = sqlx::query_as::<_, Wave>(
            r#"SELECT id, cove_id, title, sort, archived_at, created_at, updated_at
               FROM waves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
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
        w.updated_at = now_ms();

        sqlx::query(
            r#"UPDATE waves SET title = ?1, sort = ?2, archived_at = ?3, updated_at = ?4
               WHERE id = ?5"#,
        )
        .bind(&w.title)
        .bind(w.sort)
        .bind(w.archived_at)
        .bind(w.updated_at)
        .bind(&w.id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(w)
    }

    async fn wave_delete(&self, id: &str) -> Result<()> {
        let res = sqlx::query("DELETE FROM waves WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("wave {id}")));
        }
        Ok(())
    }

    // ---------------------------------------------------------------- cards
    async fn cards_by_wave(&self, wave_id: &str) -> Result<Vec<Card>> {
        let rows = sqlx::query_as::<_, Card>(
            r#"SELECT id, wave_id, kind, sort, payload, created_at, updated_at
               FROM cards WHERE wave_id = ?1 ORDER BY sort ASC"#,
        )
        .bind(wave_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn card_get(&self, id: &str) -> Result<Option<Card>> {
        let row = sqlx::query_as::<_, Card>(
            r#"SELECT id, wave_id, kind, sort, payload, created_at, updated_at
               FROM cards WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn card_create(&self, p: NewCard) -> Result<Card> {
        let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM waves WHERE id = ?1")
            .bind(&p.wave_id)
            .fetch_optional(&self.pool)
            .await?;
        if exists.is_none() {
            return Err(CalmError::NotFound(format!("wave {}", p.wave_id)));
        }

        let sort = match p.sort {
            Some(s) => s,
            None => {
                next_sort_scoped(&self.pool, "cards", "WHERE wave_id = ?1", Some(&p.wave_id))
                    .await?
            }
        };
        let now = now_ms();
        let id = new_id();
        let payload_text = serde_json::to_string(&p.payload)?;
        sqlx::query(
            r#"INSERT INTO cards (id, wave_id, kind, sort, payload, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
        )
        .bind(&id)
        .bind(&p.wave_id)
        .bind(&p.kind)
        .bind(sort)
        .bind(&payload_text)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(Card {
            id,
            wave_id: p.wave_id,
            kind: p.kind,
            sort,
            payload: p.payload,
            created_at: now,
            updated_at: now,
        })
    }

    async fn card_update(&self, id: &str, p: CardPatch) -> Result<Card> {
        let mut tx = self.pool.begin().await?;
        let mut c = sqlx::query_as::<_, Card>(
            r#"SELECT id, wave_id, kind, sort, payload, created_at, updated_at
               FROM cards WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;

        if let Some(v) = p.kind {
            c.kind = v;
        }
        if let Some(v) = p.sort {
            c.sort = v;
        }
        if let Some(v) = p.payload {
            c.payload = v;
        }
        c.updated_at = now_ms();
        let payload_text = serde_json::to_string(&c.payload)?;

        sqlx::query(
            r#"UPDATE cards SET kind = ?1, sort = ?2, payload = ?3, updated_at = ?4
               WHERE id = ?5"#,
        )
        .bind(&c.kind)
        .bind(c.sort)
        .bind(&payload_text)
        .bind(c.updated_at)
        .bind(&c.id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(c)
    }

    async fn card_delete(&self, id: &str) -> Result<()> {
        let res = sqlx::query("DELETE FROM cards WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("card {id}")));
        }
        Ok(())
    }

    // -------------------------------------------------------------- overlays
    async fn overlay_upsert(&self, p: NewOverlay) -> Result<Overlay> {
        let now = now_ms();
        let new_id_str = new_id();
        let payload_text = serde_json::to_string(&p.payload)?;
        // Use INSERT ... ON CONFLICT DO UPDATE and RETURNING the row, so the
        // unique-key composite drives idempotent upsert and we don't need a
        // separate read.
        let row = sqlx::query_as::<_, Overlay>(
            r#"INSERT INTO overlays
                   (id, plugin_id, entity_kind, entity_id, kind, payload, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
               ON CONFLICT(plugin_id, entity_kind, entity_id, kind)
                 DO UPDATE SET payload = excluded.payload,
                               updated_at = excluded.updated_at
               RETURNING id, plugin_id, entity_kind, entity_id, kind, payload, updated_at"#,
        )
        .bind(&new_id_str)
        .bind(&p.plugin_id)
        .bind(&p.entity_kind)
        .bind(&p.entity_id)
        .bind(&p.kind)
        .bind(&payload_text)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    async fn overlay_delete(
        &self,
        plugin_id: &str,
        entity_kind: &str,
        entity_id: &str,
        kind: &str,
    ) -> Result<()> {
        let res = sqlx::query(
            r#"DELETE FROM overlays
               WHERE plugin_id = ?1 AND entity_kind = ?2 AND entity_id = ?3 AND kind = ?4"#,
        )
        .bind(plugin_id)
        .bind(entity_kind)
        .bind(entity_id)
        .bind(kind)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound("overlay".into()));
        }
        Ok(())
    }

    async fn overlays_for(&self, entity_kind: &str, entity_id: &str) -> Result<Vec<Overlay>> {
        let rows = sqlx::query_as::<_, Overlay>(
            r#"SELECT id, plugin_id, entity_kind, entity_id, kind, payload, updated_at
               FROM overlays WHERE entity_kind = ?1 AND entity_id = ?2"#,
        )
        .bind(entity_kind)
        .bind(entity_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn overlays_by_kind(&self, entity_kind: &str) -> Result<Vec<Overlay>> {
        let rows = sqlx::query_as::<_, Overlay>(
            r#"SELECT id, plugin_id, entity_kind, entity_id, kind, payload, updated_at
               FROM overlays WHERE entity_kind = ?1"#,
        )
        .bind(entity_kind)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    // ------------------------------------------------------------- terminals
    async fn terminal_create(&self, p: NewTerminal) -> Result<Terminal> {
        // Parent card must exist; surface as NotFound to mirror MockRepo.
        let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM cards WHERE id = ?1")
            .bind(&p.card_id)
            .fetch_optional(&self.pool)
            .await?;
        if exists.is_none() {
            return Err(CalmError::NotFound(format!("card {}", p.card_id)));
        }
        // Per-card uniqueness — surface as Conflict to mirror MockRepo
        // (the schema also enforces this via UNIQUE on terminals.card_id).
        let dup: Option<(String,)> = sqlx::query_as("SELECT id FROM terminals WHERE card_id = ?1")
            .bind(&p.card_id)
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
        sqlx::query(
            r#"INSERT INTO terminals
                   (id, card_id, program, cwd, env, daemon_handle, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)"#,
        )
        .bind(&id)
        .bind(&p.card_id)
        .bind(&p.program)
        .bind(&p.cwd)
        .bind(&env_text)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(Terminal {
            id,
            card_id: p.card_id,
            program: p.program,
            cwd: p.cwd,
            env: p.env,
            daemon_handle: None,
            created_at: now,
        })
    }

    async fn terminal_get(&self, id: &str) -> Result<Option<Terminal>> {
        let row = sqlx::query_as::<_, Terminal>(
            r#"SELECT id, card_id, program, cwd, env, daemon_handle, created_at
               FROM terminals WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn terminal_get_by_card(&self, card_id: &str) -> Result<Option<Terminal>> {
        let row = sqlx::query_as::<_, Terminal>(
            r#"SELECT id, card_id, program, cwd, env, daemon_handle, created_at
               FROM terminals WHERE card_id = ?1"#,
        )
        .bind(card_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn terminal_set_handle(&self, id: &str, handle: Option<&str>) -> Result<()> {
        let res = sqlx::query("UPDATE terminals SET daemon_handle = ?1 WHERE id = ?2")
            .bind(handle)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("terminal {id}")));
        }
        Ok(())
    }

    // --------------------------------------------------------------- plugins
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

    async fn plugin_install(&self, p: NewPlugin) -> Result<Plugin> {
        // Manifest and user_config are stored as TEXT (JSON-encoded). We
        // serialize ourselves so the `INSERT ... ON CONFLICT DO UPDATE` clause
        // can bind the strings once and reuse them through `excluded.*`.
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
        // Re-read so callers get the canonical row (including `installed_at`).
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
        // ON DELETE CASCADE on plugin_tokens + plugin_kv handles the satellites
        // (see migrations/0002_plugins.sql).
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
        // No FK between overlays.plugin_id and plugins.id (plugins are
        // discovered + ephemeral; overlays survive plugin removal by design),
        // so we drop overlays explicitly here from Slice D's uninstall path.
        sqlx::query("DELETE FROM overlays WHERE plugin_id = ?1")
            .bind(plugin_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn plugin_kv_clear(&self, plugin_id: &str) -> Result<()> {
        // Idempotent — uninstall calls this unconditionally. The plugins-level
        // delete also cascades via FK, so this is the explicit form for cases
        // where the row was already removed (or never existed).
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

    async fn plugin_token_get(&self, plugin_id: &str) -> Result<Option<(String, i64)>> {
        let row: Option<(String, i64)> = sqlx::query_as(
            r#"SELECT hashed_token, expires_at FROM plugin_tokens WHERE plugin_id = ?1"#,
        )
        .bind(plugin_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn plugin_token_delete(&self, plugin_id: &str) -> Result<()> {
        // Idempotent per the trait contract.
        sqlx::query("DELETE FROM plugin_tokens WHERE plugin_id = ?1")
            .bind(plugin_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -------------------------------------------------------- plugin kv
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

    async fn plugin_kv_list(
        &self,
        plugin_id: &str,
        prefix: &str,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        // `LIKE ? ESCAPE '\'` with the user-supplied prefix concatenated with
        // `%` would let the prefix itself contain wildcards. Slice C will hand
        // us trusted prefixes from plugin code, but Slice A's contract is "any
        // string is a literal prefix" — so glob-escape `%` and `_` ourselves.
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

    async fn plugin_kv_delete(&self, plugin_id: &str, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2")
            .bind(plugin_id)
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -------------------------------------------------------------- settings
    async fn settings_get_all(&self) -> Result<Vec<(String, String)>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as(r#"SELECT key, value FROM settings ORDER BY key ASC"#)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
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
        // Idempotent — empty-string upserts coming through `PUT /api/settings`
        // get rewritten to deletes, and missing rows are not an error there.
        sqlx::query("DELETE FROM settings WHERE key = ?1")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
