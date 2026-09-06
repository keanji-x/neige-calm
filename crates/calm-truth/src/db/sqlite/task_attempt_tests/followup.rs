//! Bounded storage followups: immutable gate evidence and allocation inventory.
use super::*;

#[tokio::test]
async fn task_recovery_allocation_inventory_includes_missing_rows_and_pages_by_key() {
    let repo = setup().await;
    let blocks = [block("a", &[]), block("b", &[]), block("c", &[])];
    project(&repo, &blocks).await;
    fail(&repo, &blocks[1]).await;
    let receipt = recover(&repo, &blocks[1]).await;
    // A recovery allocation exists before projection, and initial pending C may
    // be withdrawn. Neither can disappear from the logical task inventory.
    let mut withdrawn = blocks.clone();
    withdrawn[1].payload["ready"] = json!(false);
    withdrawn[2].payload["ready"] = json!(false);
    project(&repo, &withdrawn).await;
    let first = task_attempt_current_by_track_pool(repo.pool(), "w", None, 2)
        .await
        .unwrap();
    assert_eq!(
        first
            .iter()
            .map(|allocation| allocation.key.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(first[1].attempt_id, receipt.attempt_id);
    assert_eq!(first[1].generation, 2);
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let second = task_attempt_current_by_track_tx(&mut tx, "w", Some(&first[1].key), 2)
        .await
        .unwrap();
    assert_eq!(
        second
            .iter()
            .map(|allocation| allocation.key.as_str())
            .collect::<Vec<_>>(),
        ["c"]
    );
    assert!(
        task_attempt_current_by_track_tx(&mut tx, "w", Some("c"), 2)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        task_attempt_current_by_track_tx(&mut tx, "foreign", None, 500)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        task_attempt_current_by_track_tx(&mut tx, "w", None, 0)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        tasks_by_track_tx(&mut tx, "w")
            .await
            .unwrap()
            .iter()
            .map(|row| row.key.as_str())
            .collect::<Vec<_>>(),
        ["a"]
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn task_recovery_allocation_inventory_caps_page_size_without_silent_miss() {
    let repo = setup().await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    for index in 0..501 {
        // Exercise the production initial-registration trigger. Admission policy
        // is independent of this storage pagination boundary.
        let key = format!("k{index:04}");
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,created_at_ms,updated_at_ms) VALUES(?1,'w',?2,'terminal','true','{}',0,0)")
            .bind(format!("w:{key}")).bind(key).execute(&mut *tx).await.unwrap();
    }
    let first = task_attempt_current_by_track_tx(&mut tx, "w", None, i64::MAX)
        .await
        .unwrap();
    assert_eq!(first.len(), 500);
    assert_eq!(first[0].key, "k0000");
    assert_eq!(first[499].key, "k0499");
    let second = task_attempt_current_by_track_tx(&mut tx, "w", Some(&first[499].key), 500)
        .await
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].key, "k0500");
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn task_recovery_historical_gate_log_retains_execution_and_gate_identity() {
    use crate::model::CardRole;
    use crate::track_fs_view::{TrackFsError, TrackFsView, task_gate_log_path};
    let repo = setup().await;
    let mut b = block("b", &[]);
    b.payload["gate"] = json!({"steps":[{"name":"check","cmd":"true"}]});
    project(&repo, std::slice::from_ref(&b)).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    task_claim_pending_tx(&mut tx, "w:b", 1, constraint(&b).refs(), false)
        .await
        .unwrap();
    task_start_verifying_from_worker_tx(&mut tx, "w:b", "w", TaskReporter::Kernel, 2)
        .await
        .unwrap();
    task_gate_attempt_bump_tx(&mut tx, "w:b", 1, 3)
        .await
        .unwrap();
    task_gate_attempt_bump_tx(&mut tx, "w:b", 2, 4)
        .await
        .unwrap();
    task_apply_gate_result_tx(&mut tx, "w:b", 2, false, Some("gate-red"), "{}", 5)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let receipt = recover(&repo, &b).await;
    project(&repo, std::slice::from_ref(&b)).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    task_claim_pending_tx(
        &mut tx,
        &receipt.attempt_id,
        6,
        constraint(&b).refs(),
        false,
    )
    .await
    .unwrap();
    task_start_verifying_from_worker_tx(&mut tx, &receipt.attempt_id, "w", TaskReporter::Kernel, 7)
        .await
        .unwrap();
    task_gate_attempt_bump_tx(&mut tx, &receipt.attempt_id, 1, 8)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let logs = tempfile::tempdir().unwrap();
    for (file, text) in [
        ("w:b-g1.log".into(), "old first gate"),
        ("w:b-g2.log".into(), "old second gate"),
        (format!("{}-g1.log", receipt.attempt_id), "current gate"),
    ] {
        std::fs::write(logs.path().join(file), text).unwrap();
    }
    let write = crate::state::WriteContext::new(
        crate::card_role_cache::CardRoleCache::new(),
        crate::track_area_cache::TrackAreaCache::new(),
    );
    let track = repo.track_get("w").await.unwrap().unwrap();
    let view =
        TrackFsView::new(&repo, &write).with_gate_log_access(CardRole::Planner, logs.path().into());
    let historical = task_gate_log_path("w:b", 1).unwrap();
    assert_eq!(historical, "runs/w:b/gates/1.log");
    assert_eq!(
        view.cat(&track, &historical).await.unwrap().content,
        "old first gate"
    );
    assert_eq!(
        view.cat(&track, "runs/w:b/gates/2.log")
            .await
            .unwrap()
            .content,
        "old second gate"
    );
    assert_eq!(
        view.cat(&track, "plan/b/gate.log").await.unwrap().content,
        "current gate"
    );
    assert!(view.cat(&track, "runs/w:b/gates/3.log").await.is_err());
    for invalid in [
        "runs/w:b/gates/0.log",
        "runs/w:b/gates/01.log",
        "runs/w:b/gates/+1.log",
        "runs/w:b/gates/1.log/extra",
        "runs/w:b/gates/../1.log",
        "runs/w%3Ab/gates/1.log",
    ] {
        assert!(view.cat(&track, invalid).await.is_err(), "{invalid}");
    }
    sqlx::query("INSERT INTO tracks(id,area_id,title,sort,lifecycle,created_at,updated_at) VALUES('foreign','area','other',0,'working',0,0)")
        .execute(repo.pool()).await.unwrap();
    let foreign = repo.track_get("foreign").await.unwrap().unwrap();
    assert!(matches!(
        view.cat(&foreign, "runs/w:b/gates/1.log").await,
        Err(TrackFsError::Forbidden(_))
    ));
    for role in [CardRole::Worker, CardRole::Assistant] {
        let restricted =
            TrackFsView::new(&repo, &write).with_gate_log_access(role, logs.path().into());
        assert!(matches!(
            restricted.cat(&track, "runs/w:b/gates/1.log").await,
            Err(TrackFsError::Forbidden(_))
        ));
    }
    assert!(matches!(
        TrackFsView::new(&repo, &write)
            .cat(&track, "runs/w:b/gates/1.log")
            .await,
        Err(TrackFsError::Forbidden(_))
    ));
}
