//! #1164 P1 — external connector host (`kind: mcp-http` / `cli-query`).
//!
//! Covers the design doc's §4 acceptance list, minus the items that belong to
//! later slices:
//!
//! * **#1** install + enable → Running, and still Running after a full service
//!   restart (simulated by rebuilding host + registry from disk over the same
//!   repo, which is exactly what boot does).
//! * **#3** a real `tools/call` returns real upstream data — against a local
//!   stub HTTP MCP server that reproduces the recorded wire shape from §1.1
//!   (single `event: message` frame, one `data:` line, no session header).
//!   Never the real network.
//! * **#5** `secrets.json` values appear in no API response.
//! * **#6** stopping an `app` plugin makes its tools invisible immediately
//!   (anti-relaxation regression for `process: Option<…>`).
//! * **#7** the boot audit's `PluginToolRegistered` read sees connector tools,
//!   which is only possible if materialization happens BEFORE the live
//!   `Running` publication (§2.7(1)).
//! * **#8** `rotate-token` on a connector is a 4xx AND has no side effects —
//!   including when the registry has no entry at all, where the guard must fail
//!   CLOSED rather than fall through to the delete + restart.
//! * **#10** a hung upstream does not block boot; the connector lands
//!   `Unavailable`.
//! * **#11** `set_exposes_tools` no-ops for an absent id: an uninstall that
//!   completes while a spawn is in flight must not resurrect the entry.
//!
//! Plus two things §4 does not enumerate but a review found missing:
//! the API key must appear in NO error sink (`Unavailable` reason, `/enable`
//! body, `tools_call` error) for either a refused connection or a hung
//! upstream; and a connector's card-creation refusal is asserted by driving
//! the REAL `POST /api/waves/{id}/cards` route, not its two accessors.
//!
//! §4 #2 and #9 (discovery + underscore routing) are unit tests against the
//! production projection/route functions in `mcp_server::transport`; §4 #4 is
//! `cli-query` execution, which is a later slice.

#![cfg(unix)]

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{
    ConnectorClient, HostError, PluginHost, PluginRegistry, PluginRuntimeStatus,
};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::{Instant, sleep};
use tower::ServiceExt;

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-echo");

const CONNECTOR_ID: &str = "mcp-wisburg";
const SECRET_NAME: &str = "WISBURG_API_KEY";
const SECRET_VALUE: &str = "sk-super-secret-do-not-leak-8213";
/// Underscores on purpose (§4 #9): the id↔tool boundary is `_`.
const ALLOWED_TOOL: &str = "list_institutional_reports";
const ALLOWED_TOOL_2: &str = "get_report_detail";
/// Served upstream but NOT in `tools_allow` — must never materialize.
const DENIED_TOOL: &str = "admin_purge";

// ===========================================================================
// Stub upstream MCP server
//
// Mimics the shape recorded in design §1.1: `content-type: text/event-stream`
// with a single `event: message` + one `data:` line and no `Mcp-Session-Id`.
// Deliberately hand-rolled HTTP/1.1 — the point is to reproduce THAT wire
// shape, and a framework would normalize it away.
// ===========================================================================

#[derive(Clone, Copy, PartialEq)]
enum StubMode {
    /// Answer everything promptly.
    Normal,
    /// Accept the connection, read the request, then never write. This is the
    /// failure that would otherwise hang boot (§2.2).
    Hang,
}

struct StubServer {
    addr: std::net::SocketAddr,
    /// Query strings seen, so a test can prove the API key rode along.
    seen_queries: Arc<std::sync::Mutex<Vec<String>>>,
    /// Methods seen, in order.
    seen_methods: Arc<std::sync::Mutex<Vec<String>>>,
    /// Set once `tools/list` has been received (used to line up the
    /// uninstall-vs-in-flight-spawn race deterministically).
    tools_list_received: Arc<AtomicBool>,
    _task: tokio::task::JoinHandle<()>,
}

impl StubServer {
    async fn start(mode: StubMode) -> Self {
        Self::start_gated(mode, None).await
    }

    /// `gate` (when present) is awaited before the `tools/list` reply is
    /// written — the seam the uninstall race test needs.
    async fn start_gated(mode: StubMode, gate: Option<oneshot::Receiver<()>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
        let addr = listener.local_addr().expect("local addr");
        let seen_queries = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_methods = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tools_list_received = Arc::new(AtomicBool::new(false));

        let queries = Arc::clone(&seen_queries);
        let methods = Arc::clone(&seen_methods);
        let received = Arc::clone(&tools_list_received);
        let mut gate = gate;

        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let (target, body) = match read_request(&mut sock).await {
                    Some(v) => v,
                    None => continue,
                };
                if let Some(q) = target.split_once('?').map(|(_, q)| q.to_string()) {
                    queries.lock().unwrap().push(q);
                } else {
                    queries.lock().unwrap().push(String::new());
                }
                let req: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let method = req
                    .get("method")
                    .and_then(|m| m.as_str())
                    .unwrap_or_default()
                    .to_string();
                methods.lock().unwrap().push(method.clone());
                let id = req.get("id").cloned().unwrap_or(json!(1));

                if method == "tools/list" {
                    received.store(true, Ordering::SeqCst);
                    if let Some(rx) = gate.take() {
                        let _ = rx.await;
                    }
                }

                if mode == StubMode::Hang {
                    // Hold the socket open forever without writing. Dropping
                    // the task at test teardown closes it.
                    std::future::pending::<()>().await;
                }

                let result = match method.as_str() {
                    "initialize" => json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "stub-mcp", "version": "0.8.4" }
                    }),
                    "tools/list" => json!({ "tools": [
                        { "name": ALLOWED_TOOL, "description": "institutional reports",
                          "inputSchema": { "type": "object",
                                           "properties": { "page": { "type": "number" } } } },
                        { "name": ALLOWED_TOOL_2, "description": "one report",
                          "inputSchema": { "type": "object" } },
                        { "name": DENIED_TOOL, "description": "must stay hidden",
                          "inputSchema": { "type": "object" } },
                    ]}),
                    "tools/call" => {
                        let called = req
                            .pointer("/params/name")
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string();
                        json!({
                            "content": [{ "type": "text", "text": format!("rows for {called}") }],
                            "structuredContent": { "rows": 3, "tool": called },
                            "isError": false
                        })
                    }
                    _ => json!({}),
                };
                let payload = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
                // §1.1's exact framing: one `event:` line, one `data:` line.
                let sse = format!("event: message\ndata: {payload}\n\n");
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n",
                    sse.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(sse.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });

        Self {
            addr,
            seen_queries,
            seen_methods,
            tools_list_received,
            _task: task,
        }
    }

    fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    fn methods(&self) -> Vec<String> {
        self.seen_methods.lock().unwrap().clone()
    }

    fn queries(&self) -> Vec<String> {
        self.seen_queries.lock().unwrap().clone()
    }

    async fn wait_for_tools_list(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.tools_list_received.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "stub never saw tools/list");
            sleep(Duration::from_millis(5)).await;
        }
    }
}

/// Read one HTTP/1.1 request, returning `(request-target, body)`.
async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, String)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    let head_end = loop {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let target = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    let len: usize = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some((
        target,
        String::from_utf8_lossy(&buf[head_end..head_end + len]).to_string(),
    ))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ===========================================================================
// On-disk fixtures
// ===========================================================================

fn connector_manifest_json(url: &str, timeout_ms: u64) -> Value {
    json!({
        "manifest_version": 1,
        "kind": "mcp-http",
        "id": CONNECTOR_ID,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Wisburg Research",
        "mcp_http": {
            "url": url,
            "api_key_secret": SECRET_NAME,
            "api_key_in": "query:api_key",
            "tools_allow": [ALLOWED_TOOL, ALLOWED_TOOL_2],
            "request_timeout_ms": timeout_ms,
        }
    })
}

/// Write a connector directory INSIDE `plugins_dir` (design §0 / D9: the
/// source must live there so install hits the `src == dst` short-circuit and
/// lands a real directory, not the symlink `load_from_dir` skips).
fn write_connector(plugins_dir: &Path, url: &str, timeout_ms: u64, secret_mode: u32) -> PathBuf {
    let dir = plugins_dir.join(CONNECTOR_ID);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&connector_manifest_json(url, timeout_ms)).unwrap(),
    )
    .unwrap();

    let secrets = dir.join("secrets.json");
    let mut f = std::fs::File::create(&secrets).unwrap();
    f.write_all(json!({ SECRET_NAME: SECRET_VALUE }).to_string().as_bytes())
        .unwrap();
    drop(f);
    std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(secret_mode)).unwrap();
    dir
}

fn write_app_plugin(plugins_dir: &Path, id: &str) -> PathBuf {
    let dir = plugins_dir.join(id);
    let bin_dir = dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(ECHO_BIN), bin_dir.join("stub")).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        json!({
            "manifest_version": 1,
            "id": id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Echo Stub",
            "entrypoint": { "command": "bin/stub" },
            "exposes_tools": [{ "name": "do_thing", "description": "noop" }],
        })
        .to_string(),
    )
    .unwrap();
    dir
}

// ===========================================================================
// Host / AppState boot helpers
// ===========================================================================

struct Boot {
    repo: Arc<dyn Repo>,
    plugins_dir: PathBuf,
    plugins_data_dir: PathBuf,
    events: EventBus,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("in-memory sqlite"),
    );
    Boot {
        repo,
        plugins_dir,
        plugins_data_dir,
        events: EventBus::new(),
        _tmp: tmp,
    }
}

impl Boot {
    /// Build a `PluginHost` whose registry is hydrated **from disk**, exactly
    /// like `AppState::new` does at boot. Calling this twice over the same
    /// `plugins_dir` + repo is our stand-in for a full service restart.
    fn host(&self) -> Arc<PluginHost> {
        let (registry, report) = PluginRegistry::load_from_dir(&self.plugins_dir).unwrap();
        assert!(
            report.skipped.is_empty(),
            "registry skipped plugin dirs: {:?}",
            report.skipped
        );
        Arc::new(PluginHost::new_full(
            Arc::new(registry),
            self.repo.clone(),
            self.plugins_dir.clone(),
            self.plugins_data_dir.clone(),
            Vec::new(),
            self.events.clone(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        ))
    }

    fn state(&self, host: Arc<PluginHost>) -> AppState {
        AppState::from_parts(
            self.repo.clone(),
            self.events.clone(),
            Arc::new(DaemonClient::new_stub()),
            host,
            Arc::new(calm_server::state::CodexClient::new_stub()),
            None,
            None,
        )
    }
}

impl Boot {
    /// One cove + one wave, so a test can drive `POST /api/waves/{id}/cards`.
    async fn seed_wave(&self) -> String {
        let cove = self
            .repo
            .cove_create(calm_server::model::NewCove {
                name: "demo".into(),
                color: "#fff".into(),
                sort: None,
            })
            .await
            .unwrap();
        self.repo
            .wave_create(calm_server::model::NewWave {
                workflow_input: None,
                cove_id: cove.id.clone(),
                title: "demo".into(),
                sort: None,
                cwd: String::new(),
                workflow_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap()
            .id
            .to_string()
    }
}

fn app(state: AppState) -> axum::Router {
    axum::Router::new()
        .merge(routes::plugins::router())
        .with_state(state)
}

/// The cards router needs the actor middleware (it reads `Actor` from
/// extensions), exactly as `main.rs` wires it.
fn cards_app(state: AppState) -> axum::Router {
    axum::Router::new()
        .merge(routes::cards::router())
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

async fn post_json(state: &AppState, path: &str, body: Value) -> (StatusCode, Value) {
    let resp = app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn get_text(state: &AppState, path: &str) -> (StatusCode, String) {
    let resp = app(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

/// The exact read `AppState::new`'s boot audit loop performs
/// (`state.rs`: registry manifests ∩ running ids → `exposes_tools`).
async fn boot_audit_tool_names(host: &Arc<PluginHost>) -> Vec<String> {
    let running = host.running_plugin_ids().await;
    let mut out = Vec::new();
    for manifest in host.registry().list() {
        if !running.contains(&manifest.id) {
            continue;
        }
        for entry in manifest.exposes_tools {
            out.push(format!("{}::{}", manifest.id, entry.name));
        }
    }
    out.sort();
    out
}

// ===========================================================================
// §4 #1 — install + enable → Running, survives a restart
// ===========================================================================

#[tokio::test]
async fn connector_installs_enables_and_stays_running_across_restart() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    let dir = write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);

    // Install through the REAL route (design §0: "install path, zero new
    // code"). Source is inside plugins_dir, per D9.
    let state = b.state(b.host());
    let (status, body) = post_json(
        &state,
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": dir.display().to_string() } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "install failed: {body}");

    let (status, body) = post_json(
        &state,
        &format!("/api/plugins/{CONNECTOR_ID}/enable"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "enable failed: {body}");
    assert_eq!(body.get("state").and_then(|s| s.as_str()), Some("running"));
    // No child process — that is the whole point of §2.5's `Option<Arc<…>>`.
    assert!(
        body.get("pid").map(|p| p.is_null()).unwrap_or(true),
        "connector must not report a pid: {body}"
    );

    // --- simulated full service restart -------------------------------
    // Fresh host + registry re-hydrated from disk, same repo (the `enabled`
    // row persists), then the boot autospawn loop.
    let host2 = b.host();
    assert!(
        host2.registry().get(CONNECTOR_ID).is_some(),
        "connector must be re-hydrated from plugins_dir on boot"
    );
    host2.autospawn_enabled().await;
    let after = host2.status(CONNECTOR_ID).await.expect("status after boot");
    assert!(
        matches!(after.status, PluginRuntimeStatus::Running),
        "connector must be Running after restart, got {:?}",
        after.status
    );
    assert!(after.pid.is_none());

    // The upstream really was contacted twice (once per boot).
    assert!(
        stub.methods().iter().filter(|m| *m == "tools/list").count() >= 2,
        "expected a tools/list per boot, saw {:?}",
        stub.methods()
    );
}

// ===========================================================================
// §4 #3 — a real tools/call returns real upstream data
// ===========================================================================

#[tokio::test]
async fn connector_tools_call_returns_upstream_data_and_sends_the_api_key() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;
    host.spawn(CONNECTOR_ID).await.expect("connector spawns");

    let client = host
        .connector_client(CONNECTOR_ID)
        .await
        .expect("running connector must expose a client");
    let ConnectorClient::Http(http) = &client else {
        panic!("expected an Http connector client, got {client:?}");
    };

    let result = http
        .tools_call(ALLOWED_TOOL, json!({ "page": 1 }))
        .await
        .expect("tools/call");
    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        result.content.first().and_then(|c| c.text.as_deref()),
        Some(format!("rows for {ALLOWED_TOOL}").as_str())
    );
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|s| s.get("rows")),
        Some(&json!(3))
    );

    // The API key rode in the query string, as `api_key_in: "query:api_key"`
    // declared — and the SSE envelope was stripped, or nothing above parsed.
    assert!(
        stub.queries()
            .iter()
            .any(|q| q.contains(&format!("api_key={SECRET_VALUE}"))),
        "api key was not sent in the query: {:?}",
        stub.queries()
    );

    // Materialization respected the allowlist (§2.2).
    let manifest = host.registry().get(CONNECTOR_ID).unwrap();
    let mut names: Vec<&str> = manifest
        .exposes_tools
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    names.sort();
    assert_eq!(names, vec![ALLOWED_TOOL_2, ALLOWED_TOOL]);
    assert!(
        !names.contains(&DENIED_TOOL),
        "a tool outside tools_allow must never materialize"
    );
    // Materialized entries carry the upstream schema, not a placeholder.
    let listed = manifest
        .exposes_tools
        .iter()
        .find(|t| t.name == ALLOWED_TOOL)
        .unwrap();
    assert!(
        listed
            .input_schema
            .as_ref()
            .and_then(|s| s.pointer("/properties/page"))
            .is_some(),
        "upstream inputSchema must be carried over: {:?}",
        listed.input_schema
    );
}

// ===========================================================================
// §4 #5 — secrets never reach an API response
// ===========================================================================

#[tokio::test]
async fn secrets_json_values_never_appear_in_any_plugin_api_response() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    let dir = write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    let state = b.state(b.host());
    post_json(
        &state,
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": dir.display().to_string() } }),
    )
    .await;
    post_json(
        &state,
        &format!("/api/plugins/{CONNECTOR_ID}/enable"),
        json!({}),
    )
    .await;

    for path in [
        "/api/plugins".to_string(),
        format!("/api/plugins/{CONNECTOR_ID}"),
        "/api/plugins/views".to_string(),
    ] {
        let (status, text) = get_text(&state, &path).await;
        assert!(status.is_success(), "{path} -> {status}");
        assert!(
            !text.contains(SECRET_VALUE),
            "secret value leaked into {path}: {text}"
        );
    }

    // Belt and braces: the in-memory manifest (which materialization DOES
    // mutate) must not have grown a secret either.
    let manifest_json = state
        .plugin
        .registry()
        .get(CONNECTOR_ID)
        .unwrap()
        .to_json()
        .to_string();
    assert!(!manifest_json.contains(SECRET_VALUE));
    // The reference NAME is fine to expose — only the value is secret.
    assert!(manifest_json.contains(SECRET_NAME));
}

/// The happy-path secrets test above walks only successful requests, so it
/// would stay green with the key leaking through every FAILURE path. This one
/// drives the two failure shapes an operator actually hits and asserts the
/// secret appears in NONE of the three sinks a `ureq` transport error reaches:
/// the `Unavailable` reason (persisted + broadcast as
/// `Event::PluginState.last_error`), the `POST /enable` 503 body, and the
/// `tools_call` error that becomes wave transcript text.
///
/// The leak this pins: `ureq::Error`'s `Display` prints the FULL URL first, and
/// `HttpMcpClient::new` folds the API key into that URL's query string.
#[tokio::test]
async fn a_failing_connector_never_leaks_the_api_key_into_any_error_sink() {
    // ---- case A: connection refused (bind, note the port, then drop) ----
    let dead_port = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let p = l.local_addr().unwrap().port();
        drop(l);
        p
    };
    let refused_url = format!("http://127.0.0.1:{dead_port}/mcp");
    // ---- case B: upstream accepts then never answers ----
    let hung = StubServer::start(StubMode::Hang).await;

    for (label, url) in [
        ("connection refused", refused_url),
        ("hung upstream", hung.url()),
    ] {
        let b = boot().await;
        let dir = write_connector(&b.plugins_dir, &url, 400, 0o600);
        let state = b.state(b.host());

        let (status, body) = post_json(
            &state,
            "/api/plugins/install",
            json!({ "source": { "kind": "local_path", "path": dir.display().to_string() } }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{label}: install: {body}");

        // Sink 1: the `/enable` response body, read as raw text so we see it
        // verbatim rather than through a parsed `Value`.
        let resp = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/plugins/{CONNECTOR_ID}/enable"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let enable_status = resp.status();
        let enable_body = String::from_utf8(
            resp.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(
            enable_status.is_client_error() || enable_status.is_server_error(),
            "{label}: enable must fail, got {enable_status}: {enable_body}"
        );
        assert!(
            !enable_body.contains(SECRET_VALUE),
            "{label}: the API key leaked into the enable response body: {enable_body}"
        );

        // Sink 3: the runtime `Unavailable` reason — the same string that is
        // persisted and broadcast as `PluginState.last_error`.
        let st = state
            .plugin
            .status(CONNECTOR_ID)
            .await
            .expect("a failed connector must be observable");
        let PluginRuntimeStatus::Unavailable { reason } = &st.status else {
            panic!("{label}: expected Unavailable, got {:?}", st.status);
        };
        assert!(
            !reason.contains(SECRET_VALUE),
            "{label}: the API key leaked into last_error: {reason}"
        );

        // Sink 4: a `tools_call` failure, which becomes wave transcript text.
        // Build the client the same way `spawn_mcp_http` does, then call it
        // against the same dead/hung upstream.
        let manifest = state.plugin.registry().get(CONNECTOR_ID).unwrap();
        let client = calm_server::plugin_host::HttpMcpClient::new(
            CONNECTOR_ID,
            manifest.mcp_http.as_ref().unwrap(),
            Some(SECRET_VALUE),
        );
        let err = client
            .tools_call(ALLOWED_TOOL, json!({}))
            .await
            .expect_err("tools/call against a dead upstream must fail");
        assert!(
            !err.message.contains(SECRET_VALUE),
            "{label}: the API key leaked into a tools/call error: {}",
            err.message
        );
        // And a `Debug` of the client itself, which is what a `RunningPlugin`
        // dump or a `tracing` field would render.
        let dbg = format!("{client:?}");
        assert!(!dbg.contains(SECRET_VALUE), "{label}: {dbg}");

        // The failures must still SAY something useful.
        assert!(
            reason.contains("127.0.0.1") || reason.contains("timed out"),
            "{label}: reason must remain diagnosable: {reason}"
        );
    }
}

/// `cli-query` parses and installs in this slice but has no execution runtime
/// yet. That is a "not implemented" condition, not a kernel fault: it must
/// report through the same channel as every other connector bring-up failure
/// (503 + an observable `Unavailable`), not as a 500. `HostError::BadState`
/// has no arm in `spawn_error_to_calm`, so routing it through `BadState` gave
/// an operator a kernel-fault-shaped 500.
#[tokio::test]
async fn cli_query_enable_is_a_503_not_a_kernel_fault_500() {
    let b = boot().await;
    let id = "cli-longbridge";
    let dir = b.plugins_dir.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        json!({
            "manifest_version": 1,
            "kind": "cli-query",
            "id": id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Longbridge",
            "cli_query": {
                "command": "longbridge",
                "tools": [{
                    "name": "quote",
                    "input_schema": {
                        "type": "object",
                        "properties": { "symbol": { "type": "string" } },
                        "required": ["symbol"],
                        "additionalProperties": false
                    },
                    "args": ["quote", "{{symbol}}"]
                }]
            }
        })
        .to_string(),
    )
    .unwrap();

    let state = b.state(b.host());
    let (status, body) = post_json(
        &state,
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": dir.display().to_string() } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "install failed: {body}");

    let (status, body) = post_json(&state, &format!("/api/plugins/{id}/enable"), json!({})).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "cli-query enable must be a 503, got {status}: {body}"
    );
    assert!(
        body.to_string().contains("not implemented"),
        "the reason must be actionable: {body}"
    );

    // And it is observable, like every other failed connector.
    let st = state.plugin.status(id).await.expect("observable status");
    assert!(
        matches!(st.status, PluginRuntimeStatus::Unavailable { .. }),
        "got {:?}",
        st.status
    );
    assert!(!state.plugin.running_plugin_ids().await.contains(id));
}

/// §2.4 — a wrongly-permissioned secrets file refuses the enable outright.
#[tokio::test]
async fn world_readable_secrets_file_refuses_enable() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o644);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    let err = host.spawn(CONNECTOR_ID).await.expect_err("must refuse");
    let msg = err.to_string();
    assert!(
        msg.contains("0600"),
        "error must state the requirement: {msg}"
    );
    assert!(
        matches!(err, HostError::ConnectorUnavailable { .. }),
        "got {err:?}"
    );
    // The failure must be OBSERVABLE, not just returned to whoever called
    // `enable`. Boot autospawn swallows the error, so the runtime entry is an
    // operator's only signal.
    let status = host
        .status(CONNECTOR_ID)
        .await
        .expect("a failed connector must still have a status");
    let PluginRuntimeStatus::Unavailable { reason } = &status.status else {
        panic!("expected Unavailable, got {:?}", status.status);
    };
    assert!(reason.contains("secrets.json"), "{reason}");
    assert!(!host.running_plugin_ids().await.contains(CONNECTOR_ID));
    assert!(
        !stub.methods().iter().any(|m| m == "tools/list"),
        "the upstream must not be contacted at all: {:?}",
        stub.methods()
    );
}

// ===========================================================================
// §4 #6 — stopping an `app` plugin hides its tools immediately
//
// Anti-relaxation regression for §2.5: making `process` optional must not
// weaken "process gone ⇒ tools gone". Visibility keys off `status` alone, and
// `running_plugin_ids` is the single gate both discovery
// (`plugin_tool_descriptors`) and dispatch (`plugin_tool_route`) consult.
// ===========================================================================

#[tokio::test]
async fn stopping_an_app_plugin_removes_it_from_the_running_set_immediately() {
    let b = boot().await;
    write_app_plugin(&b.plugins_dir, "app-echo");
    let host = b.host();
    seed_row(&b, "app-echo").await;
    host.spawn("app-echo").await.expect("app spawns");

    assert!(host.running_plugin_ids().await.contains("app-echo"));
    assert!(host.mcp_client("app-echo").await.is_some());
    assert!(!boot_audit_tool_names(&host).await.is_empty());

    host.stop("app-echo").await.expect("stop");

    assert!(
        !host.running_plugin_ids().await.contains("app-echo"),
        "a stopped plugin must leave the running set at once"
    );
    assert!(host.mcp_client("app-echo").await.is_none());
    assert!(host.connector_client("app-echo").await.is_none());
    assert!(
        boot_audit_tool_names(&host).await.is_empty(),
        "no tool may remain visible for a stopped plugin"
    );
}

// ===========================================================================
// §4 #7 — materialization strictly precedes the live `Running` publication
// ===========================================================================

#[tokio::test]
async fn connector_tools_are_materialized_before_the_id_becomes_running() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    host.spawn(CONNECTOR_ID).await.expect("connector spawns");

    // STRUCTURAL, not sampled. The two production steps are adjacent
    // SYNCHRONOUS blocks with no `.await` between them, so no concurrent
    // observer can ever be scheduled in the gap — a sampling test passes just
    // as green with the two blocks swapped, which makes it not a test of the
    // ordering at all. `connector_spawn_order` stamps a process-global
    // monotonic tick as the last action of each block, so swapping the blocks
    // swaps the ticks and this assertion fails.
    let order = host
        .connector_spawn_order(CONNECTOR_ID)
        .expect("a successful connector spawn must record both steps");
    let (materialized, inserted) = (
        order.materialized_at.expect("materialization tick"),
        order.live_inserted_at.expect("live-insert tick"),
    );
    assert!(
        materialized < inserted,
        "materialization (tick {materialized}) must strictly precede the live \
         `Running` insert (tick {inserted}) — §2.7(1). `running_plugin_ids` gates \
         both tool discovery and the boot audit, and both then read \
         `manifest.exposes_tools`; publishing Running first opens a window in \
         which the connector is visible with an empty catalog."
    );
    assert!(order.materialized_before_live_insert());

    // And the steady state is what the ordering was protecting: Running WITH a
    // populated catalog.
    assert!(host.running_plugin_ids().await.contains(CONNECTOR_ID));
    assert_eq!(
        host.registry()
            .get(CONNECTOR_ID)
            .map(|m| m.exposes_tools.len()),
        Some(2)
    );

    // And the boot audit's own read sees them, so `PluginToolRegistered`
    // covers external connectors with no audit hole.
    let audited = boot_audit_tool_names(&host).await;
    assert_eq!(
        audited,
        vec![
            format!("{CONNECTOR_ID}::{ALLOWED_TOOL_2}"),
            format!("{CONNECTOR_ID}::{ALLOWED_TOOL}"),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>(),
    );
}

// ===========================================================================
// §4 #8 — rotate-token on a connector is a 4xx with NO side effects
// ===========================================================================

#[tokio::test]
async fn rotate_token_on_a_connector_is_rejected_without_side_effects() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    let dir = write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    let state = b.state(b.host());
    post_json(
        &state,
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": dir.display().to_string() } }),
    )
    .await;
    post_json(
        &state,
        &format!("/api/plugins/{CONNECTOR_ID}/enable"),
        json!({}),
    )
    .await;

    // Plant a token row so a stray delete would be observable. (Production
    // never mints one for a connector — the kind branch precedes
    // `ensure_plugin_token` — which is exactly what makes the delete a bug.)
    b.repo
        .plugin_token_set(CONNECTOR_ID, "planted-hash", i64::MAX)
        .await
        .expect("plant token row");
    let tools_list_calls_before = stub.methods().iter().filter(|m| *m == "tools/list").count();

    let (status, body) = post_json(
        &state,
        &format!("/api/plugins/{CONNECTOR_ID}/rotate-token"),
        json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "rotate-token on a connector must be a 4xx, got {status}: {body}"
    );

    assert_eq!(
        b.repo.plugin_token_get(CONNECTOR_ID).await.unwrap(),
        Some(("planted-hash".to_string(), i64::MAX)),
        "the token row must NOT have been deleted"
    );
    assert_eq!(
        stub.methods().iter().filter(|m| *m == "tools/list").count(),
        tools_list_calls_before,
        "no restart may have been triggered (a restart re-runs tools/list)"
    );
    let after = state.plugin.status(CONNECTOR_ID).await.expect("status");
    assert!(
        matches!(after.status, PluginRuntimeStatus::Running),
        "the connector must still be Running, got {:?}",
        after.status
    );
}

/// §4 #8, the fail-open hole: the kind guard used to be conditional on
/// `registry.get(id)` returning `Some`, so an id the registry does not know
/// fell straight THROUGH the guard into the token delete + restart — i.e. the
/// one case where the kind cannot be proven was also the case that got the
/// side effects. Acceptance §4 #8 requires no side effect.
#[tokio::test]
async fn rotate_token_with_no_registry_entry_has_no_side_effects() {
    let b = boot().await;
    let state = b.state(b.host());

    // A plugin ROW exists (so the route's own lookup succeeds) but the
    // registry does not know the id — uninstall-mid-flight, a manifest that
    // failed to load, a plugins_dir the operator moved.
    seed_row(&b, CONNECTOR_ID).await;
    b.repo
        .plugin_token_set(CONNECTOR_ID, "planted-hash", i64::MAX)
        .await
        .expect("plant token row");
    assert!(
        state.plugin.registry().get(CONNECTOR_ID).is_none(),
        "precondition: the registry must NOT know this id"
    );

    let (status, body) = post_json(
        &state,
        &format!("/api/plugins/{CONNECTOR_ID}/rotate-token"),
        json!({}),
    )
    .await;
    assert!(
        status.is_client_error(),
        "an unprovable kind must fail CLOSED, got {status}: {body}"
    );
    assert_eq!(
        b.repo.plugin_token_get(CONNECTOR_ID).await.unwrap(),
        Some(("planted-hash".to_string(), i64::MAX)),
        "the token row must NOT have been deleted"
    );
    assert!(
        state.plugin.status(CONNECTOR_ID).await.is_none(),
        "nothing may have been started"
    );
}

// ===========================================================================
// §4 #10 — a hung upstream does not block boot
// ===========================================================================

#[tokio::test]
async fn hung_upstream_lands_unavailable_without_blocking_boot() {
    let stub = StubServer::start(StubMode::Hang).await;
    let b = boot().await;
    // Short timeout so the test is fast; the production default is 10s.
    write_connector(&b.plugins_dir, &stub.url(), 400, 0o600);
    // Plus a healthy app plugin, to prove boot completes for everyone else.
    write_app_plugin(&b.plugins_dir, "app-echo");
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;
    seed_row(&b, "app-echo").await;

    let started = Instant::now();
    // This is the call `AppState::new` awaits INLINE. If it can hang, boot
    // hangs. The OUTER timeout is what makes this test able to fail fast
    // instead of wedging the suite; the elapsed assertion below is what makes
    // it able to fail at all.
    tokio::time::timeout(Duration::from_secs(5), host.autospawn_enabled())
        .await
        .expect("boot autospawn never returned against a hung upstream");
    let elapsed = started.elapsed();

    // 400 ms budget; 2 s allows for the app plugin's own spawn plus scheduling
    // slack, and still fails if the connector pays its budget twice (once for
    // `initialize`, once for `tools/list`) — which is exactly what the single
    // outer `tokio::time::timeout` in `spawn_mcp_http` prevents.
    assert!(
        elapsed < Duration::from_secs(2),
        "boot autospawn took {elapsed:?} against a hung upstream with a 400ms \
         budget — the bring-up is not bounded as ONE total wall-clock timeout"
    );
    // `None` is NOT acceptable: a connector that failed must be observable, or
    // `GET /api/plugins/{id}` reports it as never-enabled with no last_error.
    let status = host
        .status(CONNECTOR_ID)
        .await
        .expect("a hung upstream must leave an observable runtime entry");
    let PluginRuntimeStatus::Unavailable { reason } = &status.status else {
        panic!("connector must be Unavailable, got {:?}", status.status);
    };
    assert!(
        reason.contains("tools/list") || reason.contains("timed out"),
        "reason must say what failed: {reason}"
    );
    assert!(!host.running_plugin_ids().await.contains(CONNECTOR_ID));

    // The rest of boot happened.
    assert!(
        host.running_plugin_ids().await.contains("app-echo"),
        "one unreachable connector must not stop other plugins from starting"
    );
}

// ===========================================================================
// §4 #11 — uninstall completing during an in-flight spawn must not resurrect
// ===========================================================================

#[tokio::test]
async fn uninstall_during_an_in_flight_spawn_does_not_resurrect_the_registry_entry() {
    let (release, gate) = oneshot::channel::<()>();
    let stub = StubServer::start_gated(StubMode::Normal, Some(gate)).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;
    assert!(host.registry().get(CONNECTOR_ID).is_some());

    // Start the spawn; it will block inside `tools/list`.
    let spawn_host = Arc::clone(&host);
    let spawning = tokio::spawn(async move { spawn_host.spawn(CONNECTOR_ID).await });

    stub.wait_for_tools_list().await;

    // ... and while it is on the wire, uninstall lands. `uninstall_plugin`
    // does `stop()` (NotFound for a spawning id — treated as benign), deletes
    // the DB row, then removes the registry entry. The in-flight spawn keeps
    // going: there is no per-plugin lifecycle lock (risk R12).
    host.registry().remove(CONNECTOR_ID);
    assert!(host.registry().get(CONNECTOR_ID).is_none());

    let _ = release.send(());
    let outcome = spawning.await.unwrap();

    assert!(
        outcome.is_err(),
        "the spawn must not report success after its manifest was uninstalled"
    );
    assert!(
        host.registry().get(CONNECTOR_ID).is_none(),
        "the uninstalled manifest must NOT be resurrected into the registry \
         by the in-flight spawn's materialization (§2.7(3))"
    );
    assert!(host.registry().is_empty());
    assert!(
        host.status(CONNECTOR_ID).await.is_none(),
        "no live/reserved entry may survive the abandoned spawn"
    );
    assert!(!host.running_plugin_ids().await.contains(CONNECTOR_ID));
}

// ===========================================================================
// §2.6 — the misleading 404 on card creation via a connector tool
// ===========================================================================

#[tokio::test]
async fn connector_card_creation_is_a_4xx_that_names_the_real_reason() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;
    host.spawn(CONNECTOR_ID).await.expect("connector spawns");
    let wave_id = b.seed_wave().await;
    let state = b.state(Arc::clone(&host));

    // Drive the REAL route. Asserting only on the two accessors would pass
    // unchanged if `routes/cards.rs` were reverted to the misleading 404.
    let resp = cards_app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/waves/{wave_id}/cards"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "via_tool_call": {
                            "plugin_id": CONNECTOR_ID,
                            "tool_name": ALLOWED_TOOL,
                            "arguments": {}
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a running connector must NOT get the `not running` 404: {body}"
    );
    assert!(
        body.contains("connector") && body.contains("mcp-http"),
        "the message must name the real reason (wrong KIND, not `not running`): {body}"
    );
    assert!(
        !body.contains("not running"),
        "telling an operator a demonstrably-Running connector is not running \
         sends them to debug the wrong thing: {body}"
    );
    assert!(
        state.repo.cards_by_wave(&wave_id).await.unwrap().is_empty(),
        "no card may have been written"
    );

    // A genuinely-absent plugin still gets the 404 — the two cases must not
    // have collapsed into one.
    let resp = cards_app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/waves/{wave_id}/cards"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "via_tool_call": {
                            "plugin_id": "nope", "tool_name": "t", "arguments": {}
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // The seam the route consults, asserted directly as well: `mcp_client`
    // (stdio-only) says None while `connector_client` says "yes, mcp-http".
    assert!(
        host.mcp_client(CONNECTOR_ID).await.is_none(),
        "mcp_client() must narrow to (Running, Stdio)"
    );
    assert_eq!(
        host.connector_client(CONNECTOR_ID)
            .await
            .expect("connector is running")
            .variant_name(),
        "mcp-http"
    );
    assert!(host.mcp_client("nope").await.is_none());
    assert!(host.connector_client("nope").await.is_none());
}

// ===========================================================================
// §2.5 — `neige.*` callbacks are refused for connectors
// ===========================================================================

#[tokio::test]
async fn neige_callbacks_are_refused_for_connectors() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;
    host.spawn(CONNECTOR_ID).await.expect("connector spawns");

    let err = host
        .dispatch_neige_callback(CONNECTOR_ID, "neige.kv.get", json!({ "key": "k" }), None)
        .await
        .expect_err("connectors have no neige.* channel");
    assert_eq!(err.code, -32002);
    assert!(
        err.message.contains("mcp-http"),
        "the refusal must name the kind: {}",
        err.message
    );
}

// ---------------------------------------------------------------------------

/// Seed the `plugins` row the install route would have written (FK target for
/// `plugin_tokens`, and the `enabled` flag `autospawn_enabled` reads).
async fn seed_row(b: &Boot, id: &str) {
    b.repo
        .plugin_install(calm_server::model::NewPlugin {
            id: id.into(),
            version: "0.1.0".into(),
            install_path: b.plugins_dir.join(id).display().to_string(),
            manifest: json!({}),
            enabled: true,
            user_config: json!({}),
        })
        .await
        .expect("seed plugin row");
}
