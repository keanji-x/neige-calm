//! Public creation, retained wake and reopen use the real ACP spy process, once per ACP
//! provider whose profile claims canonical Planner input.
use super::*;
use axum::extract::FromRef;
use calm_truth::db::RepoRead;
use calm_types::runtime::AgentProvider;

/// Each registration's spy logs under its own root: OpenCode's is the fixture root.
pub(super) fn peer_root(fixture: &Fixture, provider: &AgentProvider) -> std::path::PathBuf {
    match provider {
        AgentProvider::OpenCode => fixture.root.path().into(),
        AgentProvider::Grok => fixture.root.path().join("grok"),
        AgentProvider::Codex | AgentProvider::Claude => panic!("{provider:?} has no ACP spy"),
    }
}

/// The other configured ACP registration, for a request that changes only its provider.
fn other(provider: &AgentProvider) -> AgentProvider {
    match provider {
        AgentProvider::OpenCode => AgentProvider::Grok,
        AgentProvider::Grok => AgentProvider::OpenCode,
        AgentProvider::Codex | AgentProvider::Claude => panic!("{provider:?} has no ACP spy"),
    }
}

async fn body(app: &axum::Router, cookie: &str, provider: &AgentProvider) -> Value {
    let area = post(
        app,
        cookie,
        "/api/areas",
        "area",
        json!({"name":"ACP creation","color":"#123456"}),
    )
    .await;
    json!({"area_id":area["id"],"title":"","planner_provider":provider.wire_name(),
        "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY})
}

pub(super) fn configure(fixture: &mut Fixture) {
    fixture
        .execution
        .get_or_insert_with(crate::test_support::execution::Supervisor::new);
    let config = fixture.root.path().join("acp.json");
    std::fs::write(fixture.root.path().join("scenario"), "reply").unwrap();
    let grok = fixture.root.path().join("grok");
    std::fs::create_dir_all(&grok).unwrap();
    std::fs::write(grok.join("scenario"), "reply").unwrap();
    std::fs::write(&config, json!({"agents":[
        {"provider":"opencode","command":"/usr/bin/python3",
        "args":["-u",concat!(env!("CARGO_MANIFEST_DIR"),"/tests/fixtures/acp_planner/agent.py")],
        "env":{"ACP_FIXTURE_ROOT":fixture.root.path()},"min_agent_version":"1.0.0"},
        {"provider":"grok","command":"/usr/bin/python3",
        "args":["-u",concat!(env!("CARGO_MANIFEST_DIR"),"/tests/fixtures/acp_planner/agent.py")],
        "env":{"ACP_FIXTURE_ROOT":grok,"ACP_FIXTURE_IDENTITY":"meta","ACP_FIXTURE_VENDOR":"xai","ACP_FIXTURE_AUTH_METHODS":"cached_token","ACP_FIXTURE_VERSION":"1.0.62"},"min_agent_version":"1.0.0"}
    ]}).to_string()).unwrap();
    fixture.acp_config = Some(config);
}

fn prompts(fixture: &Fixture, provider: &AgentProvider) -> usize {
    std::fs::read_to_string(peer_root(fixture, provider).join("requests.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["method"] == "session/prompt")
        .count()
}

#[tokio::test]
async fn canonical_opencode_creation_accepts_before_ready_replays_and_wakes_after_restart() {
    accepts_before_ready_replays_and_wakes_after_restart(AgentProvider::OpenCode).await;
}

#[tokio::test]
async fn canonical_grok_creation_accepts_before_ready_replays_and_wakes_after_restart() {
    accepts_before_ready_replays_and_wakes_after_restart(AgentProvider::Grok).await;
}

async fn accepts_before_ready_replays_and_wakes_after_restart(provider: AgentProvider) {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let request = body(&app, &cookie, &provider).await;
    let created = post(&app, &cookie, "/api/tracks", "first", request.clone()).await;
    let track = created["id"].as_str().unwrap();
    let (card, worker, accepted) = fixture.carrier(track).await;
    assert_eq!(accepted.canonical_inputs.pending.len(), 1);
    assert!(accepted.canonical_inputs.attempts.is_empty());
    let receipt = fixture.receipt(request["area_id"].as_str().unwrap()).await;
    assert_eq!(
        post(&app, &cookie, "/api/tracks", "first", request.clone()).await["id"],
        created["id"]
    );
    for (field, value) in [
        ("first_message", json!("changed")),
        ("planner_provider", json!(other(&provider).wire_name())),
        ("model", json!("changed")),
    ] {
        let mut changed = request.clone();
        changed[field] = value;
        assert_eq!(
            status(&app, &cookie, "/api/tracks", "first", changed).await,
            StatusCode::CONFLICT
        );
    }
    assert!(!state.shared_codex_appserver.is_running());
    fixture.shutdown_process(&state).await;
    drop(app);
    drop(state);
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    configure(&mut fixture);
    let (state, app, cookie) = fixture.application().await;
    assert_eq!(
        post(&app, &cookie, "/api/tracks", "first", request.clone()).await["id"],
        created["id"]
    );
    // Authenticated HTTP wake paths retain the original carrier and saved prefix.
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "wake",
        json!({"text":"Continue on the retained ACP conversation"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (_, current, frame) = fixture.carrier(track).await;
            assert_eq!(current, worker);
            if !frame.canonical_inputs.attempts.is_empty()
                && frame.canonical_inputs.active.is_none()
                && frame.canonical_inputs.pending.is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(prompts(&fixture, &provider) > 0);
    let requests =
        std::fs::read_to_string(peer_root(&fixture, &provider).join("requests.jsonl")).unwrap();
    assert_eq!(
        requests
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|record| record["method"] == "session/prompt"
                && record["params"]["prompt"].to_string().contains(FIRST_BODY))
            .count(),
        1,
        "the saved first input reaches the real ACP prompt exactly once"
    );
    let identity: (Option<String>, Option<String>) =
        sqlx::query_as("SELECT thread_id,agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let original = fixture.receipt(request["area_id"].as_str().unwrap()).await;
    assert_eq!(original.acknowledgement, receipt.acknowledgement);
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop(app);
    drop(state);
    let before = prompts(&fixture, &provider);
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    assert_eq!(
        post(&app, &cookie, "/api/tracks", "first", request).await["id"],
        created["id"]
    );
    assert_eq!(
        prompts(&fixture, &provider),
        before,
        "restart/key replay never resends"
    );
    assert_eq!(fixture.carrier(track).await.1, worker);
    let after: (Option<String>, Option<String>) =
        sqlx::query_as("SELECT thread_id,agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        after, identity,
        "restart preserves local and native identities"
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_opencode_creation_empty_is_lazy() {
    empty_is_lazy(AgentProvider::OpenCode).await;
}

#[tokio::test]
async fn canonical_grok_creation_empty_is_lazy() {
    empty_is_lazy(AgentProvider::Grok).await;
}

async fn empty_is_lazy(provider: AgentProvider) {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    let (state, app, cookie) = fixture.application().await;
    let mut request = body(&app, &cookie, &provider).await;
    request.as_object_mut().unwrap().remove("first_message");
    let created = post(&app, &cookie, "/api/tracks", "empty", request.clone()).await;
    assert_eq!(
        post(&app, &cookie, "/api/tracks", "empty", request).await["id"],
        created["id"]
    );
    let (_, _, frame) = fixture.carrier(created["id"].as_str().unwrap()).await;
    assert!(frame.canonical_inputs.pending.is_empty());
    assert!(frame.header.last_thread_id.is_none());
    assert_eq!(prompts(&fixture, &provider), 0);
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_opencode_creation_catalog_uses_acp_owner() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    let (state, _, _) = fixture.application().await;
    let route = crate::state::RouteState::from_ref(&state);
    let advice = crate::planner_model::acquisition::CatalogAccess::new(
        &route.provider_availability,
        &route.claude_planner,
        &route.acp_planner,
        &state.shared_codex_appserver,
    )
    .canonical_mint(
        &calm_types::runtime::AgentProvider::OpenCode,
        Some("fixture/model-a"),
        Some("unsupported"),
    )
    .await
    .unwrap();
    assert!(
        advice.adjustment.is_some(),
        "ACP setup catalog judges efforts"
    );
    assert!(!state.shared_codex_appserver.is_running());
    assert_eq!(
        prompts(&fixture, &AgentProvider::OpenCode),
        0,
        "catalog discovery never prompts"
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_opencode_creation_catalog_does_not_reuse_another_registration() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    std::fs::write(fixture.root.path().join("grok/scenario"), "catalog_error").unwrap();
    let (state, _, _) = fixture.application().await;
    let route = crate::state::RouteState::from_ref(&state);
    let grok = route
        .acp_planner
        .discover_catalog(&calm_types::runtime::AgentProvider::Grok, None)
        .await;
    assert_eq!(
        grok.source,
        crate::planner_model::catalog::ModelSource::Unavailable
    );
    let advice = crate::planner_model::acquisition::CatalogAccess::new(
        &route.provider_availability,
        &route.claude_planner,
        &route.acp_planner,
        &state.shared_codex_appserver,
    )
    .canonical_mint(
        &calm_types::runtime::AgentProvider::OpenCode,
        Some("fixture/model-a"),
        Some("unsupported"),
    )
    .await
    .unwrap();
    assert!(!advice.catalog_unavailable);
    assert!(advice.adjustment.is_some());
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_opencode_creation_unknown_projects_keep_and_server_retry_reason() {
    unknown_projects_keep_and_server_retry_reason(AgentProvider::OpenCode).await;
}

#[tokio::test]
async fn canonical_grok_creation_unknown_projects_keep_and_server_retry_reason() {
    unknown_projects_keep_and_server_retry_reason(AgentProvider::Grok).await;
}

/// Delivery unknown holds the input: Keep is offered, retry is not, and nothing is resent.
async fn unknown_projects_keep_and_server_retry_reason(provider: AgentProvider) {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    std::fs::write(peer_root(&fixture, &provider).join("scenario"), "lost").unwrap();
    let (state, app, cookie) = fixture.application().await;
    let mut request = body(&app, &cookie, &provider).await;
    request["model"] = json!("fixture/model-a");
    request["reasoning_effort"] = json!("normal");
    let created = post(&app, &cookie, "/api/tracks", "first", request).await;
    let (card, worker, _) = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let facts = fixture.carrier(created["id"].as_str().unwrap()).await;
            if facts
                .2
                .canonical_inputs
                .attempts
                .first()
                .is_some_and(|a| matches!(a.delivery, InputDeliveryFact::Unknown))
            {
                break facts;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let run = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!(
                            "/api/cards/{card}/planner/run?contract=conversation_v1"
                        ))
                        .header(header::COOKIE, &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let run: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            if status == StatusCode::OK {
                break run;
            }
            // The read owner fences a concurrent runtime transition after assembly.
            assert_eq!(status, StatusCode::CONFLICT, "{run}");
            assert!(
                run["error"].as_str().is_some_and(
                    |reason| reason.contains("conversation runtime changed while reading")
                ),
                "{run}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let actions = &run["active_input"]["submissions"][0]["attempt"]["actions"];
    assert_eq!(actions["keep"]["kind"], "available");
    assert_eq!(
        actions["retry"],
        json!({"kind":"unavailable","reason":"unsupported_binding"})
    );
    assert_eq!(run["history_replacement_supported"], false);
    assert_eq!(prompts(&fixture, &provider), 1);
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_opencode_creation_historical_template_key_replays_through_http() {
    historical_template_key_replays_through_http(AgentProvider::OpenCode).await;
}

#[tokio::test]
async fn canonical_grok_creation_historical_template_key_replays_through_http() {
    historical_template_key_replays_through_http(AgentProvider::Grok).await;
}

async fn historical_template_key_replays_through_http(provider: AgentProvider) {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    let (state, app, cookie) = fixture.application().await;
    let mut original = body(&app, &cookie, &provider).await;
    original["template_id"] = json!("small-change");
    // Persist a released allocation through its existing owner. The public
    // System entry is canonical after G4; replay must retain the original key.
    if provider == AgentProvider::OpenCode {
        original["area_id"] =
            post(&app, &cookie, "/api/areas/system", "system", json!({})).await["id"].clone();
    }
    let cwd = fixture.root.path().join("attached");
    crate::test_support::init_fixture_git_repo(&cwd);
    original["cwd"] = json!(cwd);
    original["isolation"] = json!("none");
    original["attach_folder"] = json!(true);
    let created =
        crate::test_support::historical_track(&state, original.clone(), Some("historical")).await;
    let replay = post(&app, &cookie, "/api/tracks", "historical", original.clone()).await;
    assert_eq!(replay["id"], created["id"]);
    original.as_object_mut().unwrap().remove("template_id");
    assert_eq!(
        status(&app, &cookie, "/api/tracks", "historical", original).await,
        StatusCode::CONFLICT
    );
    // The historical first message still reaches the peer, on historical storage only.
    tokio::time::timeout(Duration::from_secs(30), async {
        while prompts(&fixture, &provider) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let card: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(created["id"].as_str().unwrap())
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let mut connection = fixture.repo.pool().acquire().await.unwrap();
    assert!(
        !crate::db::sqlite::conversation::card_has_canonical_storage(&mut connection, &card)
            .await
            .unwrap()
    );
    drop(connection);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_turns")
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap(),
        0
    );
    let worker: Option<String> = sqlx::query_scalar("SELECT session_id FROM cards WHERE id=?1")
        .bind(&card)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    if let Some(worker) = worker {
        state.harness.shutdown_and_remove(&worker).await.unwrap();
    }
    fixture.shutdown_process(&state).await;
}

/// Catalog GETs for dormant cards discover in each bound workspace, even when
/// another cwd has a fresh discovery-cache entry for the same registration.
#[tokio::test]
async fn canonical_opencode_creation_catalog_get_keeps_workspace_scope() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    let (state, app, cookie) = fixture.application().await;
    let mut request = body(&app, &cookie, &AgentProvider::OpenCode).await;
    request.as_object_mut().unwrap().remove("first_message");
    let mut workspaces = Vec::new();
    for key in ["scope-a", "scope-b"] {
        let created = post(&app, &cookie, "/api/tracks", key, request.clone()).await;
        let track = created["id"].as_str().unwrap();
        let (card, _, _) = fixture.carrier(track).await;
        let workspace = fixture
            .repo
            .track_get(track)
            .await
            .unwrap()
            .unwrap()
            .workspace
            .agent_cwd()
            .to_owned();
        // Dormant acceptance has not materialized its managed workspace. Supply
        // the directory so this test isolates discovery's cwd contract.
        std::fs::create_dir_all(&workspace).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/models?card_id={card}"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let catalog: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(catalog["source"], "live", "{catalog}");
        let requests = std::fs::read_to_string(fixture.root.path().join("requests.jsonl")).unwrap();
        assert!(
            requests
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .any(|call| call["method"] == "session/new" && call["params"]["cwd"] == workspace),
            "GET did not discover in {workspace}"
        );
        workspaces.push(workspace);
    }
    assert_ne!(workspaces[0], workspaces[1]);
    assert_eq!(prompts(&fixture, &AgentProvider::OpenCode), 0);
    assert!(!state.shared_codex_appserver.is_running());
    fixture.shutdown_process(&state).await;
}

mod acp_assistant;
mod g3_assistant;

mod u7_canonical;
