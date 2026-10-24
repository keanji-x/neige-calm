//! One authoritative projection of unmatched worktree receipts, across cards and tracks.
use crate::error::Result;
use sqlx::SqliteConnection;

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct ProvisionedWorktree {
    pub event_id: i64,
    pub track_id: String,
    pub card_id: String,
    pub area_id: String,
    pub path: String,
}

/// A removal cancels earlier provisions of the same track/path, even from another card.
/// Keep other tracks' claims visible so consumers can refuse conflicting ownership.
/// Events outlive entities: do not join to cards or tracks here.
pub async fn unmatched_provisioned_worktrees(
    conn: &mut SqliteConnection,
) -> Result<Vec<ProvisionedWorktree>> {
    Ok(sqlx::query_as(
        "SELECT e.id AS event_id, e.scope_track AS track_id, \
                json_extract(e.payload, '$.card_id') AS card_id, \
                e.scope_area AS area_id, json_extract(e.payload, '$.path') AS path \
         FROM events e WHERE e.kind = 'worktree.provisioned' \
         AND NOT EXISTS (SELECT 1 FROM events later WHERE later.id > e.id \
             AND later.scope_track = e.scope_track \
             AND later.kind IN ('worktree.provisioned', 'worktree.removed') \
             AND json_extract(later.payload, '$.path') = json_extract(e.payload, '$.path')) \
         ORDER BY e.id",
    )
    .fetch_all(conn)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::sqlite::{SqlxRepo, append_decision_event_in_tx, begin_immediate_tx},
        event::{Event, EventScope},
        ids::ActorId,
    };

    #[tokio::test]
    async fn unmatched_receipts_match_track_path_across_cards_and_keep_other_owners() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        async fn receipt(
            tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
            track: &str,
            card: &str,
            path: &str,
            removed: bool,
        ) {
            let event = if removed {
                Event::WorktreeRemoved {
                    track_id: track.into(),
                    card_id: card.into(),
                    path: path.into(),
                }
            } else {
                Event::WorktreeProvisioned {
                    track_id: track.into(),
                    card_id: card.into(),
                    path: path.into(),
                }
            };
            append_decision_event_in_tx(
                tx,
                &ActorId::KernelDispatcher,
                &EventScope::Card {
                    card: card.into(),
                    track: track.into(),
                    area: "area".into(),
                },
                None,
                &event,
            )
            .await
            .unwrap();
        }
        receipt(&mut tx, "owner", "a", "/extra", false).await;
        receipt(&mut tx, "other", "a", "/extra", false).await;
        receipt(&mut tx, "owner", "b", "/extra", true).await;
        let rows = unmatched_provisioned_worktrees(&mut tx).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].track_id, "other");
        receipt(&mut tx, "owner", "c", "/extra", false).await;
        receipt(&mut tx, "owner", "d", "/extra", false).await;
        receipt(&mut tx, "owner", "c", "relative", false).await;
        let rows = unmatched_provisioned_worktrees(&mut tx).await.unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].card_id, "d");
        assert_eq!(rows[2].path, "relative");
        // No live entity join: receipts remain queryable after their owners disappear.
        tx.commit().await.unwrap();
        assert_eq!(
            unmatched_provisioned_worktrees(&mut repo.pool().acquire().await.unwrap())
                .await
                .unwrap()
                .len(),
            3
        );
    }
}
