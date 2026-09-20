use sqlx::Sqlite;
use sqlx::Transaction;

use calm_types::track_report::TrackReportPayload;

use super::infra::next_sort_scoped_in_tx;
use super::overlay_delete_by_entity_tx;
use super::session_row::{
    WorkerSessionDeleteScope, clear_track_root_session_refs_for_worker_session_delete_tx,
};
use crate::card_role_cache::CardRoleCache;
use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::model::*;
use crate::validation::{SERVER_OWNED_TERMINAL_PAYLOAD_KEYS, server_owned_value_is_sticky};

pub async fn terminal_get_by_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<Option<Terminal>> {
    let row = sqlx::query_as::<_, Terminal>(
        r#"SELECT id, card_id, program, cwd, env, pid,
                  theme_fg, theme_bg, exit_code, signal_killed,
                  pty_output, pty_output_truncated, created_at
           FROM terminals WHERE card_id = ?1"#,
    )
    .bind(card_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row)
}

/// Card-row insert that lets the caller pre-mint the row id, so atomic-card endpoints can stamp it into per-card
/// sidecar paths before the row exists. Direct SQL and frozen migration seeds bypass the track-report guard below.
pub async fn card_create_with_id_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: String,
    p: NewCard,
    role: CardRole,
    // Explicit and required: every call site must decide whether the card is user-deletable; a hidden default
    // minting kernel-owned cards as deletable would be a security regression.
    deletable: bool,
    card_role_cache: &CardRoleCache,
) -> Result<Card> {
    // A track-report can only be born as a kernel-owned ReportCard with the canonical initial payload; all report
    // content arrives later through the persist boundary, which writes payload and CRDT together.
    if p.kind == "track-report" {
        if role != CardRole::ReportCard {
            return Err(CalmError::BadRequest(
                "track-report cards must be kernel-minted with the reportcard role",
            ));
        }
        let Ok(payload) = serde_json::from_value::<TrackReportPayload>(p.payload.clone()) else {
            return Err(CalmError::BadRequest(
                "track-report cards must be created with the canonical initial payload; \
                 use the report persist boundary to add content",
            ));
        };
        if payload != TrackReportPayload::initial() {
            return Err(CalmError::BadRequest(
                "track-report cards must be created with the canonical initial payload; \
                 use the report persist boundary to add content",
            ));
        }
    }
    // Missing tracks must return NotFound before sort allocation or either half of an atomic card create runs.
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM tracks WHERE id = ?1")
        .bind(p.track_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("track {}", p.track_id)));
    }
    let sort = match p.sort {
        Some(s) => s,
        None => {
            next_sort_scoped_in_tx(
                tx,
                "cards",
                "WHERE track_id = ?1",
                Some(p.track_id.as_ref()),
            )
            .await?
        }
    };
    let now = now_ms();
    let payload_text = serde_json::to_string(&p.payload)?;
    let title = p.title.filter(|t| !t.trim().is_empty());
    // SQLite has no native bool; `deletable` is stored as `1` / `0` and sqlx maps `bool ↔ i64` directly.
    sqlx::query(
        r#"INSERT INTO cards
               (id, track_id, kind, sort, payload, title, role, deletable, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
    )
    .bind(&id)
    .bind(p.track_id.as_str())
    .bind(&p.kind)
    .bind(sort)
    .bind(&payload_text)
    .bind(&title)
    .bind(role.as_db_str())
    .bind(deletable)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    // Write-through into the role cache *inside* the surrounding transaction so a follow-up emit in the same closure
    // sees the freshly minted role; a rollback leaves a stale entry the next boot's `seed_from_db` overwrites.
    let card_id: CardId = id.into();
    card_role_cache.insert(card_id.clone(), role, p.track_id.clone());
    Ok(Card {
        id: card_id,
        track_id: p.track_id,
        kind: p.kind,
        sort,
        payload: p.payload,
        title,
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
    // User-facing Worker cards are user-deletable; planner / report cards take the explicit `false` route.
    card_create_with_id_tx(tx, new_id(), p, CardRole::Worker, true, card_role_cache).await
}

async fn card_for_update_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<Card> {
    sqlx::query_as::<_, crate::db::rows::CardRow>(
        r#"SELECT id, track_id, kind, sort, payload, title, deletable, created_at, updated_at
           FROM cards WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .map(Card::from)
    .ok_or_else(|| CalmError::NotFound(format!("card {id}")))
}

async fn card_update_inner_tx(
    tx: &mut Transaction<'_, Sqlite>,
    mut c: Card,
    p: CardPatch,
) -> Result<Card> {
    if let Some(v) = p.kind {
        c.kind = v;
    }
    if let Some(v) = p.sort {
        c.sort = v;
    }
    if let Some(mut v) = p.payload {
        // The server-owned keys are sticky: the payload column is replaced wholesale, so each kernel-minted value in
        // `SERVER_OWNED_TERMINAL_PAYLOAD_KEYS` is re-inserted into the replacement and no writer can drop the hook routing
        // or the permissions audit trail by omission. A non-object replacement is refused; only creation mints them.
        let stored: Vec<(&str, serde_json::Value)> = SERVER_OWNED_TERMINAL_PAYLOAD_KEYS
            .iter()
            .filter_map(|key| {
                c.payload
                    .get(*key)
                    .filter(|value| server_owned_value_is_sticky(key, value))
                    .map(|value| (*key, value.clone()))
            })
            .collect();
        if let Some((first_key, _)) = stored.first() {
            let Some(map) = v.as_object_mut() else {
                return Err(CalmError::BadRequest(format!(
                    "card {} carries `{first_key}`; its payload must stay a JSON object",
                    c.id
                )));
            };
            for (key, value) in stored {
                map.insert(key.to_owned(), value);
            }
        }
        c.payload = v;
    }
    if let Some(v) = p.title {
        c.title = Some(v).filter(|t| !t.trim().is_empty());
    }
    // `p.deletable` is intentionally ignored (the route returns 400 when a client sends it) and the UPDATE never touches the column.
    c.updated_at = now_ms();
    let payload_text = serde_json::to_string(&c.payload)?;

    sqlx::query(
        r#"UPDATE cards SET kind = ?1, sort = ?2, payload = ?3, title = ?4, updated_at = ?5
           WHERE id = ?6"#,
    )
    .bind(&c.kind)
    .bind(c.sort)
    .bind(&payload_text)
    .bind(&c.title)
    .bind(c.updated_at)
    .bind(c.id.as_str())
    .execute(&mut **tx)
    .await?;
    Ok(c)
}

pub async fn card_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: CardPatch,
) -> Result<Card> {
    let existing = card_for_update_tx(tx, id).await?;
    let targets_report = p.kind.as_deref() == Some("track-report");
    let transitions_from_report = existing.kind == "track-report" && p.kind.is_some();
    let updates_report_payload = existing.kind == "track-report" && p.payload.is_some();
    if transitions_from_report || targets_report || updates_report_payload {
        return Err(CalmError::BadRequest(
            "track-report kind transitions and payloads must go through the report persist boundary \
             (POST /api/tracks/:id/report or the calm.report.* tools)",
        ));
    }
    card_update_inner_tx(tx, existing, p).await
}

/// Track-report-only update that rewrites the `payload` JSON AND the opaque `body_crdt` blob in one transaction,
/// so the JSON cache and the CRDT authoritative bytes never drift. `card_update_tx` never touches `body_crdt`.
pub async fn card_update_with_crdt_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: CardPatch,
    body_crdt: Vec<u8>,
) -> Result<Card> {
    // Reuse the JSON+timestamps update path so the two codepaths can't drift on `updated_at` / payload semantics.
    let existing = card_for_update_tx(tx, id).await?;
    if existing.kind != "track-report" {
        return Err(CalmError::BadRequest(
            "card_update_with_crdt_tx is restricted to track-report cards",
        ));
    }
    if p.kind.as_deref().is_some_and(|kind| kind != existing.kind) {
        return Err(CalmError::BadRequest(
            "card_update_with_crdt_tx cannot change card kind",
        ));
    }
    let card = card_update_inner_tx(tx, existing, p).await?;
    // A separate UPDATE so plain `card_update_tx` callers never bind a `Vec<u8>` they don't care about.
    sqlx::query(r#"UPDATE cards SET body_crdt = ?1 WHERE id = ?2"#)
        .bind(&body_crdt)
        .bind(card.id.as_str())
        .execute(&mut **tx)
        .await?;
    Ok(card)
}

/// Read the opaque CRDT blob for a card inside an open transaction. `None` when the row is absent or `body_crdt`
/// IS NULL (pre-CRDT rows and non-track-report cards); read in the same tx so a concurrent writer can't slip in between.
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

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use crate::db::RepoSyncDomainRaw;
    use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};

    async fn track(repo: &SqlxRepo) -> Track {
        let area = repo
            .area_create(NewArea {
                name: "cards".into(),
                color: "#123456".into(),
                sort: None,
            })
            .await
            .unwrap();
        repo.track_create(NewTrack {
            area_id: area.id,
            title: "track".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap()
    }

    fn new_card(track_id: &str, kind: &str) -> NewCard {
        NewCard {
            track_id: track_id.into(),
            kind: kind.into(),
            sort: None,
            payload: if kind == "track-report" {
                serde_json::to_value(TrackReportPayload::initial()).unwrap()
            } else {
                serde_json::json!({})
            },
            title: None,
        }
    }

    #[tokio::test]
    async fn track_report_create_refuses_non_canonical_payload() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let track = track(&repo).await;
        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let mut arbitrary_report = new_card(track.id.as_str(), "track-report");
        arbitrary_report.payload = serde_json::json!({"arbitrary": "fixture payload"});
        let error = card_create_with_id_tx(
            &mut tx,
            "bad_report".into(),
            arbitrary_report,
            CardRole::ReportCard,
            false,
            repo.card_role_cache(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "bad request: track-report cards must be created with the canonical initial payload; \
             use the report persist boundary to add content"
        );
    }

    #[tokio::test]
    async fn track_report_create_accepts_canonical_payload() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let track = track(&repo).await;
        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let canonical_report = new_card(track.id.as_str(), "track-report");
        let accepted = card_create_with_id_tx(
            &mut tx,
            "canonical_report".into(),
            canonical_report.clone(),
            CardRole::ReportCard,
            false,
            repo.card_role_cache(),
        )
        .await
        .unwrap();
        assert_eq!(accepted.payload, canonical_report.payload);
    }

    #[tokio::test]
    async fn track_report_create_accepts_canonical_payload_with_null_blocks() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let track = track(&repo).await;
        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let mut canonical_report = new_card(track.id.as_str(), "track-report");
        canonical_report.payload["blocks"] = serde_json::Value::Null;
        card_create_with_id_tx(
            &mut tx,
            "canonical_report_with_null_blocks".into(),
            canonical_report,
            CardRole::ReportCard,
            false,
            repo.card_role_cache(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn track_report_create_refuses_non_reportcard_role() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let track = track(&repo).await;
        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let error = card_create_with_id_tx(
            &mut tx,
            "bad_role".into(),
            new_card(track.id.as_str(), "track-report"),
            CardRole::Worker,
            false,
            repo.card_role_cache(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "bad request: track-report cards must be kernel-minted with the reportcard role"
        );
    }

    #[tokio::test]
    async fn raw_track_report_create_mints_kernel_owned_reportcard() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let track = track(&repo).await;
        let card = repo
            .card_create(new_card(track.id.as_str(), "track-report"))
            .await
            .unwrap();
        let (role, deletable): (String, bool) =
            sqlx::query_as("SELECT role, deletable FROM cards WHERE id = ?1")
                .bind(card.id.as_str())
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(role, CardRole::ReportCard.as_db_str());
        assert!(!deletable);
        assert!(!card.deletable);
        assert_eq!(
            repo.card_role_cache().get(&card.id),
            Some(CardRole::ReportCard)
        );
    }

    #[tokio::test]
    async fn raw_track_report_create_refuses_second_report_for_track() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let track = track(&repo).await;
        repo.card_create(new_card(track.id.as_str(), "track-report"))
            .await
            .unwrap();
        let error = repo
            .card_create(new_card(track.id.as_str(), "track-report"))
            .await
            .unwrap_err();
        assert!(
            matches!(error, CalmError::Db(_)),
            "unique index error: {error:?}"
        );
    }

    #[tokio::test]
    async fn crdt_update_seam_rejects_non_report_and_kind_change() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let track = track(&repo).await;
        let worker = repo
            .card_create(new_card(track.id.as_str(), "worker"))
            .await
            .unwrap();
        let report = repo
            .card_create(new_card(track.id.as_str(), "track-report"))
            .await
            .unwrap();
        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let non_report =
            card_update_with_crdt_tx(&mut tx, worker.id.as_str(), CardPatch::default(), vec![1])
                .await
                .unwrap_err();
        assert_eq!(
            non_report.to_string(),
            "bad request: card_update_with_crdt_tx is restricted to track-report cards"
        );
        let kind_change = card_update_with_crdt_tx(
            &mut tx,
            report.id.as_str(),
            CardPatch {
                kind: Some("worker".into()),
                ..CardPatch::default()
            },
            vec![1],
        )
        .await
        .unwrap_err();
        assert_eq!(
            kind_change.to_string(),
            "bad request: card_update_with_crdt_tx cannot change card kind"
        );
    }
}

pub async fn card_delete_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    card_role_cache: &CardRoleCache,
) -> Result<()> {
    clear_track_root_session_refs_for_worker_session_delete_tx(
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
    // Not reached when a track/area delete cascades cards via FK — those paths sweep card overlays in their own txn.
    overlay_delete_by_entity_tx(tx, "card", id).await?;
    // Keep the role cache in lockstep with the table; a rollback leaves the cache missing an entry, which at worst
    // causes an `enforce_role` deny — the safe failure mode.
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

/// Transactional terminal-row insert, composable inside `write_with_event` closures alongside the card write.
/// This is the workspace freeze point: a `terminals` row stores a `cwd` that nothing re-anchors, so the freeze sits at
/// this low-level write entry rather than on the composites. `RepoOutOfDomain::terminal_create` bypasses it (test fixtures only).
pub async fn terminal_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    p: NewTerminal,
) -> Result<Terminal> {
    // Parent card must exist; `track_id` comes back on the same read because the freeze below needs it.
    let owner: Option<(String,)> = sqlx::query_as("SELECT track_id FROM cards WHERE id = ?1")
        .bind(p.card_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    let Some((track_id,)) = owner else {
        return Err(CalmError::NotFound(format!("card {}", p.card_id)));
    };
    // Per-card uniqueness, surfaced as Conflict (the schema also enforces it via UNIQUE on terminals.card_id).
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
    // Theme is a write-once row invariant: rendered to comma-decimal once so every spawn path can use it with zero allocation.
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
    // The track's workspace must stop being movable in the SAME transaction as the row it protects.
    // Deadlock hazard: a transaction must not read, off the pool, any table it has itself written before it commits —
    // under shared cache the pool connection blocks on a lock only the caller can release. Use the `*_tx` readers instead.
    super::track_workspace::track_workspace_freeze_tx(tx, &track_id, now).await?;
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

/// Persisted card identity used when deciding runtime capabilities in a write
/// transaction. Policy stays with the caller; this read owns only table shape.
pub struct CardExecutionShape {
    pub kind: String,
    pub role: CardRole,
    pub payload: serde_json::Value,
}

pub async fn card_execution_shape_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<CardExecutionShape> {
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT kind,role,payload FROM cards WHERE id=?1")
            .bind(card_id)
            .fetch_optional(&mut **tx)
            .await?;
    let (kind, role, payload) =
        row.ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    Ok(CardExecutionShape {
        kind,
        role: CardRole::try_from(role).map_err(CalmError::Internal)?,
        payload: serde_json::from_str(&payload)?,
    })
}
