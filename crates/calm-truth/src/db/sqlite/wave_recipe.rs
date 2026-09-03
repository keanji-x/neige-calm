//! `wave_recipes` storage (#1292).
//!
//! A recipe is a saved report a user can instantiate into new waves. The
//! table is deliberately plain: an id, the title, the body, and a
//! `revision` that is the optimistic-lock anchor.
//!
//! # `revision`, not `updated_at`
//!
//! Every write validates and bumps `revision` **in the same statement**
//! (`UPDATE ... WHERE id = ?1 AND revision = ?2`), so a stale writer's
//! `UPDATE` matches zero rows and the caller learns it lost. Doing this with
//! `updated_at` would be wrong twice over: two writes inside one millisecond
//! are indistinguishable, and a clock that steps backwards makes a stale
//! write look current.
//!
//! # Normalization is not here
//!
//! The privilege-field normalization a recipe body needs lives at the write
//! boundary in `calm-server` (`routes::wave_recipes`), next to the body
//! parsing it depends on. This module stores what it is handed.
//!
//! # Only the writers are here
//!
//! `get` and `list` are plain SELECTs on the pool in
//! `out_of_domain.rs`, with no transaction: #930 requires a writing
//! transaction to be `BEGIN IMMEDIATE`, and wrapping a single read in one
//! would take the write lock to read. The one `_tx` reader that remains
//! ([`wave_recipe_get_tx`]) exists because [`wave_recipe_update_tx`] needs
//! to read *inside* its own transaction — to tell a stale revision from a
//! missing row, and to read the row back.

use sqlx::Sqlite;
use sqlx::Transaction;

use crate::error::{CalmError, Result};
use crate::model::*;

/// Insert a new recipe at `revision = 1`.
pub async fn wave_recipe_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    title: &str,
    body: &str,
) -> Result<WaveRecipe> {
    let now = now_ms();
    let id = new_id();
    sqlx::query(
        r#"INSERT INTO wave_recipes (id, title, body, revision, created_at, updated_at)
           VALUES (?1, ?2, ?3, 1, ?4, ?4)"#,
    )
    .bind(&id)
    .bind(title)
    .bind(body)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(WaveRecipe {
        id,
        title: title.to_string(),
        body: body.to_string(),
        revision: 1,
        created_at: now,
        updated_at: now,
    })
}

/// Replace a recipe's content iff `if_revision` still matches.
///
/// Returns `Conflict` when the row exists at a different revision and
/// `NotFound` when it does not exist at all. The two are distinguished by a
/// follow-up read rather than by the `UPDATE`'s row count alone, because
/// "zero rows changed" cannot tell them apart and the caller must answer
/// 404 and 409 differently.
pub async fn wave_recipe_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    title: &str,
    body: &str,
    if_revision: i64,
) -> Result<WaveRecipe> {
    let now = now_ms();
    let changed = sqlx::query(
        r#"UPDATE wave_recipes
              SET title = ?2, body = ?3, revision = revision + 1, updated_at = ?4
            WHERE id = ?1 AND revision = ?5"#,
    )
    .bind(id)
    .bind(title)
    .bind(body)
    .bind(now)
    .bind(if_revision)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if changed == 0 {
        return Err(match wave_recipe_get_tx(tx, id).await? {
            Some(current) => CalmError::Conflict(format!(
                "wave recipe {id} is at revision {}, not {if_revision}",
                current.revision
            )),
            None => CalmError::NotFound(format!("wave recipe {id}")),
        });
    }
    // Read the row back rather than reconstructing it: `created_at` is not
    // in scope here, and a hand-built return value is exactly where a future
    // column silently gets a wrong default.
    wave_recipe_get_tx(tx, id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave recipe {id}")))
}

pub async fn wave_recipe_get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Option<WaveRecipe>> {
    let row: Option<(String, String, String, i64, i64, i64)> = sqlx::query_as(
        "SELECT id, title, body, revision, created_at, updated_at FROM wave_recipes WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(
        |(id, title, body, revision, created_at, updated_at)| WaveRecipe {
            id,
            title,
            body,
            revision,
            created_at,
            updated_at,
        },
    ))
}

pub async fn wave_recipe_delete_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<()> {
    let changed = sqlx::query("DELETE FROM wave_recipes WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    if changed == 0 {
        return Err(CalmError::NotFound(format!("wave recipe {id}")));
    }
    Ok(())
}
