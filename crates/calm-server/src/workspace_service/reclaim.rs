//! Compatibility recovery for old reclaim operations. New retirement uses the
//! authoritative workspace resource ledger and never constructs a delete request.
pub(crate) mod operation;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::location_admission::LocationAdmission;
    use crate::{
        error::CalmError,
        event::{Event, EventScope},
        ids::ActorId,
    };
    use calm_truth::db::sqlite::append_decision_event_in_tx;

    #[tokio::test]
    async fn retirement_durable_target_requires_no_live_guard_or_global_terminal_fence() {
        let repo = calm_truth::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("extra");
        let db = repo.db().clone();
        db.writer()
            .with_writer(|session| {
                Box::pin(async move {
                    let mut tx = session.begin_immediate().await.unwrap();
                    let provision = append_decision_event_in_tx(
                        &mut tx,
                        &ActorId::KernelDispatcher,
                        &EventScope::Card {
                            card: "card".into(),
                            track: "track".into(),
                            area: "area".into(),
                        },
                        None,
                        &Event::WorktreeProvisioned {
                            track_id: "track".into(),
                            card_id: "card".into(),
                            path: path.to_str().unwrap().into(),
                        },
                    )
                    .await
                    .unwrap();
                    calm_truth::db::sqlite::workspace_retirement::retire_extra_worktree_tx(
                        &mut tx, provision, "track", &path,
                    )
                    .await
                    .unwrap();
                    operation::terminal_admission_tx(&mut tx, "unrelated")
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();
                    let admission = LocationAdmission {
                        managed_root: root.path().join("managed"),
                        objective_checkout_root: root.path().join("objective"),
                        teardown_targets: Default::default(),
                        area_locks: crate::per_card_lock::new_keyed_locks(),
                        track_locks: crate::per_card_lock::new_keyed_locks(),
                    };
                    let mut conn = repo.pool().acquire().await.unwrap();
                    assert!(matches!(
                        admission.try_fence(&mut conn, &path, None).await,
                        Err(CalmError::BadRequest(_))
                    ));
                    admission
                        .try_fence(&mut conn, root.path(), None)
                        .await
                        .unwrap();
                })
            })
            .await
            .unwrap();
    }
}
