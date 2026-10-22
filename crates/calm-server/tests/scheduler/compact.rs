//! Compact admission and transient waiting without an installed Planner runtime.
use super::*;
use calm_server::db::sqlite::{CheckoutOccupancy, checkout_admission, checkout_occupancy};

#[tokio::test]
async fn compact_task_is_admitted_beside_writer_and_waits_without_worker_or_lease() {
    let boot = boot().await;
    let writer = plan_task(&boot.track_id, "writer", TaskKind::Codex, &[]);
    seed_projected_task(&boot, writer.clone()).await;
    let db = boot.repo.sqlite_db().unwrap();
    sqlx::query("UPDATE tasks SET status='running' WHERE id=?1")
        .bind(&writer.id)
        .execute(db.writer_pool())
        .await
        .unwrap();
    let (track, card, current) =
        resolve_report_for_track(boot.repo.as_ref(), boot.track_id.as_str())
            .await
            .unwrap();
    let body = format!(
        "{}\n{}",
        current.body,
        render_fence(
            "task",
            &json!({
                "key":"compact", "kind":"compact", "context":"Continue from verified task evidence.", "ready":true,"declared_by":"spec"
            })
        )
    );
    persist_report(
        boot.repo.as_ref(),
        &boot.events,
        &boot.write,
        ActorId::KernelDispatcher,
        EditAuthor::Planner,
        track,
        card,
        current.clone(),
        TrackReportPayload::new(current.summary, body),
        current.doc_rev,
        None,
    )
    .await
    .unwrap();
    let id = format!("{}:compact", boot.track_id);
    let compact = boot.repo.task_get(&id).await.unwrap().unwrap();
    assert_eq!(compact.kind, TaskKind::Compact);
    assert_eq!(compact.status, TaskStatus::Pending);
    assert!(!compact.runs_in_track_checkout());
    assert!(build_worker_payload(&compact).is_err());
    let mut conn = db.read_pool().acquire().await.unwrap();
    assert_eq!(
        checkout_occupancy(&mut conn, boot.track_id.as_str(), "")
            .await
            .unwrap(),
        CheckoutOccupancy::Busy
    );
    assert_eq!(
        checkout_admission(std::slice::from_ref(&compact), CheckoutOccupancy::Busy)[0].1,
        None
    );
    drop(conn);
    let before_operations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM operations")
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    let mut events = boot.events.subscribe();
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.schedule_track(boot.track_id.clone()).await;
    let waiting = boot.repo.task_get(&id).await.unwrap().unwrap();
    assert_eq!(waiting.status, TaskStatus::Dispatched);
    assert!(waiting.status_detail.is_none());
    assert!(waiting.worker_card_id.is_none());
    assert!(waiting.running_deadline_ms.is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM workspace_leases")
            .fetch_one(db.read_pool())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM operations")
            .fetch_one(db.read_pool())
            .await
            .unwrap(),
        before_operations
    );
    let mut dispatched = 0;
    let mut failures = 0;
    while let Ok(event) = events.try_recv() {
        match &event.event {
            Event::TaskDispatched {
                idempotency_key,
                kind,
                ..
            } if idempotency_key == &id => {
                assert_eq!(kind, "compact");
                dispatched += 1;
            }
            Event::TaskFailed {
                idempotency_key, ..
            } if idempotency_key == &id => failures += 1,
            _ => {}
        }
    }
    assert_eq!((dispatched, failures), (1, 0));
    // Only the compact row remains active; it must never occupy the checkout.
    sqlx::query("UPDATE tasks SET status='done' WHERE id=?1")
        .bind(&writer.id)
        .execute(db.writer_pool())
        .await
        .unwrap();
    let mut conn = db.read_pool().acquire().await.unwrap();
    assert_eq!(
        checkout_occupancy(&mut conn, boot.track_id.as_str(), "")
            .await
            .unwrap(),
        CheckoutOccupancy::Free
    );
}
