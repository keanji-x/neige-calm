//! Implicit methods and lazy isolation through canonical HTTP allocation and real worker preparation.
use super::*;
use crate::mcp_server::ToolCallIdentity;
use crate::model::CardRole;
use crate::operation::OperationRepo;
use calm_truth::db::RepoRead;

async fn attached(f: &Fixture) -> (AppState, axum::Router, String, Value) {
    let (state, app, cookie) = f.application().await;
    state.dispatcher.abort_event_listener_for_test();
    let area = u8_canonical::area(&app, &cookie, "implicit method").await;
    let path = u8b_canonical::repo(f);
    post(
        &app,
        &cookie,
        "/api/plugins/gitforge/enable",
        "enable-gitforge",
        json!({}),
    )
    .await;
    let mut body = u8_canonical::body(&area, "codex");
    body["cwd"] = json!(path);
    body["isolation"] = json!("none");
    body["attach_folder"] = json!(true);
    body["first_message"] = json!(FIRST_BODY);
    let created = post(&app, &cookie, "/api/tracks", "first", body).await;
    assert!(created["template_id"].is_null());
    assert!(created["workspace"]["worktree"].is_null());
    (state, app, cookie, created)
}

fn identity(created: &Value, card: &str, worker: &str) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: card.into(),
        role: CardRole::Planner,
        provider: calm_types::runtime::AgentProvider::Codex,
        session_id: worker.into(),
        track_id: Some(created["id"].as_str().unwrap().into()),
        area_id: created["area_id"].as_str().unwrap().into(),
        thread_id: "method-selection".into(),
    }
}

async fn report(f: &Fixture, track: &str) -> crate::track_report::TrackReportPayload {
    crate::track_report::resolve_report_for_track(f.repo.as_ref(), track)
        .await
        .unwrap()
        .2
}

#[tokio::test]
async fn track_set_canonical_none_git_grants_preserves_authored_report_and_carrier() {
    let f = Fixture::new().await;
    let (state, app, cookie, created) = attached(&f).await;
    let id = created["id"].as_str().unwrap();
    let (card, worker, frame) = f.carrier(id).await;
    let route = crate::state::RouteState::from_ref(&state);
    let track = f.repo.track_get(id).await.unwrap().unwrap();
    let scope = crate::mcp_server::tool_visibility::plugin_scope_for_track_row_with_grants(
        Some(f.repo.db()),
        &track,
        Some(&route.plugin),
    )
    .await;
    let manifest = plugin::builtin::get("gitforge").unwrap().manifest();
    assert!(
        scope.allows_manifest(manifest),
        "a Git locator grants its built-in before method selection"
    );
    let current = report(&f, id).await;
    let response = app.clone().oneshot(Request::post(format!("/api/tracks/{id}/report"))
        .header(header::COOKIE, &cookie).header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"summary":"Authored summary", "body":"# Investigation\n\nKeep this evidence.\n", "ifDocRev":current.doc_rev}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let authored = report(&f, id).await;
    let registry = crate::mcp_server::build_default_registry();
    let tool = registry.lookup("neige_track_set").unwrap();
    for template in ["small-change", "long-term-goal", "small-change"] {
        let out = tool(
            state.mcp_context(),
            identity(&created, &card, &worker),
            json!({"template_id":template,"message":"Use this working method"}),
        )
        .await
        .unwrap()
        .into_structured();
        assert_eq!(out["ok"], true);
        assert_eq!(out["template_id"], template);
        assert_eq!(
            out["plugin_scope"],
            if template == "long-term-goal" {
                json!("gitforge")
            } else {
                Value::Null
            }
        );
        assert!(
            out["instructions"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        );
        let after = report(&f, id).await;
        assert_eq!(after.summary, authored.summary);
        let blocks = after.blocks.unwrap();
        assert!(calm_types::report_contract::is_pure_comment_block(
            &calm_types::report_blocks::flat_text(&blocks[0])
        ));
        for original in authored.blocks.as_ref().unwrap() {
            assert!(
                blocks.contains(original),
                "authored block identity, revision and content survive"
            );
        }
        assert_eq!(
            f.carrier(id).await,
            (card.clone(), worker.clone(), frame.clone())
        );
        let selected = f.repo.card_get(&card).await.unwrap().unwrap();
        assert!(
            selected
                .payload
                .get(crate::validation::PLANNER_TEMPLATE_CONTEXT_PAYLOAD_KEY)
                .is_some()
        );
    }
    let updated = f.repo.track_get(id).await.unwrap().unwrap();
    let objectives: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM track_capabilities WHERE track_id=?1 AND capability='objectives')")
        .bind(id).fetch_one(f.repo.pool()).await.unwrap();
    assert!(objectives);
    let grants =
        crate::track_plugin_access::current_grants(Some(f.repo.db()), &updated, &route.plugin)
            .await
            .unwrap();
    assert!(grants.contains("gitforge"));
    let wakes: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE scope_track=?1 AND kind='track.wake_requested' AND json_extract(payload,'$.source')='track_configuration'")
        .bind(id).fetch_one(f.repo.pool()).await.unwrap();
    assert_eq!(wakes, 0);
    let mut unauthorized = identity(&created, &card, &worker);
    unauthorized.role = CardRole::Assistant;
    assert!(
        tool(
            state.mcp_context(),
            unauthorized,
            json!({"template_id":"small-change"})
        )
        .await
        .is_err()
    );
    f.shutdown_process(&state).await;
}

async fn dev_with_input(f: &Fixture) -> (AppState, axum::Router, String, Value) {
    let (state, app, cookie, implicit) = attached(f).await;
    let mut body = u8_canonical::body(&json!({"id":implicit["area_id"]}), "codex");
    body["cwd"] = implicit["workspace"]["path"].clone();
    body["isolation"] = json!("none");
    body["attach_folder"] = json!(true);
    body["template_id"] = json!("dev");
    body["template_input"] = json!({"merge_policy":"ask"});
    let created = post(&app, &cookie, "/api/tracks", "dev-with-input", body).await;
    assert_eq!(created["template_input"], json!({"merge_policy":"ask"}));
    (state, app, cookie, created)
}

#[tokio::test]
async fn rest_dev_to_small_change_clears_previous_template_input() {
    let f = Fixture::new().await;
    let (state, app, cookie, created) = dev_with_input(&f).await;
    let id = created["id"].as_str().unwrap();
    let original = f.carrier(id).await;
    let response = app
        .oneshot(
            Request::patch(format!("/api/tracks/{id}"))
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"template_id":"small-change"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let updated: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["template_id"], "small-change");
    assert!(updated["template_input"].is_null());
    assert!(updated["plugin_scope"].is_null());
    assert!(
        f.repo
            .track_get(id)
            .await
            .unwrap()
            .unwrap()
            .template_input
            .is_none()
    );
    assert_eq!(f.carrier(id).await, original);
    f.shutdown_process(&state).await;
}

#[tokio::test]
async fn mcp_dev_to_small_change_clears_previous_template_input() {
    let f = Fixture::new().await;
    let (state, _app, _cookie, created) = dev_with_input(&f).await;
    let id = created["id"].as_str().unwrap();
    let original = f.carrier(id).await;
    let registry = crate::mcp_server::build_default_registry();
    let tool = registry.lookup("neige_track_set").unwrap();
    let kept = tool(
        state.mcp_context(),
        identity(&created, &original.0, &original.1),
        json!({"template_id":"long-term-goal"}),
    )
    .await
    .unwrap()
    .into_structured();
    assert_eq!(kept["template_id"], "long-term-goal");
    assert_eq!(
        f.repo.track_get(id).await.unwrap().unwrap().template_input,
        Some(json!({"merge_policy":"ask"}))
    );
    let out = tool(
        state.mcp_context(),
        identity(&created, &original.0, &original.1),
        json!({"template_id":"small-change"}),
    )
    .await
    .unwrap()
    .into_structured();
    assert_eq!(out["template_id"], "small-change");
    assert!(out["plugin_scope"].is_null());
    assert!(
        !out["instructions"]
            .as_str()
            .unwrap()
            .contains("merge_policy")
    );
    let updated = f.repo.track_get(id).await.unwrap().unwrap();
    assert_eq!(updated.template_id.as_deref(), Some("small-change"));
    assert!(updated.template_input.is_none());
    assert_eq!(f.carrier(id).await, original);
    let card = f.repo.card_get(&original.0).await.unwrap().unwrap();
    assert!(!card.payload.to_string().contains("merge_policy"));
    f.shutdown_process(&state).await;
}

#[tokio::test]
async fn lazy_worktree_first_worker_preparation_keeps_canonical_planner_carrier() {
    let f = Fixture::new().await;
    let (state, _app, _cookie, created) = attached(&f).await;
    let id = created["id"].as_str().unwrap();
    let original = f.carrier(id).await;
    let thread: Option<String> =
        sqlx::query_scalar("SELECT thread_id FROM worker_sessions WHERE id=?1")
            .bind(&original.1)
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    assert!(thread.is_none());
    prepare_worker(&f, &state, id).await;
    let upgraded = f.repo.track_get(id).await.unwrap().unwrap();
    assert_eq!(
        upgraded.workspace.path,
        created["workspace"]["path"].as_str().unwrap()
    );
    assert!(
        upgraded
            .workspace
            .worktree
            .as_ref()
            .is_some_and(|path| std::path::Path::new(path).is_dir())
    );
    assert_eq!(f.carrier(id).await, original);
    assert!(
        f.repo
            .card_get(&original.0)
            .await
            .unwrap()
            .unwrap()
            .payload
            .get("bound_session_cwd")
            .is_none()
    );
    f.shutdown_process(&state).await;
}

async fn worker_operation(f: &Fixture, id: &str) -> String {
    let task = format!("{id}:first-worker");
    f.repo.db().writer().execute(sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,status,created_at_ms,updated_at_ms) VALUES(?1,?2,'first-worker','codex','Inspect source','{}','[]','dispatched',1,1)")
        .bind(&task).bind(id)).await.unwrap();
    let operations = crate::operation::SqlxOperationRepo::new(f.repo.db().clone());
    let payload = json!({"actor":crate::ids::ActorId::KernelDispatcher,"track_id":id,"idempotency_key":task,"goal":"Inspect source","context":{}});
    operations
        .insert_operation(
            "codex-worker",
            crate::operation::OperationKey {
                operation_key: crate::model::new_id(),
                idempotency_key: Some(task),
                payload_hash: "first-worker".into(),
            },
            payload,
        )
        .await
        .unwrap()
}

async fn prepare_worker(f: &Fixture, state: &AppState, id: &str) {
    let operation = worker_operation(f, id).await;
    let operations = crate::operation::SqlxOperationRepo::new(f.repo.db().clone());
    tokio::time::timeout(
        Duration::from_secs(30),
        state
            .operation_runtime
            .prepare_once_for_test(&operation, false),
    )
    .await
    .expect("worker preparation must use the driver's already held Track fence")
    .unwrap();
    assert!(matches!(
        operations
            .get_operation(&operation)
            .await
            .unwrap()
            .unwrap()
            .phase,
        crate::operation::Phase::TxCommitted
    ));
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn lazy_worktree_cancellation_retains_track_fence_until_blocking_git_finishes() {
    use crate::per_card_lock::{lock_key, try_lock_key};
    use crate::test_seams::{PausePoint, install_pause_for_test};

    let f = Fixture::new().await;
    let (state, _app, _cookie, created) = attached(&f).await;
    let id = created["id"].as_str().unwrap();
    let path = created["workspace"]["path"].as_str().unwrap();
    let path = std::fs::canonicalize(path).unwrap();
    let pause = PausePoint::default();
    install_pause_for_test(
        "track-metadata-before",
        path.to_str().unwrap(),
        pause.clone(),
    );
    let operation = worker_operation(&f, id).await;
    let runtime = state.operation_runtime.clone();
    let handle =
        tokio::spawn(async move { runtime.prepare_once_for_test(&operation, false).await });
    tokio::time::timeout(Duration::from_secs(30), pause.entered.notified())
        .await
        .expect("blocking Git reaches Track metadata");
    handle.abort();
    assert!(handle.await.unwrap_err().is_cancelled());
    let route = crate::state::RouteState::from_ref(&state);
    let fenced = try_lock_key(&route.track_delete_locks, id).is_none();
    pause.release.notify_waiters();
    let guard = tokio::time::timeout(
        Duration::from_secs(30),
        lock_key(&route.track_delete_locks, id),
    )
    .await
    .expect("Track fence releases after blocking Git finishes");
    drop(guard);
    f.shutdown_process(&state).await;
    assert!(
        fenced,
        "cancelled preparation must retain the Track fence during blocking Git"
    );
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn lazy_worktree_claude_bound_session_cwd_survives_cold_recovery() {
    preserve_bound_cwd(calm_types::runtime::AgentProvider::Claude, false).await;
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn lazy_worktree_acp_bound_session_cwd_survives_cold_recovery() {
    preserve_bound_cwd(calm_types::runtime::AgentProvider::OpenCode, false).await;
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn lazy_worktree_claude_prechange_cold_recovery() {
    preserve_bound_cwd(calm_types::runtime::AgentProvider::Claude, true).await;
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn lazy_worktree_acp_prechange_cold_recovery() {
    preserve_bound_cwd(calm_types::runtime::AgentProvider::OpenCode, true).await;
}

#[cfg(feature = "fixtures")]
async fn preserve_bound_cwd(provider: calm_types::runtime::AgentProvider, lazy: bool) {
    let mut f = Fixture::new().await;
    let fake = f.root.path().join("claude");
    if provider == calm_types::runtime::AgentProvider::Claude {
        std::fs::create_dir_all(&fake).unwrap();
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/claude_planner_fake/claude.sh"
            ),
            fake.join("claude"),
        )
        .unwrap();
        std::fs::write(fake.join("scenario"), "exit").unwrap();
        let config = f.root.path().join("claude.json");
        std::fs::write(&config, json!({"claude_binary":fake.join("claude"), "claude_version":"2.1.280", "config_dir":f.root.path().join("claude-config")}).to_string()).unwrap();
        f.claude_config = Some(config);
        f.execution = Some(crate::test_support::execution::Supervisor::new());
    } else {
        opencode_creation_tests::configure(&mut f);
    }
    let (state, app, cookie) = f.application().await;
    let area = u8_canonical::area(&app, &cookie, "bound cwd").await;
    let path = u8b_canonical::repo(&f);
    let mut body = u8_canonical::body(&area, provider.wire_name());
    body["cwd"] = json!(path);
    body["isolation"] = json!("none");
    body["attach_folder"] = json!(true);
    body["first_message"] = json!(FIRST_BODY);
    let created = post(&app, &cookie, "/api/tracks", "first", body).await;
    let track = created["id"].as_str().unwrap();
    let (card, worker, _) = f.carrier(track).await;
    completed_inputs(&f, track, 1).await;
    let payload = f.repo.card_get(&card).await.unwrap().unwrap().payload;
    assert_eq!(
        payload["bound_session_cwd"],
        json!({"worker_session_id":worker,"cwd":path})
    );
    f.repo
        .db()
        .writer()
        .execute(
            sqlx::query(
                "UPDATE cards SET payload=json_remove(payload,'$.bound_session_cwd') WHERE id=?1",
            )
            .bind(&card),
        )
        .await
        .unwrap();
    assert!(
        f.repo
            .card_get(&card)
            .await
            .unwrap()
            .unwrap()
            .payload
            .get("bound_session_cwd")
            .is_none()
    );
    if lazy {
        state.dispatcher.abort_event_listener_for_test();
        prepare_worker(&f, &state, track).await;
    } else {
        let response = app.clone().oneshot(Request::patch(format!("/api/tracks/{track}"))
        .header(header::COOKIE, &cookie).header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"workspace":{"kind":"attached","path":path,"isolation":"worktree","attach_folder":true}}).to_string())).unwrap()).await.unwrap();
        let status = response.status();
        let upgraded: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(status, StatusCode::OK, "{upgraded}");
        assert!(upgraded["workspace"]["worktree"].is_string());
    }
    assert_eq!(
        f.repo.card_get(&card).await.unwrap().unwrap().payload["bound_session_cwd"],
        json!({"worker_session_id":worker,"cwd":path})
    );
    assert_eq!(f.carrier(track).await.1, worker);
    f.shutdown_process(&state).await;
    drop((state, app));
    f.repo = Arc::new(SqlxRepo::open(&f.url).await.unwrap());
    let (state, app, cookie) = f.application().await;
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "second",
        json!({"text":"Continue in the original directory"}),
    )
    .await;
    completed_inputs(&f, track, 2).await;
    assert_eq!(f.carrier(track).await.1, worker);
    if provider == calm_types::runtime::AgentProvider::Claude {
        assert_eq!(
            std::fs::read_to_string(fake.join("pwd")).unwrap().trim(),
            path.to_str().unwrap()
        );
    } else {
        let calls = std::fs::read_to_string(f.root.path().join("requests.jsonl")).unwrap();
        let resumed: Vec<Value> = calls
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|call| {
                matches!(
                    call["method"].as_str(),
                    Some("session/load" | "session/resume")
                )
            })
            .collect();
        assert!(
            !resumed.is_empty(),
            "cold recovery must reopen the retained native session"
        );
        assert!(
            resumed
                .iter()
                .all(|call| call["params"]["cwd"] == json!(path))
        );
    }
    f.shutdown_process(&state).await;
}

#[cfg(feature = "fixtures")]
async fn completed_inputs(f: &Fixture, track: &str, count: usize) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (_, _, frame) = f.carrier(track).await;
            if frame.canonical_inputs.attempts.len() >= count
                && frame.canonical_inputs.active.is_none()
                && frame.canonical_inputs.pending.is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("retained provider completes its actual accepted inputs");
}

#[tokio::test]
async fn codex_selection_read_keeps_bound_cwd_after_lazy_upgrade() {
    let f = Fixture::new().await;
    let (state, app, cookie) = f.application().await;
    let area = u8_canonical::area(&app, &cookie, "config scope").await;
    let path = u8b_canonical::repo(&f);
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let mut body = u8_canonical::body(&area, "codex");
    body["cwd"] = json!(path);
    body["isolation"] = json!("none");
    body["attach_folder"] = json!(true);
    let created = post(&app, &cookie, "/api/tracks", "first", body).await;
    let track = created["id"].as_str().unwrap();
    let original = f.carrier(track).await;
    let card = f.repo.card_get(&original.0).await.unwrap().unwrap();
    let mut payload = card.payload;
    let fields = payload.as_object_mut().unwrap();
    crate::planner_model::CardModelSelection::apply_to_payload(
        fields,
        Some("chosen-model"),
        Some("medium"),
    );
    crate::planner_model::CardModelSelection::apply_to_payload(fields, None, None);
    let card_id = original.0.clone();
    f.repo
        .db()
        .writer()
        .with_writer(|writer| {
            Box::pin(async move {
                let mut tx = writer.begin_immediate().await.unwrap();
                crate::db::sqlite::card_update_tx(
                    &mut tx,
                    &card_id,
                    crate::model::CardPatch {
                        title: None,
                        kind: None,
                        sort: None,
                        payload: Some(payload),
                        deletable: None,
                    },
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
    post(
        &app,
        &cookie,
        &format!("/api/cards/{}/planner/input", original.0),
        "first-input",
        json!({"text":FIRST_BODY}),
    )
    .await;
    for (index, turn) in ["before-upgrade", "after-upgrade"].into_iter().enumerate() {
        if index == 1 {
            prepare_worker(&f, &state, track).await;
            let upgraded = f.repo.track_get(track).await.unwrap().unwrap();
            assert_ne!(upgraded.workspace.agent_cwd(), path.to_str().unwrap());
            post(
                &app,
                &cookie,
                &format!("/api/cards/{}/planner/input", original.0),
                "second",
                json!({"text":"keep config scope"}),
            )
            .await;
        }
        let start = loop {
            let call = request(&mut peer).await;
            let result = match call["method"].as_str().unwrap() {
                "config/read" => {
                    assert_eq!(call["params"]["cwd"], json!(path));
                    json!({"config":{"model":"original-model","model_reasoning_effort":"high"}})
                }
                "thread/start" => {
                    assert_eq!(index, 0);
                    assert_eq!(call["params"]["cwd"], json!(path));
                    json!({"thread":{"id":THREAD}})
                }
                "turn/start" => break call,
                method => panic!("unexpected RPC: {method}"),
            };
            send(&mut peer, json!({"id":call["id"],"result":result})).await;
        };
        assert_eq!(start["params"]["model"], "original-model");
        assert_eq!(start["params"]["effort"], "high");
        send(
            &mut peer,
            json!({"id":start["id"],"result":{"turn":{"id":turn}}}),
        )
        .await;
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/started",
            json!({"id":turn,"status":"inProgress"}),
        )
        .await;
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/completed",
            json!({"id":turn,"status":"completed"}),
        )
        .await;
        completed_inputs(&f, track, index + 1).await;
        let current = f.carrier(track).await;
        assert_eq!((&current.0, &current.1), (&original.0, &original.1));
    }
    f.shutdown_process(&state).await;
}
