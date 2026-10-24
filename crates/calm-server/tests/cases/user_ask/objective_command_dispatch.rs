//! Serial dispatch and completion between authorization and attempt observation.
use super::*;
use calm_server::test_seams::{PausePoint, install_pause_for_test};

async fn snapshot(w: &World, stage: &str) {
    let db = w.boot.repo.sqlite_db().unwrap();
    for (name, query) in [
        (
            "operations",
            "SELECT json_object('id',id,'key',operation_key,'payload',json(payload_json),'phase',phase,\
             'from_phase',json_extract(phase_detail_json,'$.from_phase'),'phase_detail',json(phase_detail_json),\
             'compensation',json(compensation_state),'last_error',last_error,'output',json(tx_output_json)) FROM \
             operations WHERE kind='objective-check-run' ORDER BY created_at_ms,id",
        ),
        (
            "intents",
            "SELECT json_object('track',track_id,'seq',event_seq,'ordinal',ordinal,'kind',kind,'intent',\
             json(intent_json),'status',status,'settlement',json(settlement_json)) FROM objective_intents ORDER \
             BY event_seq,ordinal",
        ),
        (
            "events",
            "SELECT json_object('seq',seq,'kind',kind,'event',json(payload_json)) FROM objective_events ORDER BY seq",
        ),
    ] {
        let rows: Vec<String> = sqlx::query_scalar(query)
            .fetch_all(db.read_pool())
            .await
            .unwrap();
        eprintln!("{stage} {name}: [{}]", rows.join(","));
    }
}
async fn statuses(w: &World) -> Vec<String> {
    sqlx::query_scalar("SELECT CASE WHEN status='settled' THEN json_extract(settlement_json,'$.settlement') ELSE status END \
             FROM objective_intents WHERE kind='run_check' ORDER BY ordinal")
        .fetch_all(w.boot.repo.sqlite_db().unwrap().read_pool()).await.unwrap()
}
async fn entered(signal: &tokio::sync::Notify) {
    tokio::time::timeout(Duration::from_secs(10), signal.notified())
        .await
        .unwrap();
}
async fn completed(w: &World, n: usize) {
    // Observation only: do not project while the controlled dispatch holds its track lock.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let p = phases(w).await;
            if p.len() == n && p.iter().all(|p| p == "succeeded") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("real supervisor completion");
}

#[tokio::test]
async fn objective_command_one_active_operation_per_track() {
    let (first_hook, first_entered, first_resume) = pause();
    let (second_hook, second_entered, second_resume) = pause();
    let releases = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let w = world("true", |a| {
        a.with_before_release(Arc::new(move || {
            let (n, first, second) = (
                releases.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                first_hook.clone(),
                second_hook.clone(),
            );
            Box::pin(async move {
                match n {
                    0 => first().await,
                    1 => second().await,
                    _ => {}
                }
            })
        }))
    })
    .await;
    let mut d = definition();
    d["mode"] = json!({"loop":{"plateau":10}});
    d["checks"][0]["role"] = json!("ratchet");
    d["checks"][0]["config"] = json!({"kind":"command","params":{"command":"true"}});
    let mut second = d["checks"][0].clone();
    second["id"] = json!("second");
    d["checks"].as_array_mut().unwrap().push(second);
    define(&w.boot, Some(w.block.clone()), d).await;
    approve(&w).await;
    evaluate(&w, &w.revision).await.unwrap();
    entered(&first_entered).await;
    tokio::join!(project(&w), project(&w));
    snapshot(&w, "first-held").await;
    assert_eq!(phases(&w).await, ["parked"]);
    assert_eq!(statuses(&w).await, ["dispatched", "pending"]);
    assert_eq!(count(&w, "check_finished").await, 0);
    let second_id = objective_host::unsettled_intents(&w.boot.repo.sqlite_db().unwrap()).await.unwrap()
        .into_iter().find(|r| matches!(&r.intent, objective::Intent::RunCheck { check, .. } if check.as_str() == "second")).unwrap().id;
    first_resume.notify_one();
    completed(&w, 1).await;
    project(&w).await;
    entered(&second_entered).await;
    tokio::join!(project(&w), project(&w));
    snapshot(&w, "second-held").await;
    assert_eq!(phases(&w).await, ["succeeded", "parked"]);
    assert_eq!(statuses(&w).await, ["reported", "dispatched"]);
    assert_eq!(count(&w, "check_finished").await, 1);

    let seam = PausePoint::default();
    install_pause_for_test(
        "objective-command-snapshot",
        &second_id.to_string(),
        seam.clone(),
    );
    tokio::join!(project(&w), async {
        entered(&seam.entered).await;
        second_resume.notify_one();
        completed(&w, 2).await;
        snapshot(&w, "completed-during-dispatch").await;
        assert_eq!(statuses(&w).await, ["reported", "reported"]);
        assert_eq!(count(&w, "check_finished").await, 2);
        assert!(matches!(
            state(&w).await.objectives[&objective::ids::ObjectiveKey::new("kr")].evaluation,
            Some(objective::engine::Evaluation::Ready { .. })
        ));
        seam.release.notify_one();
    });
    snapshot(&w, "after-dispatch").await;
    assert_eq!(phases(&w).await, ["succeeded", "succeeded"]);
    settled(&w).await;
    let events_before: i64 = sqlx::query_scalar("SELECT count(*) FROM objective_events")
        .fetch_one(w.boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
    evaluate(&w, &w.revision).await.unwrap();
    tokio::join!(project(&w), project(&w));
    let events_after: i64 = sqlx::query_scalar("SELECT count(*) FROM objective_events")
        .fetch_one(w.boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
    assert_eq!(
        events_after, events_before,
        "same-revision replay appended an event"
    );
    snapshot(&w, "replay").await;
    assert_eq!(phases(&w).await, ["succeeded", "succeeded"]);
    assert_eq!(statuses(&w).await, ["reported", "reported"]);
    assert_eq!(count(&w, "check_finished").await, 2);
}
