use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_api_key_authenticates_readiness_and_catalog_without_subscription_fallback() {
    for offered in ["cached_token,xai.api_key", "xai.api_key,grok.com"] {
        let root = tempfile::tempdir().unwrap();
        let host = host_with_auth(
            root.path(),
            &AgentProvider::Grok,
            grok(
                json!({"ACP_FIXTURE_VERSION":"1.0.70","ACP_FIXTURE_AUTH_METHODS":offered,
            "XAI_API_KEY":"fixture-declared-api-key"}),
            ),
            "1.0.70",
            Some("xai.api_key"),
        );
        assert_eq!(verdict(&host, &AgentProvider::Grok).await, Verdict::Ready);
        let catalog = host.discover_catalog(&AgentProvider::Grok, None).await;
        assert!(!catalog.models.is_empty(), "{catalog:?}");
        let frames = lines(root.path(), "requests.jsonl");
        let auth: Vec<_> = frames
            .iter()
            .filter(|f| f["method"] == "authenticate")
            .collect();
        assert_eq!(auth.len(), 2);
        assert!(
            auth.iter()
                .all(|f| f["params"] == json!({"methodId":"xai.api_key"}))
        );
        assert!(
            lines(root.path(), "environment.jsonl")
                .iter()
                .all(|env| env["XAI_API_KEY"] == true)
        );
        assert!(all_stopped(root.path()));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_failure_never_falls_back_or_exposes_remote_credentials() {
    for (offered, behavior) in [
        ("cached_token", "ok"),
        ("cached_token,xai.api_key", "error"),
        ("cached_token,xai.api_key", "eof"),
        ("cached_token,xai.api_key", "silent"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let host = host_with_auth(
            root.path(),
            &AgentProvider::Grok,
            grok(
                json!({"ACP_FIXTURE_VERSION":"1.0.70", "ACP_FIXTURE_AUTH_METHODS":offered,
                "ACP_FIXTURE_AUTHENTICATE":behavior, "ACP_FIXTURE_AUTH_ERROR":"fixture-private-marker"}),
            ),
            "1.0.70",
            Some("xai.api_key"),
        );
        let checked = ProviderAvailabilityCache::default()
            .acp(&AgentProvider::Grok, Freshness::Recheck, &host)
            .await;
        let answer = serde_json::to_value(crate::agent_providers::ProviderAvailability::from(
            checked.clone(),
        ))
        .unwrap();
        let Verdict::Unavailable(reason) = checked.verdict else {
            panic!("API key {behavior} must fail closed");
        };
        assert_eq!(answer["authentication_method"], "xai.api_key");
        assert!(reason.contains("xai.api_key"), "{reason}");
        assert!(!reason.contains("fixture-private-marker"), "{reason}");
        assert!(!reason.contains("sign it in"), "{reason}");
        assert_eq!(
            methods(root.path()),
            if offered == "cached_token" {
                vec!["initialize"]
            } else {
                vec!["initialize", "authenticate"]
            }
        );
        if behavior == "error" {
            let catalog = host.discover_catalog(&AgentProvider::Grok, None).await;
            assert!(catalog.models.is_empty(), "{catalog:?}");
        }
        let frames = lines(root.path(), "requests.jsonl");
        assert!(frames.iter().all(|f| f["method"] != "session/new"));
        assert!(
            frames
                .iter()
                .filter(|f| f["method"] == "authenticate")
                .all(|f| f["params"] == json!({"methodId":"xai.api_key"}))
        );
        assert!(all_stopped(root.path()));
    }
}
