use super::*;

#[tokio::test]
async fn dedicated_codex_home_imports_only_provider_values_and_hides_shell_credentials() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("policy").await;
    let text = std::fs::read_to_string(endpoint.home.home.join("config.toml")).unwrap();
    let document: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(document["model"].as_str(), Some("normal"));
    assert!(document.get("hooks").is_none());
    assert!(document["mcp_servers"].get("untrusted").is_none());
    assert_eq!(
        document["mcp_servers"]["calm"]["env"]["NEIGE_MCP_TOKEN"].as_str(),
        Some("MCP_SECRET")
    );
    assert!(
        !document["shell_environment_policy"]
            .to_string()
            .contains("SECRET")
    );
    assert!(
        !document["shell_environment_policy"]
            .to_string()
            .contains("NEIGE_MCP")
    );
    assert_eq!(
        document["projects"]["/workspace"]["trust_level"].as_str(),
        Some("untrusted")
    );
    let policy = &document["permissions"][DELIVERY_PROFILE];
    assert_eq!(policy["network"]["enabled"].as_bool(), Some(false));
    for path in [
        "/provider/home",
        "/provider/control",
        "/provider/mcp",
        "/proc",
        "/workspace/.codex",
    ] {
        assert_eq!(policy["filesystem"][path].as_str(), Some("deny"));
    }
    assert_eq!(policy["filesystem"]["/workspace"].as_str(), Some("write"));
    assert!(
        policy["filesystem"]
            .get("/provider/home/tmp/arg0")
            .is_none()
    );
    for feature in ["code_mode", "multi_agent", "remote_control", "apps"] {
        assert_eq!(document["features"][feature].as_bool(), Some(false));
    }
    let visible = format!(
        "{:?} {:?} {}",
        f.seed,
        f.native,
        serde_json::to_string(&SessionRecord::prepared(endpoint)).unwrap()
    );
    assert!(!visible.contains("MCP_SECRET"));
    assert!(!visible.contains("AUTH_SECRET"));
}

#[tokio::test]
async fn dedicated_codex_changed_context_cannot_reuse_frozen_endpoint() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("context").await;
    let mut changed = endpoint.request.clone();
    changed.developer_instructions.push_str(" changed");
    assert!(matches!(
        f.controller.prepare(changed, &f.seed, &f.native).await,
        Err(Error::Conflict(_))
    ));
    assert_eq!(
        f.controller.probe(&endpoint).await.unwrap(),
        BoundaryState::Prepared
    );
}
