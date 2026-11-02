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

// Retirement owns a provision location; card/area presentation metadata is not
// part of that identity and cannot turn old unadmitted cleanup into a dependency.
#[derive(sqlx::FromRow)]
pub(super) struct ProvisionLocation {
    pub event_id: i64,
    pub track_id: String,
    pub path: String,
}

/// A removal cancels earlier provisions of the same track/path, even from another card.
/// Keep other tracks' claims visible so consumers can refuse conflicting ownership.
/// Events outlive entities: do not join to cards or tracks here.
pub async fn unmatched_provisioned_worktrees(
    conn: &mut SqliteConnection,
) -> Result<Vec<ProvisionedWorktree>> {
    unmatched(conn, Scope::All).await
}

pub(super) async fn provisioned_for_track(
    conn: &mut SqliteConnection,
    track: &str,
) -> Result<Vec<ProvisionLocation>> {
    unmatched(conn, Scope::Track(track)).await
}

pub(super) async fn current_provision(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<Option<ProvisionLocation>> {
    Ok(unmatched(conn, Scope::Event(id)).await?.into_iter().next())
}

enum Scope<'a> {
    All,
    Track(&'a str),
    Event(i64),
}

async fn unmatched<T>(conn: &mut SqliteConnection, scope: Scope<'_>) -> Result<Vec<T>>
where
    T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Send + Unpin,
{
    let mut query = sqlx::QueryBuilder::new(
        "SELECT e.id AS event_id,e.scope_track AS track_id,json_extract(e.payload,'$.card_id') AS card_id, \
         e.scope_area AS area_id,json_extract(e.payload,'$.path') AS path FROM events e WHERE e.kind='worktree.provisioned'",
    );
    match scope {
        Scope::All => {}
        Scope::Track(track) => {
            query.push(" AND e.scope_track=").push_bind(track);
        }
        Scope::Event(id) => {
            query.push(" AND e.id=").push_bind(id);
        }
    }
    query.push(" AND NOT EXISTS (SELECT 1 FROM events later WHERE later.id>e.id \
        AND later.scope_track=e.scope_track AND later.kind IN ('worktree.provisioned','worktree.removed') \
        AND json_extract(later.payload,'$.path')=json_extract(e.payload,'$.path')) ORDER BY e.id");
    Ok(query.build_query_as().fetch_all(conn).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::sqlite::{SqlxRepo, append_decision_event_in_tx},
        event::{Event, EventScope},
        ids::ActorId,
    };

    #[tokio::test]
    async fn unmatched_receipts_match_track_path_across_cards_and_keep_other_owners() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let write_db = (repo.db()).clone();
        write_db
            .writer()
            .with_writer(|write_session| {
                Box::pin(async move {
                    let mut tx = write_session.begin_immediate().await.unwrap();
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
                })
            })
            .await
            .expect("independent write operation")
    }
}
