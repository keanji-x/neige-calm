use sqlx::Sqlite;
use sqlx::Transaction;

use super::infra::next_sort_scoped_in_tx;
use super::{
    WorkerSessionDeleteScope, clear_wave_root_session_refs_for_worker_session_delete_tx,
    overlay_delete_by_entity_tx,
};
use crate::card_role_cache::CardRoleCache;
use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::model::*;

pub async fn terminal_get_by_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<Option<Terminal>> {
    let row = sqlx::query_as::<_, Terminal>(
        r#"SELECT id, card_id, program, cwd, env, pid,
                  theme_fg, theme_bg, exit_code, signal_killed, created_at
           FROM terminals WHERE card_id = ?1"#,
    )
    .bind(card_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row)
}

/// Card-row insert that lets the caller pre-mint the row id.
///
/// Carved out from `card_create_tx` so atomic-card endpoints (terminal,
/// codex) can stamp the soon-to-exist card id into per-card sidecar paths
/// (e.g. `codex_homes_dir.join(card_id)`) *before* the row hits the DB,
/// without re-fetching the row after insert. The standalone
/// [`card_create_tx`] wrapper preserves the original "mint inside the
/// helper" contract for every other caller.
pub async fn card_create_with_id_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: String,
    p: NewCard,
    role: CardRole,
    // Issue #229 PR A — explicit, required: every call site must decide
    // whether the card is user-deletable. Per `[[required-over-option]]`
    // an `Option<bool>` with a serde default would silently hide the
    // wrong default at any future callsite (kernel-owned cards minted
    // as deletable would be a security regression). The three live
    // callers cover the policy:
    //   * `card_create_tx`              → `true`  (user-facing Worker cards)
    //   * dispatcher worker terminals    → `true`  (workers are user-facing)
    //   * `card_with_codex_create_tx`    → caller decides (`false` for spec)
    deletable: bool,
    card_role_cache: &CardRoleCache,
) -> Result<Card> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM waves WHERE id = ?1")
        .bind(p.wave_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("wave {}", p.wave_id)));
    }

    let sort = match p.sort {
        Some(s) => s,
        None => {
            next_sort_scoped_in_tx(tx, "cards", "WHERE wave_id = ?1", Some(p.wave_id.as_ref()))
                .await?
        }
    };
    let now = now_ms();
    let payload_text = serde_json::to_string(&p.payload)?;
    // `role` lands in the `cards.role` column added by migration 0008
    // (PR3, #136). User-facing card creation now uniformly passes
    // `CardRole::Worker`; wave-create passes `CardRole::Spec`.
    //
    // `deletable` lands in the column added by migration 0013 (#229 PR A).
    // SQLite has no native bool; we encode as `1` / `0`, matching the
    // column's `INTEGER NOT NULL DEFAULT 1` shape. sqlx maps `bool ↔ i64`
    // transparently via its `Encode<Sqlite>` impl, so the bind is direct.
    sqlx::query(
        r#"INSERT INTO cards
               (id, wave_id, kind, sort, payload, role, deletable, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
    )
    .bind(&id)
    .bind(p.wave_id.as_str())
    .bind(&p.kind)
    .bind(sort)
    .bind(&payload_text)
    .bind(role.as_db_str())
    .bind(deletable)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    // PR3 (#136) — write-through into the role cache. The cache update
    // happens *inside* the surrounding `write_with_event` transaction
    // so a follow-up emit in the same closure can see the freshly
    // minted role via `enforce_role`'s lookup. A txn rollback leaves a
    // stale entry; that's acceptable per the cache's documented
    // semantics — `enforce_role` denies in the only direction that
    // matters (unknown card) and the next boot's `seed_from_db` will
    // overwrite stale entries from the persisted truth.
    let card_id: CardId = id.into();
    card_role_cache.insert(card_id.clone(), role, p.wave_id.clone());
    Ok(Card {
        id: card_id,
        wave_id: p.wave_id,
        kind: p.kind,
        sort,
        payload: p.payload,
        runtime: None,
        deletable,
        created_at: now,
        updated_at: now,
    })
}

pub async fn card_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    p: NewCard,
    card_role_cache: &CardRoleCache,
) -> Result<Card> {
    // User-facing Worker cards are user-deletable by default — the user
    // added them via REST and can remove them the same way. Spec / report
    // cards take the explicit `false` route via
    // `card_with_codex_create_tx`.
    card_create_with_id_tx(tx, new_id(), p, CardRole::Worker, true, card_role_cache).await
}

pub async fn card_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: CardPatch,
) -> Result<Card> {
    let mut c = sqlx::query_as::<_, crate::db::rows::CardRow>(
        r#"SELECT id, wave_id, kind, sort, payload, deletable, created_at, updated_at
           FROM cards WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .map(Card::from)
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
    // Issue #229 PR A — `p.deletable` is intentionally ignored here.
    // The route handler in `routes/cards.rs::update_card` returns 400
    // when a client sends the field; the field exists on `CardPatch`
    // only to make that 400 explicit (rather than serde silently
    // dropping an unknown field). The UPDATE statement below also
    // doesn't touch the `deletable` column — defense in depth.
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
    .bind(c.id.as_str())
    .execute(&mut **tx)
    .await?;
    Ok(c)
}

/// Issue #247 PR1 — wave-report-specific transactional update that
/// rewrites both the legacy `payload` JSON column AND the new opaque
/// CRDT blob in `body_crdt` in one statement. Wraps [`card_update_tx`]
/// for the JSON+timestamps path, then re-runs a single UPDATE to
/// stamp the blob. Both writes happen inside the supplied `tx` so a
/// rollback drops them together — the JSON cache and the CRDT
/// authoritative bytes never drift.
///
/// `body_crdt` is the `automerge::AutoCommit::save()` bytes from
/// `wave_report_doc::ReportDoc::to_bytes`; the kernel never
/// interprets the column outside of the round-trip via that module.
///
/// This is a **wave-report-only** seam. Terminal / codex /
/// plugin cards continue going through `card_update_tx`, which never
/// touches `body_crdt` — the column stays NULL on those rows forever.
pub async fn card_update_with_crdt_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: CardPatch,
    body_crdt: Vec<u8>,
) -> Result<Card> {
    // Reuse the existing JSON+timestamps update path so the two
    // codepaths can't drift on what `updated_at` / payload-text
    // semantics look like.
    let card = card_update_tx(tx, id, p).await?;
    // Second statement: stamp the opaque CRDT bytes onto the row.
    // Split into its own UPDATE (rather than extending the one above)
    // so plain `card_update_tx` callers never sqlx-bind a `Vec<u8>`
    // they don't care about. The combined cost is one extra UPDATE
    // per wave-report write, which is dominated by the surrounding
    // event-emit work.
    sqlx::query(r#"UPDATE cards SET body_crdt = ?1 WHERE id = ?2"#)
        .bind(&body_crdt)
        .bind(card.id.as_str())
        .execute(&mut **tx)
        .await?;
    Ok(card)
}

/// Issue #247 PR1 — read the opaque CRDT blob for a card inside an
/// open transaction. Returns `None` in either of two cases:
///
///   * the card row doesn't exist (fetched via `fetch_optional` —
///     no `NotFound` is raised, the absent row collapses into the
///     same "no blob to load" signal as a NULL column), or
///   * the row exists but `body_crdt` IS NULL (every pre-PR1 row,
///     plus non-wave-report cards which never get initialized).
///
/// Returns `Some(bytes)` for any row whose first post-PR1 write has
/// run through `card_update_with_crdt_tx`.
///
/// Read inside the same tx as the update so a concurrent writer
/// can't slip a blob in between this read and our `to_bytes` write
/// (the wave-report write path is the only writer of the column
/// today, but pinning the read to the tx is cheap and matches the
/// pattern the rest of `*_tx` uses).
pub async fn card_body_crdt_get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Option<Vec<u8>>> {
    let row: Option<(Option<Vec<u8>>,)> =
        sqlx::query_as(r#"SELECT body_crdt FROM cards WHERE id = ?1"#)
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(row.and_then(|(blob,)| blob))
}

pub async fn card_delete_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    card_role_cache: &CardRoleCache,
) -> Result<()> {
    clear_wave_root_session_refs_for_worker_session_delete_tx(
        tx,
        WorkerSessionDeleteScope::Card { card_id: id },
    )
    .await?;
    sqlx::query("DELETE FROM worker_sessions WHERE card_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;

    let res = sqlx::query("DELETE FROM cards WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("card {id}")));
    }
    // Not reached when a wave/cove delete cascades cards via FK — those
    // paths sweep card overlays in their own txn via
    // overlay_delete_card_overlays_by_wave_tx / overlay_delete_subtree_by_cove_tx.
    overlay_delete_by_entity_tx(tx, "card", id).await?;
    // PR3 (#136) — keep the role cache in lockstep with the table.
    // Like the insert-side write-through, this happens before commit;
    // a txn rollback would leave the cache temporarily missing an
    // entry. The consequence is at worst an `enforce_role` deny on a
    // re-emit that would have been allowed (the card still exists),
    // which is the *safe* failure mode for an auth gate.
    card_role_cache.remove(&CardId::from(id));
    Ok(())
}

pub async fn terminal_delete_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<()> {
    let res = sqlx::query("DELETE FROM terminals WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("terminal {id}")));
    }
    Ok(())
}

/// Transactional terminal-row insert. Structural twin of the `terminal_create`
/// method on `SqlxRepo` — same parent-card-exists and per-card uniqueness
/// pre-checks, same `NotFound` / `Conflict` mapping — but composable inside
/// `Repo::write_with_event` closures alongside the card write.
///
/// Currently only invoked from `card_with_terminal_create_tx`; the standalone
/// `RepoOutOfDomain::terminal_create` path still talks to the pool directly so
/// the existing `POST /api/cards/:id/terminal` recipe keeps its behavior
/// untouched until #13 PR2 swaps it out.
pub async fn terminal_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    p: NewTerminal,
) -> Result<Terminal> {
    // Parent card must exist; surface as NotFound to mirror MockRepo.
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM cards WHERE id = ?1")
        .bind(p.card_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("card {}", p.card_id)));
    }
    // Per-card uniqueness — surface as Conflict to mirror MockRepo
    // (the schema also enforces this via UNIQUE on terminals.card_id).
    let dup: Option<(String,)> = sqlx::query_as("SELECT id FROM terminals WHERE card_id = ?1")
        .bind(p.card_id.as_str())
        .fetch_optional(&mut **tx)
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
    // #177 — theme is a write-once row invariant. Render the
    // `(r, g, b)` tuples to comma-decimal once at row creation so
    // every spawn path that reads this row can use the theme with zero
    // allocation.
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
    .execute(&mut **tx)
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
