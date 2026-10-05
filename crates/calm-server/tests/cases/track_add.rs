//! `neige_track_add` (#2104 K1): a Planner opens an ordinary top-level Track from a stored recipe
//! through the keyed create, and the new row records which Track created it under which key.
//! Every Track here is managed, so no test touches a real repository.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_insert_tx};
use calm_server::error::CalmError;
use calm_server::event::EventBus;
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::track_add::TOOL_TRACK_ADD;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewArea, NewCard};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    ctx: Arc<AppContext>,
    registry: ToolRegistry,
    repo: Arc<SqlxRepo>,
    area_id: String,
    recipe_id: String,
    _tmp: TempDir,
}

/// A Planner (or Worker) bound to one card of one Track, with a live session the role gate resolves.
#[derive(Clone)]
struct Caller {
    identity: ToolCallIdentity,
}

async fn boot(max_open: u32) -> Boot {
    let tmp = TempDir::new().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let area = repo
        .area_create(NewArea {
            name: "track-add".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let roles = CardRoleCache::new();
    let tracks = TrackAreaCache::new();
    repo.seed_track_area_cache(&tracks).await.unwrap();
    let mut state = AppState::from_parts(
        repo_dyn.clone(),
        events.clone(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(roles.clone(), tracks.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(tracks),
    )
    .with_workspace_root(tmp.path().join("workspaces"))
    .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
        repo_dyn, None,
    ));
    // After the last builder: the creator keeps the route state it is given.
    state.bind_track_creator(max_open);
    let ctx = state.mcp_context();
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    let (status, recipe) = send(
        app.clone(),
        "POST",
        "/api/track-recipes",
        Some(json!({ "title": "Research", "body": "# Research\n\nFollow one instrument.\n" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{recipe}");
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    Boot {
        app,
        ctx,
        registry,
        repo,
        area_id: area.id.to_string(),
        recipe_id: recipe["id"].as_str().unwrap().to_string(),
        _tmp: tmp,
    }
}

async fn send(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn session(id: &str, track_id: &str, card_id: &str, contract: WorkerContract) -> WorkerSession {
    WorkerSession {
        id: WorkerSessionId::from(id),
        track_id: track_id.to_string().into(),
        provider: WorkerProviderKind::Codex,
        mode: SessionMode::Resumable,
        contract,
        parent_session_id: None,
        requester_session_id: None,
        state: WorkerSessionState::Starting,
        mcp_token_hash: None,
        thread_id: None,
        agent_session_id: None,
        active_turn_id: None,
        terminal_run_id: None,
        card_id: Some(card_id.to_string().into()),
        handle_state_json: None,
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        last_activity_ms: None,
        last_thread_status: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    }
}

impl Boot {
    async fn scalar<T>(&self, sql: &str, bind: &str) -> T
    where
        T: Send + Unpin + for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
    {
        sqlx::query_scalar(sql)
            .bind(bind)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    /// A blank Track made through `POST /api/tracks`, and its Planner as a caller.
    async fn user_track(&self, title: &str) -> (String, Caller) {
        let (status, track) = send(
            self.app.clone(),
            "POST",
            "/api/tracks",
            Some(json!({
                "planner_provider": "codex",
                "area_id": self.area_id,
                "title": title,
                "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{track}");
        let track_id = track["id"].as_str().unwrap().to_string();
        let caller = self.planner_of(&track_id).await;
        (track_id, caller)
    }

    /// The Planner of `track_id`, on its live session (the harness start's, else a seeded one).
    async fn planner_of(&self, track_id: &str) -> Caller {
        let card_id: String = self
            .scalar(
                "SELECT id FROM cards WHERE track_id = ?1 AND role = 'planner'",
                track_id,
            )
            .await;
        let live: Option<String> = sqlx::query_scalar(
            "SELECT id FROM worker_sessions WHERE card_id = ?1 \
             AND state IN ('starting','running','idle','turn_pending')",
        )
        .bind(&card_id)
        .fetch_optional(self.repo.pool())
        .await
        .unwrap();
        let session_id = match live {
            Some(id) => id,
            None => {
                let id = format!("planner-session-{card_id}");
                self.seed_session(session(&id, track_id, &card_id, WorkerContract::Planner))
                    .await;
                id
            }
        };
        self.caller(track_id, &card_id, CardRole::Planner, &session_id)
    }

    /// A Worker card on `track_id` with its own live session.
    async fn worker_on(&self, track_id: &str) -> Caller {
        let card = self
            .repo
            .card_create(NewCard {
                track_id: track_id.to_string().into(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: Value::Null,
            })
            .await
            .unwrap();
        crate::support::mcp::set_persisted_card_role(
            self.repo.as_ref(),
            card.id.as_str(),
            CardRole::Worker,
        )
        .await;
        let session_id = format!("worker-session-{}", card.id);
        self.seed_session(session(
            &session_id,
            track_id,
            card.id.as_str(),
            WorkerContract::Executor,
        ))
        .await;
        self.caller(track_id, card.id.as_str(), CardRole::Worker, &session_id)
    }

    fn caller(&self, track_id: &str, card_id: &str, role: CardRole, session_id: &str) -> Caller {
        Caller {
            identity: ToolCallIdentity {
                card_id: card_id.to_string(),
                role,
                provider: AgentProvider::Codex,
                session_id: session_id.to_string(),
                track_id: Some(track_id.to_string()),
                area_id: self.area_id.clone(),
                thread_id: format!("thread-{card_id}"),
            },
        }
    }

    async fn seed_session(&self, session: WorkerSession) {
        calm_server::db::write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                session_insert_tx(tx, session)
                    .await
                    .map_err(CalmError::from)?;
                Ok(())
            })
        })
        .await
        .expect("seed session");
    }

    fn args(&self, key: &str) -> Value {
        json!({
            "recipe_id": self.recipe_id,
            "title": format!("{key} research"),
            "idempotency_key": key,
            "text": format!("Start researching for {key}."),
            "message": format!("cover {key}"),
        })
    }

    async fn add(&self, caller: &Caller, args: Value) -> Result<Value, RpcError> {
        let handler = self
            .registry
            .lookup(TOOL_TRACK_ADD)
            .unwrap_or_else(|| panic!("tool not registered: {TOOL_TRACK_ADD}"));
        handler(self.ctx.clone(), caller.identity.clone(), args)
            .await
            .map(calm_server::mcp_server::result::ToolResult::into_structured)
    }

    async fn added(&self, caller: &Caller, key: &str) -> String {
        let out = self
            .add(caller, self.args(key))
            .await
            .unwrap_or_else(|e| panic!("add {key}: {e:?}"));
        out["track_id"].as_str().unwrap().to_string()
    }

    async fn track_count(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM tracks").await
    }

    /// `harness.user_message.enqueued` rows for `track_id`: one per delivery of a first message.
    async fn deliveries(&self, track_id: &str) -> i64 {
        self.scalar(
            "SELECT COUNT(*) FROM events WHERE kind = 'harness.user_message.enqueued' \
             AND scope_track = ?1",
            track_id,
        )
        .await
    }
}

fn assert_forbidden(result: Result<Value, RpcError>, why: &str) {
    let error = result.expect_err(why);
    assert_eq!(error.code, -32403, "{why}: {error:?}");
}

#[tokio::test]
async fn track_add_records_provenance_not_parent() {
    let boot = boot(16).await;
    let (creator, planner) = boot.user_track("portfolio").await;
    let out = boot
        .add(&planner, boot.args("invest-US-SPY-1"))
        .await
        .expect("a Planner adds a Track");
    let track_id = out["track_id"].as_str().unwrap().to_string();

    // `repo.track_get` reads `TRACK_SELECT_COLUMNS`; the detail route reads the `w.` list.
    let track = boot.repo.track_get(&track_id).await.unwrap().unwrap();
    assert_eq!(out["created_at"], json!(track.created_at));
    assert_eq!(track.creator_track_id.as_deref(), Some(creator.as_str()));
    assert_eq!(track.creator_key.as_deref(), Some("invest-US-SPY-1"));
    assert_eq!(track.recipe_id.as_deref(), Some(boot.recipe_id.as_str()));
    assert_eq!(track.title, "invest-US-SPY-1 research");
    assert_eq!(track.area_id.as_str(), boot.area_id);
    assert_eq!(track.plugin_scope, None);
    let (status, detail) = send(
        boot.app.clone(),
        "GET",
        &format!("/api/tracks/{track_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["track"]["creator_track_id"], json!(creator));
    assert_eq!(detail["track"]["creator_key"], json!("invest-US-SPY-1"));

    // A top-level Track: no parent edge, no tree budget.
    let (parent, budget): (Option<String>, Option<i64>) =
        sqlx::query_as("SELECT parent_track_id, tree_task_budget FROM tracks WHERE id = ?1")
            .bind(&track_id)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
    assert_eq!((parent, budget), (None, None));

    // The creation `TrackUpdated` is the Planner session's, and carries the audit note.
    let (actor, payload): (String, String) = sqlx::query_as(
        "SELECT actor, payload FROM events WHERE kind = 'track.updated' AND scope_track = ?1 \
         ORDER BY id LIMIT 1",
    )
    .bind(&track_id)
    .fetch_one(boot.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&actor).unwrap(),
        json!({ "kind": "AiPlannerSession", "id": planner.identity.session_id })
    );
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["agent_message"], json!("cover invest-US-SPY-1"));
    assert_eq!(payload["creator_key"], json!("invest-US-SPY-1"));
}

#[tokio::test]
async fn track_add_refuses_past_open_cap() {
    let boot = boot(2).await;
    let (_, planner) = boot.user_track("portfolio").await;
    boot.added(&planner, "k1").await;
    boot.added(&planner, "k2").await;
    let before = boot.track_count().await;
    let error = boot
        .add(&planner, boot.args("k3"))
        .await
        .expect_err("a third open added Track is over the cap of 2");
    assert_eq!(error.code, -32409, "{error:?}");
    assert_eq!(
        boot.track_count().await,
        before,
        "a refused add creates nothing"
    );
    let open: i64 = boot
        .count(
            "SELECT COUNT(*) FROM tracks WHERE creator_track_id IS NOT NULL AND closed_at IS NULL",
        )
        .await;
    assert_eq!(open, 2, "nothing was closed to make room");
}

#[tokio::test]
async fn track_add_counts_only_open_tracks() {
    let boot = boot(2).await;
    let (_, planner) = boot.user_track("portfolio").await;
    let first = boot.added(&planner, "k1").await;
    boot.added(&planner, "k2").await;
    let (status, closed) = send(
        boot.app.clone(),
        "PATCH",
        &format!("/api/tracks/{first}"),
        Some(json!({ "closed": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    boot.add(&planner, boot.args("k3"))
        .await
        .expect("a closed added Track frees its slot");
}

#[tokio::test]
async fn track_add_cap_refusal_lists_open_tracks() {
    let boot = boot(2).await;
    let (creator, planner) = boot.user_track("portfolio").await;
    let first = boot.added(&planner, "k1").await;
    let second = boot.added(&planner, "k2").await;
    let error = boot
        .add(&planner, boot.args("k3"))
        .await
        .expect_err("over the cap");
    assert_eq!(error.code, -32409, "{error:?}");
    let data = error.data.clone().expect("the cap refusal carries data");
    assert_eq!(
        data["open"],
        json!([
            { "track_id": first, "creator_key": "k1" },
            { "track_id": second, "creator_key": "k2" },
        ]),
        "{error:?}"
    );
    for needle in [
        TOOL_TRACK_ADD,
        "--track-add-max-open",
        "cap of 2",
        "2 open",
        creator.as_str(),
        first.as_str(),
        second.as_str(),
    ] {
        assert!(error.message.contains(needle), "{needle}: {error:?}");
    }
}

#[tokio::test]
async fn track_add_refuses_worker() {
    let boot = boot(16).await;
    let (creator, _) = boot.user_track("portfolio").await;
    let worker = boot.worker_on(&creator).await;
    let before = boot.track_count().await;
    assert_forbidden(
        boot.add(&worker, boot.args("fresh")).await,
        "a Worker may not add a Track",
    );
    assert_eq!(boot.track_count().await, before);
    assert_eq!(
        boot.count("SELECT COUNT(*) FROM track_create_idempotency")
            .await,
        0
    );
}

#[tokio::test]
async fn track_add_refuses_worker_replay() {
    let boot = boot(16).await;
    let (creator, planner) = boot.user_track("portfolio").await;
    let added = boot.added(&planner, "k1").await;
    assert_eq!(boot.deliveries(&added).await, 1);
    let operations = boot.count("SELECT COUNT(*) FROM operations").await;
    let worker = boot.worker_on(&creator).await;
    let before = boot.track_count().await;
    assert_forbidden(
        boot.add(&worker, boot.args("k1")).await,
        "a Worker may not replay a Planner's add",
    );
    assert_eq!(boot.track_count().await, before);
    assert_eq!(boot.deliveries(&added).await, 1, "nothing was redelivered");
    assert_eq!(
        boot.count("SELECT COUNT(*) FROM operations").await,
        operations
    );
}

#[tokio::test]
async fn track_add_refuses_bound_creator() {
    let boot = boot(16).await;
    let (creator, planner) = boot.user_track("portfolio").await;
    // The recorded owner is not running, so the scope is the fail-closed one; a running owner
    // would make it `Only`. Neither is `All`.
    sqlx::query("UPDATE tracks SET plugin_scope = 'dev.neige.owner' WHERE id = ?1")
        .bind(&creator)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let before = boot.track_count().await;
    assert_forbidden(
        boot.add(&planner, boot.args("k1")).await,
        "a bound creator may not add a Track",
    );
    assert_eq!(boot.track_count().await, before);
}

#[tokio::test]
async fn track_add_refuses_created_creator() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("portfolio").await;
    let added = boot.added(&planner, "k1").await;
    let added_planner = boot.planner_of(&added).await;
    let before = boot.track_count().await;
    assert_forbidden(
        boot.add(&added_planner, boot.args("k2")).await,
        "an added Track may not add a Track",
    );
    assert_eq!(boot.track_count().await, before);
}

#[tokio::test]
async fn track_add_refuses_child_creator() {
    let boot = boot(16).await;
    let (parent, _) = boot.user_track("parent").await;
    let (creator, planner) = boot.user_track("child").await;
    sqlx::query("UPDATE tracks SET parent_track_id = ?1 WHERE id = ?2")
        .bind(&parent)
        .bind(&creator)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let before = boot.track_count().await;
    assert_forbidden(
        boot.add(&planner, boot.args("k1")).await,
        "a child Track may not add a Track",
    );
    assert_eq!(boot.track_count().await, before);
}

#[tokio::test]
async fn track_add_replays_and_refuses_changed_request() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("portfolio").await;
    let first = boot.add(&planner, boot.args("k1")).await.expect("mint");
    let count = boot.track_count().await;
    let again = boot.add(&planner, boot.args("k1")).await.expect("replay");
    assert_eq!(
        again, first,
        "a byte-identical retry returns the same result"
    );
    assert_eq!(boot.track_count().await, count, "a replay mints nothing");
    for (field, value) in [
        ("text", json!("Something else.")),
        ("title", json!("Another title")),
        ("message", json!("another note")),
        ("recipe_id", json!("another-recipe")),
    ] {
        let mut changed = boot.args("k1");
        changed[field] = value;
        let error = boot
            .add(&planner, changed)
            .await
            .expect_err("a changed request under a used key");
        assert_eq!(error.code, -32409, "{field}: {error:?}");
    }
    assert_eq!(boot.track_count().await, count);
}

#[tokio::test]
async fn track_add_delivers_text_once() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("portfolio").await;
    let added = boot.added(&planner, "k1").await;
    let text = "Start researching for k1.";
    let carrying = || async {
        let payloads: Vec<String> = sqlx::query_scalar(
            "SELECT payload_json FROM operations WHERE kind = 'planner-harness-start'",
        )
        .fetch_all(boot.repo.pool())
        .await
        .unwrap();
        payloads
            .iter()
            .filter(|p| serde_json::from_str::<Value>(p).unwrap()["first_message"] == json!(text))
            .count()
    };
    assert_eq!(boot.deliveries(&added).await, 1);
    assert_eq!(carrying().await, 1);
    boot.added(&planner, "k1").await;
    assert_eq!(
        boot.deliveries(&added).await,
        1,
        "a replay delivers nothing"
    );
    assert_eq!(carrying().await, 1);
}

/// Written straight at the database: no writer can produce half a creator provenance.
#[tokio::test]
async fn the_database_refuses_half_a_creator_provenance() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("portfolio").await;
    let added = boot.added(&planner, "k1").await;
    const REFUSED_BY: &str = "CHECK constraint failed: track_creator_is_whole";
    for sql in [
        "UPDATE tracks SET creator_key = NULL WHERE id = ?1",
        "UPDATE tracks SET creator_track_id = NULL WHERE id = ?1",
    ] {
        let error = sqlx::query(sql)
            .bind(&added)
            .execute(boot.repo.pool())
            .await
            .expect_err(sql);
        assert!(error.to_string().contains(REFUSED_BY), "{sql}: {error}");
    }
    sqlx::query("UPDATE tracks SET creator_track_id = NULL, creator_key = NULL WHERE id = ?1")
        .bind(&added)
        .execute(boot.repo.pool())
        .await
        .expect("clearing both at once is allowed");
    let indexed: i64 = boot
        .count(
            "SELECT COUNT(*) FROM pragma_index_list('tracks') \
             WHERE name = 'idx_tracks_creator_track_id'",
        )
        .await;
    assert_eq!(indexed, 1, "the cap's count is indexed");
}

/// `POST /api/tracks` cannot mint into, or replay out of, the tool's key namespace.
#[tokio::test]
async fn rest_create_refuses_the_track_add_key_namespace() {
    let boot = boot(16).await;
    let (creator, planner) = boot.user_track("portfolio").await;
    boot.added(&planner, "k1").await;
    let before = boot.track_count().await;
    for first_message in [None, Some("hello")] {
        let mut body = json!({
            "planner_provider": "codex",
            "area_id": boot.area_id,
            "title": "",
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        });
        if let Some(text) = first_message {
            body["first_message"] = json!(text);
        }
        let response = boot
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tracks")
                    .header("content-type", "application/json")
                    .header("idempotency-key", format!("track-add/{creator}/k1"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(boot.track_count().await, before);
}

/// The MCP context only borrows the creator: once every `AppState` clone is gone, the creator and
/// the route state it holds are gone too, so the binding forms no reference cycle.
#[tokio::test]
async fn track_creator_does_not_keep_the_state_alive() {
    let boot = boot(16).await;
    let weak = boot
        .ctx
        .track_creator
        .get()
        .expect("the creator is bound")
        .clone();
    assert!(weak.upgrade().is_some(), "the state owns the creator");
    let Boot { app, .. } = boot;
    drop(app);
    assert!(
        weak.upgrade().is_none(),
        "the creator outlived every AppState: something holds it strongly"
    );
}

/// A creator whose Planner runs on Claude, on a server where Claude is not ready, gets the
/// dependency refusal: -32503, nothing created.
#[tokio::test]
async fn track_add_refuses_while_the_creators_provider_is_unavailable() {
    let boot = boot(16).await;
    let (creator, planner) = boot.user_track("portfolio").await;
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.planner_provider', 'claude') \
         WHERE track_id = ?1 AND role = 'planner'",
    )
    .bind(&creator)
    .execute(boot.repo.pool())
    .await
    .unwrap();
    let before = boot.track_count().await;
    let error = boot
        .add(&planner, boot.args("k1"))
        .await
        .expect_err("Claude is not configured here");
    assert_eq!(error.code, -32503, "{error:?}");
    assert_eq!(boot.track_count().await, before);
}

/// A closed creator adds nothing: the refusal is state (-32409), and no Track is created.
#[tokio::test]
async fn track_add_refuses_closed_creator() {
    let boot = boot(16).await;
    let (creator, planner) = boot.user_track("portfolio").await;
    let (status, closed) = send(
        boot.app.clone(),
        "PATCH",
        &format!("/api/tracks/{creator}"),
        Some(json!({ "closed": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    let before = boot.track_count().await;
    let error = boot
        .add(&planner, boot.args("k1"))
        .await
        .expect_err("a closed Track adds none");
    assert_eq!(error.code, -32409, "{error:?}");
    assert!(error.message.contains("closed"), "{error:?}");
    assert_eq!(boot.track_count().await, before);
}
