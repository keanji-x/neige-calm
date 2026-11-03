//! Storage selection follows resource ownership, independent of live readiness.
use super::*;

#[tokio::test]
async fn u8b_canonical_managed_and_borrowed_named_attach_ignore_provider_readiness() {
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
    for (key, request, kind) in [
        ("borrowed", attached, "attached"),
        ("managed", managed, "managed"),
    ] {
        let (status, created) = create(&app, &cookie, key, request.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let track = created["id"].as_str().unwrap().to_owned();
        let (card, worker, frame) = fixture.carrier(&track).await;
        assert_eq!(frame.canonical_inputs.pending.len(), 1);
        assert!(frame.canonical_inputs.active.is_none());
        assert!(frame.canonical_inputs.attempts.is_empty());
        assert!(frame.header.last_thread_id.is_none());
        assert_eq!(fixture.counts(&card).await, (1, 1, 1, 0, 0));
        assert_eq!(created["workspace"]["kind"], kind);
        assert!(!state.shared_codex_appserver.is_running());
        assert!(state.harness.get(&worker).is_none());
        let before = counts(&fixture).await;
        let replay = create(&app, &cookie, key, request).await;
        assert_eq!(replay, (StatusCode::CREATED, created));
        assert_eq!(fixture.carrier(&track).await, (card, worker, frame));
        assert_eq!(counts(&fixture).await, before);
    }
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn u8b_nonhuman_registered_mcp_add_creates_attached_canonical_storage() {
    super::super::dispatcher_inputs_tests::u9_creator::attached_mcp_selection().await;
}
