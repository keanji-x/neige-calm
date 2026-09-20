//! `track_recipes` storage. `revision` is the optimistic-lock anchor: every
//! write validates and bumps it in the same statement, so a stale writer
//! matches zero rows.

use sqlx::Sqlite;
use sqlx::Transaction;

use crate::error::{CalmError, Result};
use crate::model::*;

/// Insert a new recipe at `revision = 1`.
pub async fn track_recipe_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    title: &str,
    body: &str,
) -> Result<TrackRecipe> {
    let now = now_ms();
    let id = new_id();
    sqlx::query(
        r#"INSERT INTO track_recipes (id, title, body, revision, created_at, updated_at)
           VALUES (?1, ?2, ?3, 1, ?4, ?4)"#,
    )
    .bind(&id)
    .bind(title)
    .bind(body)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(TrackRecipe {
        id,
        title: title.to_string(),
        body: body.to_string(),
        revision: 1,
        created_at: now,
        updated_at: now,
    })
}

/// Replace a recipe's content iff `if_revision` still matches. `Conflict` for
/// a stale revision, `NotFound` for a missing row — told apart by a follow-up
/// read, since a zero row count cannot distinguish them.
pub async fn track_recipe_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    title: &str,
    body: &str,
    if_revision: i64,
) -> Result<TrackRecipe> {
    let now = now_ms();
    let changed = sqlx::query(
        r#"UPDATE track_recipes
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
        return Err(match track_recipe_get_tx(tx, id).await? {
            Some(current) => CalmError::Conflict(format!(
                "track recipe {id} is at revision {}, not {if_revision}",
                current.revision
            )),
            None => CalmError::NotFound(format!("track recipe {id}")),
        });
    }
    // Read the row back rather than reconstructing it: a hand-built return value
    // is where a future column silently gets a wrong default.
    track_recipe_get_tx(tx, id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track recipe {id}")))
}

pub async fn track_recipe_get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Option<TrackRecipe>> {
    let row: Option<(String, String, String, i64, i64, i64)> = sqlx::query_as(
        "SELECT id, title, body, revision, created_at, updated_at FROM track_recipes WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
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

pub async fn track_recipe_delete_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<()> {
    let changed = sqlx::query("DELETE FROM track_recipes WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    if changed == 0 {
        return Err(CalmError::NotFound(format!("track recipe {id}")));
    }
    Ok(())
}
