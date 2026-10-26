//! Storage selection follows resource ownership, independent of live readiness.
use super::*;

#[tokio::test]
async fn u8b_canonical_managed_attach_keeps_historical_start_failure() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let a = area(&app, &cookie, "selection").await;
    let mut managed = body(&a, "codex");
    managed["attach_folder"] = json!(true);
    managed["template_id"] = json!("small-change");
    managed["first_message"] = json!(FIRST_BODY);
    // The same unavailable provider must not select a different storage owner.
    let path = repo(&fixture);
    let mut attached = managed.clone();
    attached["cwd"] = json!(path);
    attached["isolation"] = json!("none");
    let (status, created) = create(&app, &cookie, "borrowed", attached).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let (_, _, frame) = fixture.carrier(created["id"].as_str().unwrap()).await;
    assert_eq!(frame.canonical_inputs.pending.len(), 1);
    assert!(!state.shared_codex_appserver.is_running());
    let (status, failed) = create(&app, &cookie, "managed", managed).await;
    assert!(status.is_server_error(), "{status}: {failed}");
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn u8b_nonhuman_registered_mcp_add_creates_attached_canonical_storage() {
    super::super::dispatcher_inputs_tests::u9_creator::attached_mcp_selection().await;
}
