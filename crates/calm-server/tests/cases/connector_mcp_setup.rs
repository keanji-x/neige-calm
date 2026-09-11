use super::*;

fn setup_body(url: &str) -> Value {
    json!({"id": "test.json-mcp", "display_name": "JSON MCP", "url": url,
        "tools_all": true, "headers": {"Authorization": format!("Bearer {SECRET_VALUE}"), "X-Tenant": "tenant-team"}})
}

#[tokio::test]
async fn mcp_setup_check_discovers_all_pages_without_installing_or_calling_tools() {
    let stub = StubServer::start(StubMode::PaginatedTools).await;
    let b = boot().await;
    let host = b.host();
    let state = b.state(Arc::clone(&host));
    let before = get_text(&state, "/api/plugins").await;
    let before_files: Vec<_> = std::fs::read_dir(&b.plugins_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let (status, checked) =
        post_json(&state, "/api/plugins/mcp/check", setup_body(&stub.url())).await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    assert_eq!(checked["tools"], json!([ALLOWED_TOOL, ALLOWED_TOOL_2]));
    assert_eq!(get_text(&state, "/api/plugins").await, before);
    let after_files: Vec<_> = std::fs::read_dir(&b.plugins_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        after_files, before_files,
        "Check must not create a plugin or secrets tree"
    );
    assert!(host.running_plugin_ids().await.is_empty());
    assert!(!stub.methods().iter().any(|method| method == "tools/call"));
    assert!(
        stub.seen_tenants
            .lock()
            .unwrap()
            .iter()
            .all(|tenant| tenant == "tenant-team")
    );
    assert!(
        stub.auth_by_method()
            .iter()
            .all(|(_, auth)| auth == &format!("Bearer {SECRET_VALUE}"))
    );
}

#[tokio::test]
async fn mcp_setup_headers_stay_private_and_survive_install_and_restart() {
    let stub = StubServer::start(StubMode::EchoAuthInResults).await;
    let b = boot().await;
    let host = b.host();
    let state = b.state(Arc::clone(&host));
    let body = setup_body(&stub.url());
    let (status, checked) = post_json(&state, "/api/plugins/mcp/check", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    assert!(!checked.to_string().contains(SECRET_VALUE));
    let (status, installed) = post_json(
        &state,
        "/api/plugins/install",
        json!({"source": {
            "kind": "mcp_http_v2", "id": body["id"], "display_name": body["display_name"],
            "url": body["url"], "headers": body["headers"], "tools_all": true
        }}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{installed}");
    assert!(!installed.to_string().contains(SECRET_VALUE));
    let path = b.plugins_dir.join("test.json-mcp");
    let manifest = std::fs::read_to_string(path.join("manifest.json")).unwrap();
    assert!(!manifest.contains(SECRET_VALUE));
    let secrets = std::fs::read_to_string(path.join("secrets.json")).unwrap();
    assert!(secrets.contains(SECRET_VALUE));
    assert_eq!(
        std::fs::metadata(path.join("secrets.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let (status, enabled) = post_json(&state, "/api/plugins/test.json-mcp/enable", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    let host2 = b.host();
    host2.autospawn_enabled().await;
    assert!(host2.running_plugin_ids().await.contains("test.json-mcp"));
    let public = host2
        .registry()
        .get("test.json-mcp")
        .unwrap()
        .to_json()
        .to_string();
    assert!(!public.contains(SECRET_VALUE));
    assert!(!public.contains("tenant=tenant-team"));
    assert!(
        stub.seen_tenants
            .lock()
            .unwrap()
            .iter()
            .all(|tenant| tenant == "tenant-team")
    );
    assert!(
        stub.auth_by_method()
            .iter()
            .all(|(_, auth)| auth == &format!("Bearer {SECRET_VALUE}"))
    );
}

#[tokio::test]
async fn mcp_setup_check_rejects_bad_headers_and_auth_failures_without_writes() {
    let stub = StubServer::start(StubMode::EchoAuthIn4xx).await;
    let b = boot().await;
    let host = b.host();
    let state = b.state(Arc::clone(&host));
    let (status, failed) =
        post_json(&state, "/api/plugins/mcp/check", setup_body(&stub.url())).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{failed}");
    assert!(!failed.to_string().contains(SECRET_VALUE));
    assert!(
        !failed
            .to_string()
            .contains(&SECRET_VALUE[..KEY_STRADDLE_TAIL])
    );
    assert!(!failed.to_string().contains("tenant=tenant-team"));
    for headers in [
        json!({"Host":"bad.example"}),
        json!({"X-Key":"secret\r\nvalue"}),
        json!({"X-Key":"secret-alpha", "x-key":"secret-bravo"}),
    ] {
        let mut body = setup_body(&stub.url());
        body["headers"] = headers;
        let (status, failed) = post_json(&state, "/api/plugins/mcp/check", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{failed}");
    }
    assert!(!b.plugins_dir.join("test.json-mcp").exists());
}

async fn assert_header_values_refused(values: &[&str]) {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    let host = b.host();
    let state = b.state(Arc::clone(&host));
    for value in values {
        let mut body = setup_body(&stub.url());
        body["headers"] = json!({"X-Tenant":value});
        let (status, checked) = post_json(&state, "/api/plugins/mcp/check", body.clone()).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "unsupported header value must fail before discovery: {checked}"
        );
        body["kind"] = json!("mcp_http_v2");
        let (status, installed) =
            post_json(&state, "/api/plugins/install", json!({"source":body})).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "same validation before persisting: {installed}"
        );
        assert!(!b.plugins_dir.join("test.json-mcp").exists());
    }
    assert!(
        stub.methods().is_empty(),
        "invalid headers must never leave the host"
    );
}

#[tokio::test]
async fn mcp_setup_check_and_install_refuse_empty_header_values() {
    assert_header_values_refused(&["", "   "]).await;
}

#[tokio::test]
async fn mcp_setup_check_and_install_refuse_outer_whitespace() {
    assert_header_values_refused(&[" sk-private-tenant ", "sk-private-tenant "]).await;
}

#[tokio::test]
async fn mcp_setup_check_and_install_refuse_unsafe_short_header_values() {
    assert_header_values_refused(&["a", "e", "tools", "result", "12345678", "redacted>y"]).await;
}
