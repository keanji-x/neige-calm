#![cfg(unix)]
use crate::support::opencode_history as fixture;
use calm_server::{
    card_role_cache::CardRoleCache, db::sqlite::SqlxRepo, event::EventBus, state::WriteContext,
    track_area_cache::TrackAreaCache,
};
use calm_truth::db::RepoOutOfDomain as _;
use serde_json::{Value, json};
use std::sync::Arc;

#[tokio::test]
async fn opencode_history_actual_manifest_config_auth_and_scoped_results() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let fx = fixture::boot(
        repo.clone(),
        EventBus::new(),
        WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
        tmp.path(),
        false,
    )
    .await;
    let error = fx.host.spawn(fixture::HISTORY).await.unwrap_err();
    assert!(error.to_string().contains("history_config_path"));
    assert!(
        !fx.host
            .running_plugin_ids()
            .await
            .contains(fixture::HISTORY)
    );
    repo.plugin_update_user_config(fixture::HISTORY, fx.config.clone())
        .await
        .unwrap();
    fx.host.spawn(fixture::HISTORY).await.unwrap();
    assert!(fx.host.mcp_client(fixture::HISTORY).await.is_none());
    assert_eq!(fx.host.status(fixture::HISTORY).await.unwrap().pid, None);
    let calm_server::plugin_host::ConnectorClient::Builtin(client) =
        fx.host.connector_client(fixture::HISTORY).await.unwrap()
    else {
        panic!("builtin run")
    };
    let args = json!({"source_id":"synthetic","directory":"/work/allowed","session_id":"ses_allowed","limit":1});
    let result = client.tools_call("message_ls", &args).await.unwrap();
    let value = serde_json::to_value(result).unwrap();
    let text = value["content"][0]["text"].as_str().unwrap();
    assert!(text.contains(fixture::EXCERPT));
    let evidence: Value = serde_json::from_str(text).unwrap();
    assert!(evidence.get("coverage").is_some());
    assert!(evidence.get("digest").is_some());
    assert!(
        fx.http
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(_, _, auth)| auth == "Basic b3BlbmNvZGU6c3ludGhldGljLXBhc3N3b3Jk")
    );
    let before = fx.http.requests.lock().unwrap().len();
    let denied=client.tools_call("message_ls",&json!({"source_id":"synthetic","directory":"/work/denied","session_id":"ses_allowed","limit":1})).await.unwrap_err();
    assert_eq!(denied.code, -32602);
    assert_eq!(
        fx.http.requests.lock().unwrap().len(),
        before,
        "unconfigured scope makes no request"
    );
    fx.http
        .mixed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let denied = client
        .tools_call(
            "session_ls",
            &json!({"source_id":"synthetic","directory":"/work/allowed","roots":true,"limit":2}),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code, -32602);
    let value = serde_json::to_value(denied).unwrap();
    assert!(!value.to_string().contains("ses_SECRET"));
    {
        let requests = fx.http.requests.lock().unwrap();
        let query = &requests
            .iter()
            .find(|(path, _, _)| path == "/session")
            .unwrap()
            .1;
        assert!(query.contains("directory=%2Fwork%2Fallowed"));
        assert!(query.contains("roots=true"));
        assert!(query.contains("limit=2"));
        assert!(!query.contains("scope="));
    }
    fx.host.stop(fixture::HISTORY).await.unwrap();
}
