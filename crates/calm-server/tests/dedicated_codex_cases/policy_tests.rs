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

#[tokio::test]
async fn executor_environment_statement_matches_generated_policy_document() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("environment").await;
    let text = std::fs::read_to_string(endpoint.home.home.join("config.toml")).unwrap();
    let document: toml_edit::DocumentMut = text.parse().unwrap();
    let environment = executor_environment();
    let strings = |value: &serde_json::Value| -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    let policy = &document["permissions"][DELIVERY_PROFILE];
    assert_eq!(
        environment["network"]["enabled"].as_bool().unwrap(),
        policy["network"]["enabled"].as_bool().unwrap()
    );
    assert_eq!(
        environment["network"]["web_search"].as_bool().unwrap(),
        document["web_search"].as_str().unwrap() != "disabled"
    );
    assert_eq!(
        environment["path"].as_str().unwrap(),
        document["shell_environment_policy"]["set"]["PATH"]
            .as_str()
            .unwrap()
    );
    let mut writable: Vec<String> = policy["filesystem"]
        .as_table_like()
        .unwrap()
        .iter()
        .filter(|(_, mode)| mode.as_str() == Some("write"))
        .map(|(path, _)| path.to_string())
        .collect();
    writable.sort();
    let mut stated = strings(&environment["workspace"]["writable"]);
    stated.sort();
    assert_eq!(stated, writable);
    assert_eq!(
        environment["workspace"]["root"].as_str().unwrap(),
        document["shell_environment_policy"]["set"]["HOME"]
            .as_str()
            .unwrap()
    );
    let allowed: Vec<String> = document["mcp_servers"]["calm"]["enabled_tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(strings(&environment["mcp_tools"]), allowed);
    let mut disabled: Vec<String> = document["features"]
        .as_table_like()
        .unwrap()
        .iter()
        .filter(|(_, on)| on.as_bool() == Some(false))
        .map(|(name, _)| name.to_string())
        .collect();
    disabled.sort();
    let mut stated = strings(&environment["disabled_features"]);
    stated.sort();
    assert_eq!(stated, disabled);
    let mut injected: Vec<String> = endpoint
        .launch_request()
        .mounts
        .iter()
        .filter_map(|mount| {
            mount
                .destination
                .strip_prefix("/provider-bin")
                .ok()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect();
    injected.sort();
    let mut stated = strings(&environment["provided_binaries"]);
    stated.sort();
    assert_eq!(stated, injected);
    assert_eq!(environment["executor"], "codex");
    assert_eq!(environment["workspace"]["fresh_per_attempt"], true);
    assert_eq!(environment["recovery"]["environment"], "identical");
    assert_eq!(environment["recovery"]["workspace"], "new");
    assert!(
        environment["host_usr"]
            .as_str()
            .unwrap()
            .contains("not enumerated")
    );
    assert!(RECOVER_CHANGES.contains("identical execution environment"));
    assert!(RECOVER_CHANGES.contains("only the workspace is new"));
}
