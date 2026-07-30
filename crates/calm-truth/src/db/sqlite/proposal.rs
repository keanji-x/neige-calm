//! Issue #955 §5 (PR-a) — `proposals` projection over the event log.
//!
//! The event log is the sole truth for proposal state (`proposal.submitted`
//! opens a pending row; `proposal.resolved` terminalizes it with one of
//! four decisions). This module maintains a rebuildable projection table
//! so three reads stay cheap **inside a write transaction**:
//!
//!   * the pending list per wave (adjudication UI / PR-b REST),
//!   * the pending count per `(plugin, wave)` (submit-time quota — must
//!     be transaction-consistent or concurrent submits break the cap,
//!     design §5.2),
//!   * the pending-scoped `(plugin, wave, idem_key)` lookup (submit
//!     idempotency, §5.2).
//!
//! ## Consistency model
//!
//! [`proposal_apply_event_tx`] is called from the single raw
//! events-table insert (`SqlxRepo::event_append_in_tx`) — the same
//! choke point that feeds `wave_vcs` — so the projection is updated in
//! the SAME transaction as every append, no matter which write wrapper
//! emitted the event. It is intentionally *tolerant*: a resolve for an
//! unknown/already-resolved proposal is a no-op (the log keeps the
//! truth; the projection never blocks an append it didn't understand).
//!
//! [`proposals_rebuild_tx`] drops and replays the projection from the
//! log — the recovery path proving the table carries no state of its
//! own. `wave.deleted` participates in the replay because the live
//! hook removes a deleted wave's rows (design §5.5: the projection
//! hides pending proposals of deleted waves; the append-only history
//! rows stay).

use sqlx::{Row, Sqlite, Transaction};

use crate::error::Result;
use crate::event::Event;

/// One projected `proposals` row. `ops` is the JSON serialization of
/// the event's `Vec<ProposalOp>` — kept as text because the projection
/// only stores/returns it; typed decoding happens at the consumer.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ProposalRow {
    pub proposal_id: String,
    pub wave_id: String,
    pub plugin_id: String,
    pub subject_kind: String,
    pub base_doc_heads: String,
    pub ops: String,
    pub note: String,
    pub idem_key: String,
    /// `pending` | `accepted` | `rejected` | `stale` | `withdrawn`
    /// (pinned to `ProposalDecision::as_str` for resolved rows).
    pub status: String,
    pub submitted_event_id: i64,
    pub resolved_event_id: Option<i64>,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
}

const SELECT_COLS: &str = "SELECT proposal_id, wave_id, plugin_id, subject_kind, \
     base_doc_heads, ops, note, idem_key, status, submitted_event_id, \
     resolved_event_id, created_at, resolved_at FROM proposals";

/// Fold one appended event into the projection. Called by
/// `SqlxRepo::event_append_in_tx` right after the events-table insert
/// (same tx), and by [`proposals_rebuild_tx`] during replay.
pub(super) async fn proposal_apply_event_tx(
    tx: &mut Transaction<'_, Sqlite>,
    event_id: i64,
    at: i64,
    event: &Event,
) -> Result<()> {
    match event {
        Event::ProposalSubmitted {
            wave_id,
            proposal_id,
            plugin_id,
            subject_kind,
            base_doc_heads,
            ops,
            note,
            idem_key,
        } => {
            let ops_text = serde_json::to_string(ops)?;
            // `ON CONFLICT DO NOTHING`: a duplicate proposal_id can only
            // come from a replayed row — first write wins, idempotent.
            sqlx::query(
                "INSERT INTO proposals (
                     proposal_id, wave_id, plugin_id, subject_kind,
                     base_doc_heads, ops, note, idem_key, status,
                     submitted_event_id, created_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9, ?10)
                 ON CONFLICT(proposal_id) DO NOTHING",
            )
            .bind(proposal_id)
            .bind(wave_id.as_str())
            .bind(plugin_id)
            .bind(subject_kind)
            .bind(base_doc_heads)
            .bind(&ops_text)
            .bind(note)
            .bind(idem_key)
            .bind(event_id)
            .bind(at)
            .execute(&mut **tx)
            .await?;
        }
        Event::ProposalResolved {
            proposal_id,
            decision,
            ..
        } => {
            // Only a pending row terminalizes; a second resolve for the
            // same proposal is a projection no-op (the emitting handler
            // is responsible for the 409, design §5.6 idempotency).
            sqlx::query(
                "UPDATE proposals
                 SET status = ?2, resolved_event_id = ?3, resolved_at = ?4
                 WHERE proposal_id = ?1 AND status = 'pending'",
            )
            .bind(proposal_id)
            .bind(decision.as_str())
            .bind(event_id)
            .bind(at)
            .execute(&mut **tx)
            .await?;
        }
        // Design §5.5 — `wave.deleted` is an append-only event (history
        // rows survive), so the projection is the layer that hides the
        // wave's proposals. Resolved rows go too: their history lives in
        // the log, and a dangling wave id serves no reader.
        Event::WaveDeleted { id, .. } => {
            sqlx::query("DELETE FROM proposals WHERE wave_id = ?1")
                .bind(id.as_str())
                .execute(&mut **tx)
                .await?;
        }
        _ => {}
    }
    Ok(())
}

/// Drop and rebuild the whole projection from the event log. The
/// recovery/backfill path — the log is the only input, so a projection
/// that drifted (or a fresh DB restored from events) converges to the
/// same rows the live hook would have produced.
pub async fn proposals_rebuild_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
    sqlx::query("DELETE FROM proposals")
        .execute(&mut **tx)
        .await?;
    let rows = sqlx::query(
        "SELECT id, kind, payload, at FROM events
         WHERE kind IN ('proposal.submitted', 'proposal.resolved', 'wave.deleted')
         ORDER BY id ASC",
    )
    .fetch_all(&mut **tx)
    .await?;
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let kind: String = row.try_get("kind")?;
        let payload_text: String = row.try_get("payload")?;
        let at: i64 = row.try_get("at")?;
        let payload: serde_json::Value = serde_json::from_str(&payload_text)?;
        // A persisted proposal/wave-deleted row that fails to decode is
        // corrupt Tier-A data — fail the rebuild loudly rather than
        // silently skipping (the projection would otherwise diverge
        // from the log while claiming to be derived from it).
        let event = Event::from_kind_and_payload(&kind, payload)?;
        proposal_apply_event_tx(tx, id, at, &event).await?;
    }
    Ok(())
}

/// Pending proposals for one wave, submission order.
pub async fn proposals_pending_by_wave_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
) -> Result<Vec<ProposalRow>> {
    let rows = sqlx::query_as::<_, ProposalRow>(&format!(
        "{SELECT_COLS} WHERE wave_id = ?1 AND status = 'pending' ORDER BY submitted_event_id ASC"
    ))
    .bind(wave_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows)
}

/// Pending count for one `(plugin, wave)` — the submit-time quota read
/// (design §5.2). Runs against the same transaction that will append
/// the `proposal.submitted` event, so concurrent submits serialize on
/// SQLite's single writer and cannot overshoot the cap.
pub async fn proposal_pending_count_tx(
    tx: &mut Transaction<'_, Sqlite>,
    plugin_id: &str,
    wave_id: &str,
) -> Result<i64> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM proposals
         WHERE plugin_id = ?1 AND wave_id = ?2 AND status = 'pending'",
    )
    .bind(plugin_id)
    .bind(wave_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(count)
}

/// Pending-scoped idempotency lookup: the pending proposal (if any)
/// holding `(plugin, wave, idem_key)`. Resolution releases the key —
/// resolved rows never match (design §5.2).
pub async fn proposal_pending_by_idem_tx(
    tx: &mut Transaction<'_, Sqlite>,
    plugin_id: &str,
    wave_id: &str,
    idem_key: &str,
) -> Result<Option<ProposalRow>> {
    let row = sqlx::query_as::<_, ProposalRow>(&format!(
        "{SELECT_COLS} WHERE plugin_id = ?1 AND wave_id = ?2 AND idem_key = ?3 \
         AND status = 'pending'"
    ))
    .bind(plugin_id)
    .bind(wave_id)
    .bind(idem_key)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row)
}

/// Single-proposal lookup by id (any status). PR-b's withdraw / accept
/// handlers re-check `status == "pending"` through this inside their
/// write tx (already-resolved ⇒ 409).
pub async fn proposal_get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    proposal_id: &str,
) -> Result<Option<ProposalRow>> {
    let row = sqlx::query_as::<_, ProposalRow>(&format!("{SELECT_COLS} WHERE proposal_id = ?1"))
        .bind(proposal_id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::super::SqlxRepo;
    use super::*;
    use crate::event::EventScope;
    use crate::ids::{ActorId, CoveId, WaveId};
    use calm_types::proposal::{ProposalDecision, ProposalOp};

    fn wave_scope(wave: &str) -> EventScope {
        EventScope::Wave {
            wave: WaveId::from(wave),
            cove: CoveId::from("c"),
        }
    }

    fn submitted(wave: &str, proposal: &str, plugin: &str, idem: &str) -> Event {
        Event::ProposalSubmitted {
            wave_id: WaveId::from(wave),
            proposal_id: proposal.into(),
            plugin_id: plugin.into(),
            subject_kind: "report".into(),
            base_doc_heads: "ah1:deadbeef".into(),
            ops: vec![ProposalOp::DeleteBlock {
                block_id: "b_0001".into(),
                if_rev: 1,
            }],
            note: "why".into(),
            idem_key: idem.into(),
        }
    }

    fn resolved(wave: &str, proposal: &str, plugin: &str, decision: ProposalDecision) -> Event {
        Event::ProposalResolved {
            wave_id: WaveId::from(wave),
            proposal_id: proposal.into(),
            plugin_id: plugin.into(),
            decision,
        }
    }

    async fn append(repo: &SqlxRepo, actor: ActorId, wave: &str, event: &Event) -> i64 {
        repo.event_append_fixture(actor, wave_scope(wave), None, event)
            .await
            .expect("append event")
    }

    async fn all_rows(repo: &SqlxRepo) -> Vec<ProposalRow> {
        let mut tx = repo.pool().begin().await.expect("begin");
        sqlx::query_as::<_, ProposalRow>(&format!(
            "{SELECT_COLS} ORDER BY submitted_event_id ASC"
        ))
        .fetch_all(&mut *tx)
        .await
        .expect("select all proposals")
    }

    #[tokio::test]
    async fn append_projects_pending_then_resolution_terminalizes() {
        let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
        let plugin = ActorId::Plugin("dev.neige.invest".into());

        let submit_id = append(
            &repo,
            plugin.clone(),
            "w1",
            &submitted("w1", "pp-1", "dev.neige.invest", "k1"),
        )
        .await;

        let mut tx = repo.pool().begin().await.expect("begin");
        let pending = proposals_pending_by_wave_tx(&mut tx, "w1")
            .await
            .expect("pending");
        assert_eq!(pending.len(), 1);
        let row = &pending[0];
        assert_eq!(row.proposal_id, "pp-1");
        assert_eq!(row.status, "pending");
        assert_eq!(row.submitted_event_id, submit_id);
        assert_eq!(row.subject_kind, "report");
        assert!(row.ops.contains("delete_block"), "ops JSON: {}", row.ops);
        assert_eq!(
            proposal_pending_count_tx(&mut tx, "dev.neige.invest", "w1")
                .await
                .expect("count"),
            1
        );
        assert!(
            proposal_pending_by_idem_tx(&mut tx, "dev.neige.invest", "w1", "k1")
                .await
                .expect("idem")
                .is_some()
        );
        drop(tx);

        let resolve_id = append(
            &repo,
            ActorId::User,
            "w1",
            &resolved("w1", "pp-1", "dev.neige.invest", ProposalDecision::Accepted),
        )
        .await;

        let mut tx = repo.pool().begin().await.expect("begin");
        assert!(
            proposals_pending_by_wave_tx(&mut tx, "w1")
                .await
                .expect("pending")
                .is_empty(),
            "resolved proposal must leave the pending list"
        );
        assert_eq!(
            proposal_pending_count_tx(&mut tx, "dev.neige.invest", "w1")
                .await
                .expect("count"),
            0,
            "resolution must release the quota slot"
        );
        assert!(
            proposal_pending_by_idem_tx(&mut tx, "dev.neige.invest", "w1", "k1")
                .await
                .expect("idem")
                .is_none(),
            "resolution must release the idem key (pending-scoped dedup)"
        );
        let row = proposal_get_tx(&mut tx, "pp-1")
            .await
            .expect("get")
            .expect("row");
        assert_eq!(row.status, "accepted");
        assert_eq!(row.resolved_event_id, Some(resolve_id));
        assert!(row.resolved_at.is_some());
    }

    #[tokio::test]
    async fn withdraw_and_stale_decisions_project_their_status() {
        let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
        let plugin = ActorId::Plugin("p1".into());
        for (proposal, decision, status) in [
            ("pp-w", ProposalDecision::Withdrawn, "withdrawn"),
            ("pp-s", ProposalDecision::Stale, "stale"),
            ("pp-r", ProposalDecision::Rejected, "rejected"),
        ] {
            append(
                &repo,
                plugin.clone(),
                "w1",
                &submitted("w1", proposal, "p1", proposal),
            )
            .await;
            let actor = if decision == ProposalDecision::Withdrawn {
                plugin.clone()
            } else {
                ActorId::User
            };
            append(
                &repo,
                actor,
                "w1",
                &resolved("w1", proposal, "p1", decision),
            )
            .await;
            let mut tx = repo.pool().begin().await.expect("begin");
            let row = proposal_get_tx(&mut tx, proposal)
                .await
                .expect("get")
                .expect("row");
            assert_eq!(row.status, status, "decision {decision:?}");
        }
    }

    #[tokio::test]
    async fn duplicate_resolve_is_a_projection_noop() {
        let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
        let plugin = ActorId::Plugin("p1".into());
        append(
            &repo,
            plugin.clone(),
            "w1",
            &submitted("w1", "pp-1", "p1", "k1"),
        )
        .await;
        let first = append(
            &repo,
            ActorId::User,
            "w1",
            &resolved("w1", "pp-1", "p1", ProposalDecision::Rejected),
        )
        .await;
        // A (buggy / replayed) second resolve must not overwrite the
        // first terminal decision.
        append(
            &repo,
            ActorId::User,
            "w1",
            &resolved("w1", "pp-1", "p1", ProposalDecision::Accepted),
        )
        .await;
        let mut tx = repo.pool().begin().await.expect("begin");
        let row = proposal_get_tx(&mut tx, "pp-1")
            .await
            .expect("get")
            .expect("row");
        assert_eq!(row.status, "rejected");
        assert_eq!(row.resolved_event_id, Some(first));
    }

    #[tokio::test]
    async fn wave_deleted_removes_the_waves_projection_rows() {
        let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
        let plugin = ActorId::Plugin("p1".into());
        append(
            &repo,
            plugin.clone(),
            "w1",
            &submitted("w1", "pp-1", "p1", "k1"),
        )
        .await;
        append(
            &repo,
            plugin.clone(),
            "w2",
            &submitted("w2", "pp-2", "p1", "k1"),
        )
        .await;
        append(
            &repo,
            ActorId::User,
            "w1",
            &Event::WaveDeleted {
                id: WaveId::from("w1"),
                cove_id: CoveId::from("c"),
            },
        )
        .await;
        let rows = all_rows(&repo).await;
        assert_eq!(rows.len(), 1, "only w2's proposal survives: {rows:?}");
        assert_eq!(rows[0].proposal_id, "pp-2");
    }

    #[tokio::test]
    async fn rebuild_replays_the_log_into_identical_rows() {
        let repo = SqlxRepo::open("sqlite::memory:").await.expect("open");
        let plugin = ActorId::Plugin("p1".into());
        append(
            &repo,
            plugin.clone(),
            "w1",
            &submitted("w1", "pp-1", "p1", "k1"),
        )
        .await;
        append(
            &repo,
            plugin.clone(),
            "w1",
            &submitted("w1", "pp-2", "p1", "k2"),
        )
        .await;
        append(
            &repo,
            ActorId::User,
            "w1",
            &resolved("w1", "pp-1", "p1", ProposalDecision::Accepted),
        )
        .await;
        append(
            &repo,
            plugin.clone(),
            "w3",
            &submitted("w3", "pp-3", "p1", "k1"),
        )
        .await;
        append(
            &repo,
            ActorId::User,
            "w3",
            &Event::WaveDeleted {
                id: WaveId::from("w3"),
                cove_id: CoveId::from("c"),
            },
        )
        .await;

        let live = all_rows(&repo).await;
        assert_eq!(live.len(), 2, "pp-1 resolved + pp-2 pending: {live:?}");

        // Corrupt the projection, then rebuild from the log alone.
        sqlx::query("UPDATE proposals SET status = 'garbage', note = 'tampered'")
            .execute(repo.pool())
            .await
            .expect("tamper");
        let mut tx = repo.pool().begin().await.expect("begin");
        proposals_rebuild_tx(&mut tx).await.expect("rebuild");
        tx.commit().await.expect("commit");

        let rebuilt = all_rows(&repo).await;
        assert_eq!(
            rebuilt, live,
            "rebuild must converge to the live projection"
        );
    }
}
