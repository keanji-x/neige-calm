//! Single real request preparation never substitutes filesystem preparation or
//! reruns mutable configuration checks on an already accepted key.
use super::*;

pub(super) async fn accept_dormant_human_track(
    s: RouteState,
    source: HumanInputSource,
    headers: HeaderMap,
    request: CreateTrackRequest,
    advice: crate::planner_model::selection_advice::CatalogAdvice,
) -> Result<HumanTrackCreationResult> {
    match super::super::super::creation_plan::prepare_canonical_creation(
        request,
        &headers,
        s.workspace_root.clone(),
    )? {
        CreateShape::WithFirstMessage(plan) => {
            super::super::accept_dormant_human_track(s, source, plan, advice).await
        }
        CreateShape::MessageLess(_) => panic!("Human test requires first text"),
    }
}

#[tokio::test]
async fn original_key_replay_does_not_reprepare_a_changed_workspace_configuration() {
    let mut fx = Fixture::new().await;
    let accepted = fx.accept("original", FIRST).await.unwrap().ack;
    let before = fx.counts().await;
    fx.route.workspace_root = std::path::PathBuf::from("unsupported-relative-configuration");
    assert_replayed(fx.accept("original", FIRST).await.unwrap(), &accepted);
    assert!(matches!(
        fx.accept("fresh-key", FIRST).await,
        Err(CalmError::BadRequest(_))
    ));
    assert_eq!(fx.counts().await, before);
    assert!(!fx.root.exists());
    assert!(!fx.state.shared_codex_appserver.is_running());
}

#[tokio::test]
async fn a_request_requiring_attached_or_template_preparation_cannot_fall_back_to_blank() {
    let fx = Fixture::new().await;
    let mut attached = fx.request(FIRST);
    attached.cwd = Some(fx.root.to_string_lossy().into_owned());
    let mut template = fx.request(FIRST);
    template.template_id = Some("unavailable-template".into());
    let mut recipe = fx.request(FIRST);
    recipe.recipe_id = Some("unavailable-recipe".into());
    for request in [attached, template, recipe] {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", "no-fallback".parse().unwrap());
        assert!(matches!(
            accept_dormant_human_track(
                fx.route.clone(),
                fx.source.clone(),
                headers,
                request,
                crate::planner_model::selection_advice::CatalogAdvice::default()
            )
            .await,
            Err(CalmError::BadRequest(_))
        ));
        assert_eq!(fx.counts().await, (0, 0, 0, 0, 0, 0));
    }
    assert!(!fx.root.exists());
    assert!(!fx.state.shared_codex_appserver.is_running());
}

#[tokio::test]
async fn original_selection_replays_despite_changed_catalog_advice() {
    use crate::planner_model::selection_advice::{CatalogAdvice, EffortAdjustment};
    let fx = Fixture::new().await;
    let request = || {
        let mut request = fx.request(FIRST);
        request.model = Some("selected-model".into());
        request.reasoning_effort = Some("high".into());
        request
    };
    let headers = |key: &str| {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", key.parse().unwrap());
        headers
    };
    let accepted = accept_dormant_human_track(
        fx.route.clone(),
        fx.source.clone(),
        headers("selected"),
        request(),
        CatalogAdvice::default(),
    )
    .await
    .unwrap()
    .ack;
    let rejecting_advice = || CatalogAdvice {
        adjustment: Some(EffortAdjustment {
            to: Some("medium".into()),
        }),
        unknown_model: false,
        catalog_unavailable: false,
    };
    let before = fx.counts().await;
    assert_replayed(
        accept_dormant_human_track(
            fx.route.clone(),
            fx.source.clone(),
            headers("selected"),
            request(),
            rejecting_advice(),
        )
        .await
        .unwrap(),
        &accepted,
    );
    assert!(matches!(
        accept_dormant_human_track(
            fx.route.clone(),
            fx.source.clone(),
            headers("fresh"),
            request(),
            rejecting_advice(),
        )
        .await,
        Err(CalmError::BadRequest(_))
    ));
    assert_eq!(
        fx.counts().await,
        before,
        "invalid fresh selection allocates nothing"
    );
}

#[tokio::test]
async fn canonical_claude_creation_unsupported_shapes_keep_explicit_coverage() {
    use crate::routes::tracks::create::{CreationStorage, creation_storage_contract};
    let fx = Fixture::new().await;
    let original = serde_json::json!({"area_id":fx.area,"title":"new blank Track","planner_provider":"claude","first_message":FIRST,"theme":{"fg":[255,255,255],"bg":[0,0,0]}});
    for (field, value) in [
        ("admin", serde_json::json!(true)),
        ("cwd", serde_json::json!("/an-attached-directory")),
        ("attach_folder", serde_json::json!(true)),
        ("template_id", serde_json::json!("small-change")),
        ("recipe_id", serde_json::json!("a-recipe")),
        ("template_input", serde_json::json!({"a":"b"})),
        ("fork_report_from", serde_json::json!("a-track")),
        (
            "allow_cross_area_cwd",
            serde_json::json!({"folder_id":1,"area_id":"another-area"}),
        ),
    ] {
        let mut request = original.clone();
        request["planner_provider"] = serde_json::json!("claude");
        request[field] = value;
        let request = serde_json::from_value(request).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", "excluded".parse().unwrap());
        assert!(
            matches!(
                creation_storage_contract(&fx.route, &fx.source, &headers, &request)
                    .await
                    .unwrap(),
                CreationStorage::Historical
            ),
            "{field}"
        );
    }
    for provider in [AgentProvider::OpenCode, AgentProvider::Grok] {
        let mut request = fx.request(FIRST);
        request.planner_provider = provider;
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", "acp".parse().unwrap());
        assert!(matches!(
            creation_storage_contract(&fx.route, &fx.source, &headers, &request)
                .await
                .unwrap(),
            CreationStorage::Historical
        ));
    }
    assert_eq!(fx.counts().await, (0, 0, 0, 0, 0, 0));
}

#[tokio::test]
async fn canonical_claude_creation_selection_uses_actual_provider_catalog() {
    let fx = Fixture::new().await;
    let model = provider::claude::discovery::ClaudeModel {
        value: "claude-choice".into(),
        resolved_model: "claude-choice".into(),
        display_name: "Claude choice".into(),
        description: String::new(),
        effort_levels: vec!["high".into()],
    };
    fx.route
        .provider_availability
        .seed_claude_catalog(crate::claude_planner::models::ClaudeCatalog {
            default: model.clone(),
            models: vec![model],
            fetched_at_ms: crate::model::now_ms(),
        })
        .await;
    let codex = crate::shared_codex_appserver::SharedCodexAppServer::new_stub(fx.repo.clone());
    let advice = crate::routes::planner_model::creation_catalog_advice(
        &fx.route,
        &codex,
        &AgentProvider::Claude,
        Some("claude-choice"),
        Some("low"),
    )
    .await;
    assert!(
        matches!(
            advice.require_create_selection(Some("claude-choice"), Some("low")),
            Err(CalmError::BadRequest(_))
        ),
        "Claude effort refusal must come from Claude's catalog even with Codex down"
    );
    let advice = crate::routes::planner_model::creation_catalog_advice(
        &fx.route,
        &codex,
        &AgentProvider::Claude,
        Some("claude-choice"),
        Some("high"),
    )
    .await;
    advice
        .require_create_selection(Some("claude-choice"), Some("high"))
        .unwrap();
    assert!(!codex.is_running());
    assert_eq!(fx.counts().await, (0, 0, 0, 0, 0, 0));
}
