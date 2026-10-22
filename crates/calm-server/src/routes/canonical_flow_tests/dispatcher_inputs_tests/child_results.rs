//! Child finality is decided by the real scheduler, then admitted by Dispatcher.
use super::*;
use calm_truth::db::RepoEventWrite as _;

async fn child_result(deleted: bool, incomplete: bool, gated: bool) {
    let mut flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let child = flow
        .fixture
        .repo
        .track_create(crate::model::NewTrack {
            area_id: flow.area.clone().into(),
            title: "result child".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET parent_track_id=?1,closed_at=?2 WHERE id=?3")
        .bind(&flow.track)
        .bind(crate::model::now_ms())
        .bind(child.id.as_str())
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    let task_id = crate::model::new_id();
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,declared_by,spawn,child_track_id,gate_json,created_at_ms,updated_at_ms) VALUES(?1,?2,'waiting-child','codex','child result','{}','running','spec','sub-track',?3,?4,?5,?5)")
        .bind(&task_id).bind(&flow.track).bind(child.id.as_str())
        .bind(gated.then(|| json!({"steps":[{"name":"verify","cmd":"true"}]}).to_string()))
        .bind(crate::model::now_ms()).execute(flow.fixture.repo.pool()).await.unwrap();
    if deleted {
        flow.fixture
            .repo
            .track_delete(child.id.as_str())
            .await
            .unwrap();
    } else if incomplete {
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,declared_by,spawn,created_at_ms,updated_at_ms) VALUES(?1,?2,'unfinished','codex','remaining work','{}','pending','spec','in-wave',?3,?3)")
            .bind(crate::model::new_id()).bind(child.id.as_str()).bind(crate::model::now_ms())
            .execute(flow.fixture.repo.pool()).await.unwrap();
    }
    flow.state
        .dispatcher
        .scheduler()
        .reconcile_child_track_for_test(child.id.as_str())
        .await
        .unwrap();
    let expected_status = if deleted || incomplete {
        crate::model::TaskStatus::Failed
    } else if gated {
        crate::model::TaskStatus::Verifying
    } else {
        crate::model::TaskStatus::Done
    };
    assert_eq!(
        flow.fixture
            .repo
            .task_get(&task_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        expected_status
    );
    if gated && !deleted && !incomplete {
        let rows = flow
            .fixture
            .repo
            .events_for_track(&flow.track, &["task.completed"], None)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        for row in rows {
            flow.state
                .dispatcher
                .catch_up_push(flow.track.clone().into(), row.event, row.id)
                .await;
        }
        assert!(
            flow.fixture
                .carrier(&flow.track)
                .await
                .2
                .canonical_inputs
                .pending
                .is_empty(),
            "a successful child still owes its parent's gate verdict"
        );
        flow.end(TURN).await;
        flow.fixture
            .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
            .await;
        flow.close().await;
        return;
    }
    for _ in 0..3 {
        flow.state.dispatcher.reconcile_tick_for_test().await;
    }
    let snapshot = flow.fixture.carrier(&flow.track).await.2;
    let accepted = snapshot.canonical_inputs.pending.len();
    // Always finish the original turn so a failing assertion does not leak its fake provider.
    flow.end(TURN).await;
    let next = flow.next().await;
    if let Some(call) = &next {
        assert!(
            call["params"]["input"].to_string().contains(&task_id),
            "{}",
            call["params"]["input"]
        );
        flow.accept(call, "child-result-turn").await;
        flow.end("child-result-turn").await;
    }
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.close().await;
    assert_eq!(
        accepted, 1,
        "final child result must be accepted even when its parent declared a gate"
    );
    assert!(
        next.is_some(),
        "child result must wake the actual Planner without another user message"
    );
}

#[tokio::test]
async fn gated_deleted_child_result_reconciles_and_wakes_parent() {
    child_result(true, false, true).await;
}

#[tokio::test]
async fn gated_incomplete_child_result_reconciles_and_wakes_parent() {
    child_result(false, true, true).await;
}

#[tokio::test]
async fn completed_child_result_reconciles_and_wakes_parent_once() {
    child_result(false, false, false).await;
}

#[tokio::test]
async fn completed_gated_child_remains_deferred_until_gate_verdict() {
    child_result(false, false, true).await;
}
