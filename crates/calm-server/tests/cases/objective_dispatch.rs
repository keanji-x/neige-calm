//! Notify execution through the existing serial projector and durable Planner event path.
use crate::mcp_track_report::{Boot, boot, planner_identity, upsert_block};
use calm_server::track_planner_context::TrackPlannerContextProjector;
use calm_truth::objective_host::{self, TurnUsage};
use serde_json::json;

async fn setup() -> Boot {
    let boot = boot().await;
    let db = boot.repo.sqlite_db().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&db)
        .await
        .unwrap();
    calm_server::db::sqlite::track_capabilities::stamp_tx(
        &mut tx,
        &boot.track_id,
        &[calm_types::track_capability::TrackCapability::Objectives],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    upsert_block(&boot, planner_identity(&boot), json!({"kind":"objective", "payload":{
        "key":"stage", "title":"阶段一", "mode":"converge",
        "checks":[{"id":"accepted","role":"goal","config":{"kind":"fact","params":{"fact":"task_accepted","subject":"finish"}}}],
        "constraints":[],"budget":{"tokens":10,"turns":1000,"wall_secs":10000},"compact":"none"
    }})).await.unwrap();
    boot
}
fn projector(boot: &Boot) -> TrackPlannerContextProjector {
    TrackPlannerContextProjector::new(
        boot.repo.clone(),
        boot.ctx.events.clone(),
        boot.ctx.write.clone(),
    )
    .unwrap()
}
async fn usage(boot: &Boot) {
    objective_host::record_turn_usage(
        &boot.repo.sqlite_db().unwrap(),
        &boot.track_id,
        "planner-session".into(),
        "over".into(),
        TurnUsage {
            context_growth: 11,
            secs: 1,
        },
    )
    .await
    .unwrap();
}
async fn wakes(boot: &Boot) -> Vec<calm_server::db::TrackEvent> {
    boot.repo
        .events_for_track(boot.track_id.as_str(), &["track.wake_requested"], None)
        .await
        .unwrap()
}

#[tokio::test]
async fn objective_dispatch_budget_once_and_restart() {
    let boot = setup().await;
    usage(&boot).await;
    projector(&boot)
        .recompute_track(boot.track_id.as_str())
        .await
        .unwrap();
    let events = wakes(&boot).await;
    assert_eq!(events.len(), 1);
    let calm_server::event::Event::TrackWakeRequested { text, .. } = &events[0].event else {
        panic!()
    };
    for expected in ["stage", "阶段一", "预算", "10", "已用", "11"] {
        assert!(text.contains(expected), "{text}");
    }
    // New executor instance, same persisted database: boot recovery must not redeliver.
    projector(&boot)
        .recompute_track(boot.track_id.as_str())
        .await
        .unwrap();
    assert_eq!(wakes(&boot).await.len(), 1);
    assert!(
        objective_host::unsettled_intents(&boot.repo.sqlite_db().unwrap())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn objective_dispatch_backlog_superseded_without_wake() {
    let boot = setup().await;
    projector(&boot)
        .recompute_track(boot.track_id.as_str())
        .await
        .unwrap();
    assert!(wakes(&boot).await.is_empty());
    let db = boot.repo.sqlite_db().unwrap();
    let settlement: String = sqlx::query_scalar("SELECT settlement_json FROM objective_intents")
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&settlement).unwrap();
    assert_eq!(value["settlement"], "superseded");
    assert!(
        value["reason"]
            .as_str()
            .unwrap()
            .contains("ApprovalRequired")
    );
}

#[tokio::test]
async fn objective_dispatch_atomic_restart_after_delivery_before_settle() {
    let boot = setup().await;
    usage(&boot).await;
    let db = boot.repo.sqlite_db().unwrap();
    // Simulate process failure after inserting the wake, immediately before Delivered.
    sqlx::query(
        "CREATE TRIGGER reject_objective_wake BEFORE UPDATE ON objective_intents
        WHEN json_extract(NEW.settlement_json,'$.settlement')='delivered'
        BEGIN SELECT RAISE(ABORT,'crash before settle'); END",
    )
    .execute(db.read_pool())
    .await
    .unwrap();
    assert!(
        projector(&boot)
            .recompute_track(boot.track_id.as_str())
            .await
            .is_err()
    );
    assert!(wakes(&boot).await.is_empty());
    let remaining = objective_host::unsettled_intents(&db).await.unwrap();
    assert!(remaining.iter().any(|row| matches!(
        row.intent,
        objective::Intent::Notify {
            notice: objective::engine::Notice::BudgetExhausted { .. }
        }
    )));
    sqlx::query("DROP TRIGGER reject_objective_wake")
        .execute(db.read_pool())
        .await
        .unwrap();
    projector(&boot)
        .recompute_track(boot.track_id.as_str())
        .await
        .unwrap();
    projector(&boot)
        .recompute_track(boot.track_id.as_str())
        .await
        .unwrap();
    assert_eq!(wakes(&boot).await.len(), 1);
}

#[tokio::test]
async fn objective_dispatch_done_from_dispatched_backlog() {
    let boot = setup().await;
    let db = boot.repo.sqlite_db().unwrap();
    // 5c has not exposed approval ingress yet. Seed a persisted engine Notice at
    // the executor boundary; delivery itself still runs through the production owner.
    sqlx::query("INSERT INTO objective_intents(track_id,event_seq,ordinal,kind,intent_json,
        effect_key,status,dispatched_at_ms,created_at_ms)
        SELECT track_id,event_seq,ordinal+1,kind,?,json_array('notify',event_seq,ordinal+1),'dispatched',1,created_at_ms FROM objective_intents")
        .bind(json!({"intent":"notify","notice":{"notice":"objective_done","objective":"stage"}}).to_string())
        .execute(db.read_pool()).await.unwrap();
    projector(&boot)
        .recompute_track(boot.track_id.as_str())
        .await
        .unwrap();
    projector(&boot)
        .recompute_track(boot.track_id.as_str())
        .await
        .unwrap();
    assert_eq!(wakes(&boot).await.len(), 1);
    let calm_server::event::Event::TrackWakeRequested { text, .. } = &wakes(&boot).await[0].event
    else {
        panic!()
    };
    assert!(text.contains("已完成"), "{text}");
    assert!(
        objective_host::unsettled_intents(&db)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn objective_dispatch_without_capability_does_not_scan_intents() {
    let boot = boot().await;
    // A missing intent table makes any accidental scan fail. The existing indexed
    // capability guard must avoid the entire dispatcher for ordinary tracks.
    sqlx::query("DROP TABLE objective_intents")
        .execute(boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
    projector(&boot)
        .recompute_track(boot.track_id.as_str())
        .await
        .unwrap();
    assert!(wakes(&boot).await.is_empty());
}
