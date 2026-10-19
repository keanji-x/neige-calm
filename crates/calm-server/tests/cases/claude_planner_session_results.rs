//! Drive result optionality through the real session, using only fake CLI output.

use provider::TurnModelSelection;

use super::claude_planner_session_fixture::{
    Rig, client_id, completed_turn, request, until_completed,
};

async fn result_turn(scenario: &str, status: &str, error: Option<&str>) {
    let rig = Rig::new(scenario).await;
    let mut rx = rig.session().subscribe_events();
    let turn = rig
        .session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("hello"),
                &TurnModelSelection::inherit(),
                calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .expect("turn_start");
    let completed = completed_turn(&until_completed(&mut rx).await);
    assert_eq!(completed["id"], turn);
    assert_eq!(completed["status"], status, "{completed}");
    match error {
        Some(message) => assert_eq!(completed["error"]["message"], message),
        None => assert!(completed["error"].is_null(), "{completed}"),
    }
    let outcomes = rig.outcomes().await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0]["id"], turn);
    assert_eq!(outcomes[0]["status"], status);
    assert_eq!(rig.session().active_turn_id_for_thread(&rig.thread), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_terminal_reason_completes() {
    result_turn("result-no-reason", "completed", None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn null_terminal_reason_completes() {
    result_turn("result-null-reason", "completed", None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn is_error_without_terminal_reason_still_fails() {
    result_turn(
        "result-error-no-reason",
        "failed",
        Some("fake result error"),
    )
    .await;
}
