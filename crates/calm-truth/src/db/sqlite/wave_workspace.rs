//! Issue #1147 S1 — the single writer of a wave's workspace.
//!
//! `wave_workspace_write_tx` is the only function that writes
//! `waves.workspace_kind` / `workspace_path` / `workspace_frozen_at`. That is
//! now a plain organizational fact rather than an invariant needing
//! enforcement: after migration 0077 there is exactly one stored copy of the
//! path, so there is nothing for a second writer to disagree with. The earlier
//! draft kept `waves.cwd` as a duplicate and tried to police the pair with a
//! source-text scanner; three rounds of red-teaming defeated three scanners,
//! and the column was deleted instead.
//!
//! A whole-value write (rather than a patch) is still the right shape: kind,
//! path and freeze stamp describe one decision and are always decided together.

use sqlx::{Sqlite, Transaction};

use crate::error::{CalmError, Result};
use crate::model::WaveWorkspace;

/// Write a wave's workspace — kind, path and freeze stamp — in one statement.
///
/// The freeze latch is *not* enforced here in S1. The only wave S1 can
/// re-point is the kernel-owned launchpad, which is deliberately never frozen
/// (design D9 exception), so a "reject writes when `workspace_frozen_at IS NOT
/// NULL`" guard would have no legal caller to reject — vacuous, and it would
/// have to be torn out again in S3 when the PATCH path needs to distinguish
/// "frozen" from "freezing". S3 owns the latch together with the code that can
/// trip it.
pub async fn wave_workspace_write_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
    workspace: &WaveWorkspace,
) -> Result<()> {
    let res = sqlx::query(
        r#"UPDATE waves
           SET workspace_path = ?1, workspace_kind = ?2, workspace_frozen_at = ?3
           WHERE id = ?4"#,
    )
    .bind(&workspace.path)
    .bind(workspace.kind.as_db_str())
    .bind(workspace.frozen_at)
    .bind(wave_id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("wave {wave_id}")));
    }
    Ok(())
}
