//! #1669 S1 — the decisive I1 test (design §3): a Planner calls a fixture
//! stdio plugin through the kernel socket, captures the result with
//! `calm.source.capture { call }`, and `GET /api/tracks/{id}/sources/{id}`
//! answers with `body_sha256 == sha256(text blocks joined by "\n")` of the
//! reply the plugin was programmed with. Plus I6 (a later `isError` call on
//! the same key replaces the record), I4 (a worker's call is not recorded),
//! and the check that recording leaves the value returned to the model
//! byte-identical.
//!
//! One `AppContext` serves both the MCP listener (`spawn_with_context`) and
//! the axum router (`with_mcp_context`), so the ring the transport fills is
//! the ring the tool reads and the rows the route reads are the rows the
//! tool wrote.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_mcp_token_set_tx, card_with_codex_create_tx, session_bind_attribution_tx,
    session_mcp_token_set_tx, session_projection_active_for_card_tx, session_start_runtime_tx,
};
use calm_server::event::EventBus;
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::source::TOOL_SOURCE_CAPTURE;
use calm_server::mcp_server::{McpServer, build_default_registry};
use calm_server::model::{CardRole, NewArea, NewPlugin, NewTrack, now_ms};
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::plugin_results::sha256_hex;
use calm_server::routes;
use calm_server::session_projection_repo::{
    AgentProvider, ThreadAttribution, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::OnceCell;
use tokio::time::{Instant, sleep};
use tower::ServiceExt;

use crate::support::mcp::call_tool_via_socket;

const SERIES_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-series");
const PLUGIN_ID: &str = "dev.wisburg";
const TOOL_NAME: &str = "get-article-detail";
const EXPOSED_NAME: &str = "plugin.dev.wisburg_get-article-detail";
/// The spelling Codex shows the model for [`EXPOSED_NAME`].
const SANITIZED_NAME: &str = "plugin_dev_wisburg_get_article_detail";
const KNOWN_REPLY: &str = include_str!("../fixtures/source_capture/reply.json");

struct Fixture {
    ctx: Arc<AppContext>,
    repo: Arc<dyn Repo>,
    area_id: calm_server::ids::AreaId,
    app: axum::Router,
    cookie: String,
    _server: Arc<McpServer>,
    socket_path: PathBuf,
    control_dir: PathBuf,
    track_id: String,
    planner_token: String,
    planner_thread: String,
    worker_token: String,
    worker_thread: String,
    _tmp: TempDir,
}

/// The text blocks of the programmed reply, joined by `"\n"` — the body
/// I1 says the kernel must have stored.
fn known_body() -> String {
    let reply: Value = serde_json::from_str(KNOWN_REPLY).expect("fixture reply parses");
    reply["result"]["content"]
        .as_array()
        .expect("content array")
        .iter()
        .filter(|block| block["type"] == "text")
        .map(|block| block["text"].as_str().expect("text").to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn socket_safe_tempdir() -> std::io::Result<TempDir> {
    let ambient = std::env::temp_dir();
    let base = if ambient.as_os_str().len() <= 40 {
        ambient
    } else {
        PathBuf::from("/tmp")
    };
    tempfile::Builder::new().prefix("srce2e").tempdir_in(base)
}

fn program(control_dir: &Path, reply: &str) {
    std::fs::write(control_dir.join("reply.json"), reply).expect("program reply");
}

async fn wait_for_running(host: &Arc<PluginHost>, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = host.status(id).await
            && matches!(s.status, PluginRuntimeStatus::Running)
        {
            return;
        }
        assert!(Instant::now() <= deadline, "plugin did not reach Running");
        sleep(Duration::from_millis(25)).await;
    }
}

async fn mint_card_with_thread(
    sqlx_repo: &Arc<SqlxRepo>,
    card_role_cache: &CardRoleCache,
    track_id: calm_server::ids::TrackId,
    role: CardRole,
) -> (String, String) {
    let card_id = calm_server::model::new_id();
    let worker_session_id = calm_server::model::new_id();
    let mut tx = sqlx_repo.pool().begin().await.expect("begin card tx");
    let (_card, _term, mcp_token) = card_with_codex_create_tx(
        &mut tx,
        card_id.clone(),
        &worker_session_id,
        None,
        track_id,
        None,
        None,
        "/workspace".into(),
        json!({}),
        None,
        None,
        None,
        role,
        true,
        card_role_cache,
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("mint card");
    let raw_token = match mcp_token {
        Some(token) => token,
        None => {
            let token = calm_server::mcp_server::auth::CardMcpToken::generate();
            let token_hash = calm_server::mcp_server::auth::hash_token(token.as_str());
            card_mcp_token_set_tx(&mut tx, &card_id, &token_hash)
                .await
                .expect("mint card MCP token");
            session_mcp_token_set_tx(&mut tx, &worker_session_id, &token_hash)
                .await
                .expect("mint session MCP token");
            token.into_inner()
        }
    };
    tx.commit().await.expect("commit card tx");
    let thread_id = format!("thread-{card_id}");
    let mut tx = sqlx_repo.pool().begin().await.expect("begin runtime tx");
    if let Some(runtime) = session_projection_active_for_card_tx(&mut tx, &card_id)
        .await
        .expect("active runtime lookup")
    {
        session_bind_attribution_tx(
            &mut tx,
            &runtime.id,
            ThreadAttribution {
                worker_session_id: runtime.id.clone(),
                provider: AgentProvider::Codex,
                thread_id: Some(thread_id.clone()),
                session_id: None,
                active_turn_id: None,
            },
        )
        .await
        .expect("bind thread attribution");
    } else {
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: calm_server::model::new_id(),
                card_id: card_id.clone(),
                kind: WorkerSessionKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Running,
                terminal_run_id: None,
                thread_id: Some(thread_id.clone()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .expect("start runtime");
    }
    tx.commit().await.expect("commit runtime tx");
    (raw_token, thread_id)
}

async fn boot() -> Fixture {
    let tmp = socket_safe_tempdir().expect("tempdir");
    let socket_path = tmp.path().join("mcp").join("kernel.sock");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let control_dir = tmp.path().join("control");
    std::fs::create_dir_all(&control_dir).expect("control dir");
    std::fs::create_dir_all(&plugins_data_dir).expect("plugins data dir");
    program(&control_dir, KNOWN_REPLY);

    let sqlx_repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.expect("sqlite"));
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    let events = EventBus::new();
    let area = repo
        .area_create(NewArea {
            name: "source-capture".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("area");
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "source-capture".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("track");
    repo.seed_track_area_cache(&track_area_cache)
        .await
        .expect("seed cache");
    let (planner_token, planner_thread) = mint_card_with_thread(
        &sqlx_repo,
        &card_role_cache,
        track.id.clone(),
        CardRole::Planner,
    )
    .await;
    let (worker_token, worker_thread) = mint_card_with_thread(
        &sqlx_repo,
        &card_role_cache,
        track.id.clone(),
        CardRole::Worker,
    )
    .await;

    let install_dir = plugins_dir.join(PLUGIN_ID);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("plugin bin dir");
    std::os::unix::fs::symlink(Path::new(SERIES_BIN), bin_dir.join("stub")).expect("symlink");
    let manifest = Manifest::parse(
        &json!({
            "manifest_version": 1,
            "id": PLUGIN_ID,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Fake Wisburg",
            "entrypoint": {
                "command": "bin/stub",
                "env": { "STUB_SERIES_DIR": control_dir.display().to_string() }
            },
            "exposes_tools": [ { "name": TOOL_NAME, "description": "article detail" } ],
            "permissions": {}
        })
        .to_string(),
    )
    .expect("manifest");
    let registry = PluginRegistry::builder()
        .with(manifest, Some(install_dir.clone()))
        .build();
    repo.plugin_install(NewPlugin {
        id: PLUGIN_ID.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: json!({}),
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("plugin row");
    let write = calm_server::state::WriteContext::new(card_role_cache.clone(), track_area_cache);
    let plugin_host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo.clone(),
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events.clone(),
        write.clone(),
    ));
    plugin_host.spawn(PLUGIN_ID).await.expect("spawn plugin");
    wait_for_running(&plugin_host, PLUGIN_ID).await;
    let plugin_host_cell = Arc::new(OnceCell::new());
    assert!(plugin_host_cell.set(plugin_host.clone()).is_ok());

    let ctx = AppContext::new(
        repo.clone(),
        events.clone(),
        write,
        None,
        plugin_host_cell,
        Arc::new(OnceCell::new()),
        tmp.path().join("gate-logs"),
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    );
    let server = McpServer::spawn_with_context(
        ctx.clone(),
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"),
        build_default_registry(),
    )
    .await
    .expect("spawn McpServer");

    let state = AppState::from_parts(
        repo.clone(),
        events,
        Arc::new(DaemonClient::new_stub()),
        plugin_host,
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        None,
    )
    .with_mcp_context(ctx.clone());
    let auth_state = AuthState::new(AuthConfig {
        username: Some("alice".into()),
        password: Some("hunter2".into()),
        dev_autologin: false,
        display_name: "alice".into(),
    });
    let protected = routes::protected_router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            auth_state.clone(),
            auth::require_session,
        ));
    let app = axum::Router::new()
        .merge(protected)
        .merge(routes::public_router())
        .with_state(state)
        .merge(auth::router().with_state(auth_state));
    let cookie = login(&app).await;

    Fixture {
        ctx,
        repo,
        area_id: area.id,
        app,
        cookie,
        _server: server,
        socket_path,
        control_dir,
        track_id: track.id.to_string(),
        planner_token,
        planner_thread,
        worker_token,
        worker_thread,
        _tmp: tmp,
    }
}

async fn login(app: &axum::Router) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "username": "alice", "password": "hunter2" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "login");
    let raw = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie")
        .to_str()
        .unwrap();
    let first = raw.split(';').next().unwrap();
    assert!(first.starts_with(&format!("{SESSION_COOKIE}=")));
    first.to_string()
}

async fn get(app: &axum::Router, uri: &str, cookie: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::builder().method("GET").uri(uri);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    let resp = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

impl Fixture {
    async fn planner_call(&self, id: i64, name: &str, args: Value) -> Value {
        call_tool_via_socket(
            &self.socket_path,
            &self.planner_token,
            &self.planner_thread,
            id,
            name,
            args,
        )
        .await
    }

    async fn worker_call(&self, id: i64, name: &str, args: Value) -> Value {
        call_tool_via_socket(
            &self.socket_path,
            &self.worker_token,
            &self.worker_thread,
            id,
            name,
            args,
        )
        .await
    }

    /// `calm.source.capture` over the socket; the structured payload of a
    /// success, the error object of a refusal.
    async fn capture(&self, id: i64, args: Value) -> Result<Value, Value> {
        let frame = self.planner_call(id, TOOL_SOURCE_CAPTURE, args).await;
        if let Some(error) = frame.get("error") {
            return Err(error.clone());
        }
        Ok(frame["result"]["structuredContent"].clone())
    }

    async fn get_source(&self, source_id: &str) -> (StatusCode, Value) {
        get(
            &self.app,
            &format!("/api/tracks/{}/sources/{source_id}", self.track_id),
            Some(&self.cookie),
        )
        .await
    }
}

fn plugin_result(frame: &Value) -> &Value {
    assert!(
        frame.get("error").is_none(),
        "plugin call errored: {frame:#?}"
    );
    &frame["result"]
}

// ---------------------------------------------------------------------------
// I1 — the captured body is the proxy's returned text blocks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn i1_captured_body_sha256_equals_the_proxied_text_blocks() {
    let fx = boot().await;
    let args = json!({ "id": 752972 });
    let frame = fx.planner_call(10, EXPOSED_NAME, args.clone()).await;
    let returned = plugin_result(&frame);
    // Recording changed nothing the model sees: the wire result is the
    // programmed reply, structuredContent and image block included.
    let programmed: Value = serde_json::from_str(KNOWN_REPLY).unwrap();
    assert_eq!(returned, &programmed["result"], "{returned:#?}");

    let receipt = fx
        .capture(
            11,
            json!({
                "call": { "tool": SANITIZED_NAME, "args": args },
                "provenance": "full_text",
                "title": "ECB September path",
                "published_at": "2026-09-14",
                "content_id": "752972",
                "quotes": ["data-dependent"],
            }),
        )
        .await
        .expect("capture");
    let body = known_body();
    let expected_sha = sha256_hex(body.as_bytes());
    assert_eq!(receipt["body_sha256"], expected_sha, "{receipt}");
    assert_eq!(receipt["body_bytes"], body.len());
    assert_eq!(receipt["matched_call"]["tool"], EXPOSED_NAME);
    assert_eq!(receipt["matched_call"]["args"], json!({ "id": 752972 }));
    assert_eq!(receipt["quotes"][0]["id"], "q1");
    let source_id = receipt["source_id"].as_str().unwrap().to_string();

    let (status, detail) = fx.get_source(&source_id).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["body_sha256"], expected_sha, "{detail}");
    assert_eq!(detail["body"], body, "the stored body is the joined text");
    assert_eq!(detail["provenance"], "full_text");
    assert_eq!(detail["origin"]["kind"], "plugin");
    assert_eq!(detail["origin"]["plugin_id"], PLUGIN_ID);
    assert_eq!(detail["origin"]["tool"], TOOL_NAME);
    assert_eq!(detail["origin"]["args_canon"], "v1");
    assert_eq!(detail["content_id"], "752972");
    assert_eq!(detail["quotes"][0]["text"], "data-dependent");
    let start = detail["quotes"][0]["start"].as_u64().unwrap() as usize;
    let end = detail["quotes"][0]["end"].as_u64().unwrap() as usize;
    assert_eq!(&body[start..end], "data-dependent");

    let (status, list) = get(
        &fx.app,
        &format!("/api/tracks/{}/sources", fx.track_id),
        Some(&fx.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["sources"][0]["source_id"], source_id);
    assert!(list["sources"][0].get("body").is_none(), "{list}");

    // I6 — the same key called again, now failing: the record is replaced
    // and the old body is not capturable.
    program(
        &fx.control_dir,
        &json!({ "mode": "is_error", "text": "upstream 500" }).to_string(),
    );
    let frame = fx
        .planner_call(12, EXPOSED_NAME, json!({ "id": 752972 }))
        .await;
    assert_eq!(plugin_result(&frame)["isError"], json!(true));
    let error = fx
        .capture(
            13,
            json!({
                "call": { "tool": EXPOSED_NAME, "args": { "id": 752972 } },
                "provenance": "full_text",
                "title": "again",
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error["code"], -32602, "{error}");
    assert!(
        error["message"].as_str().unwrap().contains("isError"),
        "{error}"
    );
    // The first capture is untouched by the failed call.
    let (status, detail) = fx.get_source(&source_id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["body_sha256"], expected_sha);
}

// ---------------------------------------------------------------------------
// I6 on a transport failure — no `CallToolResult` at all still replaces
// ---------------------------------------------------------------------------

/// After a success, the same call fails *below* the reply level: the stub
/// answers something that is not a `CallToolResult` (a bare string), so
/// the kernel's `tools_call` returns `Err` before any `isError` verdict
/// exists. The Planner sees the RPC error, and the recorded entry for that
/// key is `Error` — the old body is not capturable any more.
#[tokio::test]
async fn i6_a_transport_error_on_the_same_key_replaces_the_success() {
    let fx = boot().await;
    let args = json!({ "id": 99 });
    plugin_result(&fx.planner_call(40, EXPOSED_NAME, args.clone()).await);
    // Sanity: the success is capturable right now.
    fx.capture(
        41,
        json!({
            "call": { "tool": EXPOSED_NAME, "args": args.clone() },
            "provenance": "full_text",
            "title": "before",
        }),
    )
    .await
    .expect("capture after success");

    program(
        &fx.control_dir,
        &json!({ "mode": "raw", "result": "not a CallToolResult" }).to_string(),
    );
    let frame = fx.planner_call(42, EXPOSED_NAME, args.clone()).await;
    let error = frame
        .get("error")
        .expect("an unparseable reply is an RPC error, not a result");
    assert_eq!(error["code"], -32603, "{frame}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("did not parse as CallToolResult"),
        "{frame}"
    );

    let refusal = fx
        .capture(
            43,
            json!({
                "call": { "tool": EXPOSED_NAME, "args": args },
                "provenance": "full_text",
                "title": "after",
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(refusal["code"], -32602, "{refusal}");
    assert!(
        refusal["message"]
            .as_str()
            .unwrap()
            .contains("returned isError"),
        "the recorded status is `error`: {refusal}"
    );
    // The omitted-args form sees the same (latest) entry.
    let refusal = fx
        .capture(
            44,
            json!({
                "call": { "tool": EXPOSED_NAME },
                "provenance": "full_text",
                "title": "after",
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(refusal["code"], -32602, "{refusal}");
}

// ---------------------------------------------------------------------------
// I4 — a worker's call is not recorded for the planner
// ---------------------------------------------------------------------------

#[tokio::test]
async fn i4_a_workers_call_leaves_nothing_for_the_planner_to_capture() {
    let fx = boot().await;
    let args = json!({ "id": 4242 });
    let frame = fx.worker_call(20, EXPOSED_NAME, args.clone()).await;
    plugin_result(&frame);
    assert!(
        fx.ctx.plugin_results.is_empty(&fx.track_id),
        "a worker call must not enter the ring"
    );
    let error = fx
        .capture(
            21,
            json!({
                "call": { "tool": EXPOSED_NAME, "args": args },
                "provenance": "full_text",
                "title": "x",
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error["code"], -32602, "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("tools with a recorded result in this track: []"),
        "{error}"
    );
    // The worker cannot capture either (require_role, not the allowlist).
    let frame = fx
        .worker_call(
            22,
            TOOL_SOURCE_CAPTURE,
            json!({
                "call": { "tool": EXPOSED_NAME, "args": { "id": 4242 } },
                "provenance": "full_text",
                "title": "x",
            }),
        )
        .await;
    assert_eq!(frame["error"]["code"], -32602, "{frame}");
    assert!(
        frame["error"]["message"]
            .as_str()
            .unwrap()
            .contains("requires role=Planner"),
        "{frame}"
    );
    let (status, list) = get(
        &fx.app,
        &format!("/api/tracks/{}/sources", fx.track_id),
        Some(&fx.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["sources"], json!([]));
}

// ---------------------------------------------------------------------------
// REST — 401 without a session, 404 across tracks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rest_sources_need_a_session_and_stay_inside_their_track() {
    let fx = boot().await;
    fx.planner_call(30, EXPOSED_NAME, json!({ "id": 1 })).await;
    let receipt = fx
        .capture(
            31,
            json!({
                "call": { "tool": EXPOSED_NAME },
                "provenance": "summary",
                "title": "x",
            }),
        )
        .await
        .expect("capture");
    let source_id = receipt["source_id"].as_str().unwrap().to_string();

    let (status, _) = get(
        &fx.app,
        &format!("/api/tracks/{}/sources", fx.track_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = get(
        &fx.app,
        &format!("/api/tracks/{}/sources/{source_id}", fx.track_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = get(
        &fx.app,
        "/api/tracks/no-such-track/sources",
        Some(&fx.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(
        &fx.app,
        &format!("/api/tracks/{}/sources/src_00000000", fx.track_id),
        Some(&fx.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Another track of the same area does not see this source.
    let other = fx
        .repo
        .track_create(NewTrack {
            template_input: None,
            area_id: fx.area_id.clone(),
            title: "other".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("other track");
    let (status, _) = get(
        &fx.app,
        &format!("/api/tracks/{}/sources/{source_id}", other.id.as_str()),
        Some(&fx.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, list) = get(
        &fx.app,
        &format!("/api/tracks/{}/sources", other.id.as_str()),
        Some(&fx.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["sources"], json!([]));
}
