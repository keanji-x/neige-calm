use calm_truth::db::sqlite::SqlxRepo;
use calm_truth::db::sqlite::progress_obligation::*;

#[tokio::test]
async fn progress_obligation_invariant_deadline_and_release_are_monotonic() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let db = repo.db().clone();
    db.writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                let mut obligation = ProgressObligation {
                    execution_id: "attempt-1".into(),
                    purpose: "handoff".into(),
                    deadline_at_ms: 10,
                };
                assert_eq!(
                    progress_obligation_require_tx(&mut tx, &obligation)
                        .await
                        .unwrap(),
                    1
                );
                obligation.deadline_at_ms = 20;
                assert_eq!(
                    progress_obligation_require_tx(&mut tx, &obligation)
                        .await
                        .unwrap(),
                    0
                );
                assert!(
                    progress_obligations_due_tx(&mut tx, "handoff", 9)
                        .await
                        .unwrap()
                        .is_empty()
                );
                let due = progress_obligations_due_tx(&mut tx, "handoff", 10)
                    .await
                    .unwrap();
                assert_eq!(due.len(), 1);
                assert_eq!(due[0].deadline_at_ms, 10);
                progress_obligation_release_tx(&mut tx, "attempt-1", "handoff")
                    .await
                    .unwrap();
                assert!(
                    !progress_obligation_notify_tx(&mut tx, &due[0], 30)
                        .await
                        .unwrap()
                );
                assert_eq!(
                    progress_obligation_require_tx(&mut tx, &obligation)
                        .await
                        .unwrap(),
                    0
                );
                assert!(
                    progress_obligations_due_tx(&mut tx, "handoff", 30)
                        .await
                        .unwrap()
                        .is_empty()
                );
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn progress_obligation_invariant_notice_cas_survives_rollback() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let obligation = ProgressObligation {
        execution_id: "attempt-2".into(),
        purpose: "handoff".into(),
        deadline_at_ms: 10,
    };
    let db = repo.db().clone();
    db.writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                progress_obligation_require_tx(&mut tx, &obligation)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
                let mut tx = write_session.begin_immediate().await.unwrap();
                assert!(
                    !progress_obligation_notify_tx(&mut tx, &obligation, 9)
                        .await
                        .unwrap()
                );
                assert!(
                    progress_obligation_notify_tx(&mut tx, &obligation, 10)
                        .await
                        .unwrap()
                );
                assert!(
                    !progress_obligation_notify_tx(&mut tx, &obligation, 10)
                        .await
                        .unwrap()
                );
                tx.rollback().await.unwrap();
                let mut tx = write_session.begin_immediate().await.unwrap();
                assert_eq!(
                    progress_obligations_due_tx(&mut tx, "handoff", 10)
                        .await
                        .unwrap()
                        .len(),
                    1
                );
                assert!(
                    progress_obligation_notify_tx(&mut tx, &obligation, 10)
                        .await
                        .unwrap()
                );
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
}
