//! External connector host (`kind: mcp-http` / `cli-query`) integration tests.

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
use calm_server::plugin_host::http_mcp::MAX_UPSTREAM_DETAIL_CHARS;
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
/// Underscores on purpose: the id↔tool boundary is `_`.
const ALLOWED_TOOL: &str = "list_institutional_reports";
const ALLOWED_TOOL_2: &str = "get_report_detail";
/// Served upstream but NOT in `tools_allow` — must never materialize.
const DENIED_TOOL: &str = "admin_purge";

/// How far past the truncation boundary the API key sits in the `EchoAuthIn4xx` fixture.
const KEY_STRADDLE_TAIL: usize = 16;

// Stub upstream MCP server, hand-rolled so it reproduces the exact recorded SSE wire shape.

#[derive(Clone, Copy, PartialEq)]
enum StubMode {
    /// Answer everything promptly.
    Normal,
    /// Accept the connection, read the request, then never write.
    Hang,
    /// Never answer `initialize` (best-effort), then answer `tools/list` promptly; must come up Running.
    HangInitialize,
    /// Answer every request with a 4xx whose body echoes the request's own `Authorization`
    /// header, padded so the key straddles the truncation boundary.
    EchoAuthIn4xx,
    /// Healthy bring-up, then a `tools/call` that takes [`SLOW_TOOLS_CALL`] to answer.
    SlowToolsCall,
    /// Healthy, but echoes the request's own `Authorization` header into every
    /// `tools/list` description and `tools/call` result.
    EchoAuthInResults,
    /// Split the catalog over two pages. The second page carries a tool that
    /// no first-page-only implementation can materialize.
    PaginatedTools,
    /// Return the same non-terminal cursor forever. A client must reject the
    /// loop rather than treating the first page as a complete catalog.
    RepeatedToolsCursor,
    /// Return a non-string pagination cursor. The MCP cursor is opaque text;
    /// coercing this value would invent a protocol the upstream did not send.
    InvalidToolsCursor,
    /// Return a fresh cursor forever. This bypasses repeated-cursor detection
    /// and therefore uniquely exercises the explicit page-count bound.
    EndlessToolsPagination,
}

/// How long [`StubMode::SlowToolsCall`] takes to answer a `tools/call`.
const SLOW_TOOLS_CALL: Duration = Duration::from_millis(1_500);

struct StubServer {
    addr: std::net::SocketAddr,
    /// Query strings seen.
    seen_queries: Arc<std::sync::Mutex<Vec<String>>>,
    /// `Authorization` header values seen, in order; empty string when absent.
    seen_auth: Arc<std::sync::Mutex<Vec<String>>>,
    seen_tenants: Arc<std::sync::Mutex<Vec<String>>>,
    /// `(JSON-RPC method, Authorization value)` per request, pushed together by the connection
    /// task; zipping `seen_methods` with `seen_auth` is not sound.
    seen_auth_by_method: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    /// Whole request targets (path AND query).
    seen_targets: Arc<std::sync::Mutex<Vec<String>>>,
    /// Methods seen, in order.
    seen_methods: Arc<std::sync::Mutex<Vec<String>>>,
    /// Set once `tools/list` has been received (used to line up the
    /// uninstall-vs-in-flight-spawn race deterministically).
    tools_list_received: Arc<AtomicBool>,
    _task: tokio::task::JoinHandle<()>,
}

/// A first-hop MCP endpoint that always redirects to another server.
struct RedirectServer {
    addr: std::net::SocketAddr,
    requests: Arc<std::sync::atomic::AtomicUsize>,
    _task: tokio::task::JoinHandle<()>,
}

impl RedirectServer {
    async fn start(location: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind redirect stub");
        let addr = listener.local_addr().expect("redirect stub address");
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&requests);

        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let location = location.clone();
                let seen = Arc::clone(&seen);
                tokio::spawn(async move {
                    if read_request(&mut sock).await.is_none() {
                        return;
                    }
                    seen.fetch_add(1, Ordering::SeqCst);
                    let response = format!(
                        "HTTP/1.1 302 Found\r\nlocation: {location}\r\n\
                         content-length: 0\r\nconnection: close\r\n\r\n"
                    );
                    let _ = sock.write_all(response.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });

        Self {
            addr,
            requests,
            _task: task,
        }
    }

    fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
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
        let seen_auth = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_tenants = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_auth_by_method = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_targets = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_methods = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tools_list_received = Arc::new(AtomicBool::new(false));

        let targets = Arc::clone(&seen_targets);
        let queries = Arc::clone(&seen_queries);
        let auths = Arc::clone(&seen_auth);
        let tenants = Arc::clone(&seen_tenants);
        let auth_by_method = Arc::clone(&seen_auth_by_method);
        let methods = Arc::clone(&seen_methods);
        let received = Arc::clone(&tools_list_received);
        // Connections are served concurrently: a mode that stalls one request must not stop the next connection.
        let gate = Arc::new(tokio::sync::Mutex::new(gate));

        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let queries = Arc::clone(&queries);
                let auths = Arc::clone(&auths);
                let tenants = Arc::clone(&tenants);
                let auth_by_method = Arc::clone(&auth_by_method);
                let targets = Arc::clone(&targets);
                let methods = Arc::clone(&methods);
                let received = Arc::clone(&received);
                let gate = Arc::clone(&gate);
                tokio::spawn(async move {
                    let (target, head, body) = match read_request(&mut sock).await {
                        Some(v) => v,
                        None => return,
                    };
                    let auth = header_value(&head, "authorization").unwrap_or_default();
                    auths.lock().unwrap().push(auth.clone());
                    let tenant = header_value(&head, "x-tenant").unwrap_or_default();
                    tenants.lock().unwrap().push(tenant.clone());
                    targets.lock().unwrap().push(target.clone());
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
                    auth_by_method
                        .lock()
                        .unwrap()
                        .push((method.clone(), auth.clone()));
                    let id = req.get("id").cloned().unwrap_or(json!(1));

                    if method == "tools/list" {
                        received.store(true, Ordering::SeqCst);
                        let taken = gate.lock().await.take();
                        if let Some(rx) = taken {
                            let _ = rx.await;
                        }
                    }

                    if mode == StubMode::EchoAuthIn4xx {
                        // Place the key so it starts `KEY_STRADDLE_TAIL` chars before the cap and runs past it.
                        let echoed = format!("Authorization: {auth}; tenant={tenant}");
                        let key_at = echoed.find(SECRET_VALUE).unwrap_or(0);
                        let pad = MAX_UPSTREAM_DETAIL_CHARS - KEY_STRADDLE_TAIL - key_at;
                        let body = format!("{}{echoed} rejected", "x".repeat(pad));
                        let head = format!(
                            "HTTP/1.1 400 Bad Request\r\ncontent-type: text/plain\r\n\
                             content-length: {}\r\nconnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = sock.write_all(head.as_bytes()).await;
                        let _ = sock.write_all(body.as_bytes()).await;
                        let _ = sock.flush().await;
                        return;
                    }

                    if mode == StubMode::Hang
                        || (mode == StubMode::HangInitialize && method == "initialize")
                    {
                        // Hold the socket open forever without writing. Dropping
                        // the task at test teardown closes it.
                        std::future::pending::<()>().await;
                    }

                    if mode == StubMode::SlowToolsCall && method == "tools/call" {
                        sleep(SLOW_TOOLS_CALL).await;
                    }

                    let echo = if mode == StubMode::EchoAuthInResults {
                        format!(" [upstream saw Authorization: {auth}; tenant={tenant}]")
                    } else {
                        String::new()
                    };

                    let result = match method.as_str() {
                        "initialize" => json!({
                            "protocolVersion": "2025-06-18",
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": format!("stub-mcp{echo}"), "version": "0.8.4" }
                        }),
                        "tools/list" if mode == StubMode::PaginatedTools => {
                            match req.pointer("/params/cursor").and_then(Value::as_str) {
                                None => json!({
                                    "tools": [{
                                        "name": ALLOWED_TOOL,
                                        "description": "first page",
                                        "inputSchema": { "type": "object" }
                                    }],
                                    "nextCursor": "page-2"
                                }),
                                Some("page-2") => json!({ "tools": [{
                                    "name": ALLOWED_TOOL_2,
                                    "description": "second page",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": { "report_id": { "type": "string" } }
                                    },
                                    "annotations": { "readOnlyHint": true }
                                }] }),
                                other => json!({ "unexpectedCursor": other }),
                            }
                        }
                        "tools/list" if mode == StubMode::RepeatedToolsCursor => json!({
                            "tools": [{ "name": ALLOWED_TOOL, "inputSchema": { "type": "object" } }],
                            "nextCursor": "again"
                        }),
                        "tools/list" if mode == StubMode::InvalidToolsCursor => json!({
                            "tools": [{ "name": ALLOWED_TOOL, "inputSchema": { "type": "object" } }],
                            "nextCursor": { "page": 2 }
                        }),
                        "tools/list" if mode == StubMode::EndlessToolsPagination => {
                            let page = req
                                .pointer("/params/cursor")
                                .and_then(Value::as_str)
                                .and_then(|cursor| cursor.strip_prefix("page-"))
                                .and_then(|number| number.parse::<usize>().ok())
                                .unwrap_or(0);
                            json!({
                                "tools": [],
                                "nextCursor": format!("page-{}", page + 1)
                            })
                        }
                        "tools/list" => json!({ "tools": [
                            { "name": ALLOWED_TOOL,
                              "description": format!("institutional reports{echo}"),
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
                                "content": [{ "type": "text",
                                              "text": format!("rows for {called}{echo}") }],
                                "structuredContent": { "rows": 3, "tool": called },
                                "isError": false
                            })
                        }
                        _ => json!({}),
                    };
                    let payload =
                        json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
                    let sse = format!("event: message\ndata: {payload}\n\n");
                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n",
                        sse.len()
                    );
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(sse.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });

        Self {
            addr,
            seen_queries,
            seen_auth,
            seen_tenants,
            seen_auth_by_method,
            seen_targets,
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

    /// `Authorization` header values seen, in order.
    fn auth_headers(&self) -> Vec<String> {
        self.seen_auth.lock().unwrap().clone()
    }

    fn auth_by_method(&self) -> Vec<(String, String)> {
        self.seen_auth_by_method.lock().unwrap().clone()
    }

    fn targets(&self) -> Vec<String> {
        self.seen_targets.lock().unwrap().clone()
    }

    async fn wait_for_tools_list(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.tools_list_received.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "stub never saw tools/list");
            sleep(Duration::from_millis(5)).await;
        }
    }
}

/// Read one HTTP/1.1 request, returning `(request-target, head, body)`.
async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, String, String)> {
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
        head,
        String::from_utf8_lossy(&buf[head_end..head_end + len]).to_string(),
    ))
}

/// Case-insensitive lookup of one header value in a raw HTTP/1.1 head.
fn header_value(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_string())
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The two budgets a connector manifest carries; they are separate constraints.
#[derive(Clone, Copy)]
struct Budgets {
    /// `mcp_http.request_timeout_ms` — the steady-state `tools/call` budget.
    call_ms: u64,
    /// `mcp_http.bringup_timeout_ms`. `None` ⇒ omit the field, exercising the
    /// derived `min(call_ms, ceiling)` default.
    bringup_ms: Option<u64>,
}

impl Budgets {
    fn uniform(ms: u64) -> Self {
        Self {
            call_ms: ms,
            bringup_ms: None,
        }
    }
}

fn connector_manifest_json(url: &str, budgets: Budgets) -> Value {
    let mut m = connector_manifest_base(url, budgets.call_ms);
    if let Some(ms) = budgets.bringup_ms {
        m["mcp_http"]["bringup_timeout_ms"] = json!(ms);
    }
    m
}

fn connector_manifest_base(url: &str, timeout_ms: u64) -> Value {
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
            "api_key_in": "bearer",
            "tools_allow": [ALLOWED_TOOL, ALLOWED_TOOL_2],
            "request_timeout_ms": timeout_ms,
        }
    })
}

/// Write a connector directory INSIDE `plugins_dir`, so install hits the `src == dst`
/// short-circuit and lands a real directory.
fn write_connector(plugins_dir: &Path, url: &str, timeout_ms: u64, secret_mode: u32) -> PathBuf {
    write_connector_with(plugins_dir, url, Budgets::uniform(timeout_ms), secret_mode)
}

fn write_connector_with(
    plugins_dir: &Path,
    url: &str,
    budgets: Budgets,
    secret_mode: u32,
) -> PathBuf {
    let dir = plugins_dir.join(CONNECTOR_ID);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&connector_manifest_json(url, budgets)).unwrap(),
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

fn write_all_tools_connector(plugins_dir: &Path, url: &str) -> PathBuf {
    let dir = write_connector(plugins_dir, url, 5_000, 0o600);
    let mut manifest = connector_manifest_json(url, Budgets::uniform(5_000));
    manifest["mcp_http"]
        .as_object_mut()
        .unwrap()
        .remove("tools_allow");
    manifest["mcp_http"]["tools_all"] = json!(true);
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
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

struct Boot {
    repo: Arc<dyn Repo>,
    /// The same store as [`Self::repo`], typed, so a test can break it on purpose.
    sqlx: Arc<SqlxRepo>,
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
    let sqlx = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx.clone();
    Boot {
        repo,
        sqlx,
        plugins_dir,
        plugins_data_dir,
        events: EventBus::new(),
        _tmp: tmp,
    }
}

impl Boot {
    /// Build a `PluginHost` hydrated from disk, like boot; calling twice over the same
    /// `plugins_dir` + repo simulates a restart.
    fn host(&self) -> Arc<PluginHost> {
        self.host_with_disabled(Vec::new())
    }

    /// [`Self::host`] with `config.plugins_disabled` populated.
    fn host_with_disabled(&self, plugins_disabled: Vec<String>) -> Arc<PluginHost> {
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
            plugins_disabled,
            self.events.clone(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
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
    /// One area + one track, so a test can drive `POST /api/tracks/{id}/cards`.
    async fn seed_track(&self) -> String {
        let area = self
            .repo
            .area_create(calm_server::model::NewArea {
                name: "demo".into(),
                color: "#fff".into(),
                sort: None,
            })
            .await
            .unwrap();
        self.repo
            .track_create(calm_server::model::NewTrack {
                template_input: None,
                area_id: area.id.clone(),
                title: "demo".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
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

#[tokio::test]
async fn connector_installs_enables_and_stays_running_across_restart() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    let dir = write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);

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
    assert!(
        body.get("pid").map(|p| p.is_null()).unwrap_or(true),
        "connector must not report a pid: {body}"
    );

    // Simulated full service restart: fresh host + registry from disk, same repo.
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

#[tokio::test]
async fn default_all_tools_install_discovers_every_page_and_survives_restart() {
    let stub = StubServer::start(StubMode::PaginatedTools).await;
    let b = boot().await;
    let host = b.host();
    let state = b.state(Arc::clone(&host));

    let (status, installed) = post_json(
        &state,
        "/api/plugins/install",
        json!({ "source": {
            "kind": "mcp_http_v2",
            "id": CONNECTOR_ID,
            "display_name": "Paginated MCP",
            "tools_all": true,
            "url": stub.url()
        }}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "install failed: {installed}");
    assert_eq!(installed["manifest"]["mcp_http"]["tools_all"], true);
    assert_eq!(
        installed["manifest"]["mcp_http"]["tools_allow"],
        json!([]),
        "the decoded manifest may publish its default empty allowlist, but tools_all is the explicit authority bit"
    );

    let (status, enabled) = post_json(
        &state,
        &format!("/api/plugins/{CONNECTOR_ID}/enable"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "enable failed: {enabled}");

    let manifest = host.registry().get(CONNECTOR_ID).expect("registry entry");
    assert_eq!(
        manifest
            .exposes_tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec![ALLOWED_TOOL, ALLOWED_TOOL_2],
        "a later page must not be silently omitted"
    );
    let later = manifest
        .exposes_tools
        .iter()
        .find(|tool| tool.name == ALLOWED_TOOL_2)
        .expect("later-page tool");
    assert!(later.kind.is_none(), "remote tools are never forge actions");
    assert_eq!(
        later
            .input_schema
            .as_ref()
            .and_then(|schema| schema.pointer("/properties/report_id/type")),
        Some(&json!("string"))
    );
    assert_eq!(later.annotations, Some(json!({ "readOnlyHint": true })));

    let ConnectorClient::Http(client) = host
        .connector_client(CONNECTOR_ID)
        .await
        .expect("running connector client")
    else {
        panic!("expected the http connector client");
    };
    let called = client
        .tools_call(ALLOWED_TOOL_2, json!({ "report_id": "r-1" }))
        .await
        .expect("later-page tool remains callable");
    assert!(
        serde_json::to_string(&called)
            .unwrap()
            .contains(ALLOWED_TOOL_2),
        "the upstream must receive the later-page tool call: {called:?}"
    );

    let host2 = b.host();
    host2.autospawn_enabled().await;
    let after = host2
        .registry()
        .get(CONNECTOR_ID)
        .expect("registry after restart");
    assert_eq!(
        after.mcp_http.as_ref().map(|block| block.tools_all),
        Some(true)
    );
    assert_eq!(
        after.exposes_tools.len(),
        2,
        "restart must rediscover every page"
    );
}

#[tokio::test]
async fn repeated_tools_cursor_fails_closed_instead_of_publishing_a_partial_catalog() {
    let stub = StubServer::start(StubMode::RepeatedToolsCursor).await;
    let b = boot().await;
    write_all_tools_connector(&b.plugins_dir, &stub.url());
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    let err = host
        .spawn(CONNECTOR_ID)
        .await
        .expect_err("a cursor loop must make the connector unavailable");
    assert!(
        err.to_string().contains("nextCursor") && err.to_string().contains("repeat"),
        "{err}"
    );
    assert!(!host.running_plugin_ids().await.contains(CONNECTOR_ID));
    assert!(
        host.registry()
            .get(CONNECTOR_ID)
            .is_some_and(|manifest| manifest.exposes_tools.is_empty()),
        "the first page is not a complete catalog and must not be published"
    );
}

#[tokio::test]
async fn non_string_tools_cursor_fails_closed_instead_of_ending_pagination() {
    let stub = StubServer::start(StubMode::InvalidToolsCursor).await;
    let b = boot().await;
    write_all_tools_connector(&b.plugins_dir, &stub.url());
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    let err = host
        .spawn(CONNECTOR_ID)
        .await
        .expect_err("an invalid cursor must not be mistaken for end-of-list");
    assert!(
        err.to_string().contains("nextCursor") && err.to_string().contains("string"),
        "{err}"
    );
    assert!(!host.running_plugin_ids().await.contains(CONNECTOR_ID));
}

#[tokio::test]
async fn endlessly_fresh_tools_cursors_hit_the_page_cap_before_publish() {
    let stub = StubServer::start(StubMode::EndlessToolsPagination).await;
    let b = boot().await;
    write_all_tools_connector(&b.plugins_dir, &stub.url());
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    let err = host
        .spawn(CONNECTOR_ID)
        .await
        .expect_err("fresh cursors forever must still be bounded");
    assert!(
        err.to_string().contains("catalog exceeded") && err.to_string().contains("pages"),
        "the page cap, not partial success, must end the catalog: {err}"
    );
    assert!(!host.running_plugin_ids().await.contains(CONNECTOR_ID));
}

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

    // Assert on the header VALUE, paired with the method: bring-up's two requests alone
    // would satisfy an `any`.
    let by_method = stub.auth_by_method();
    let expected = format!("Bearer {SECRET_VALUE}");
    for (method, auth) in &by_method {
        assert_eq!(
            auth, &expected,
            "every request must carry `Bearer <key>`; `{method}` did not: {by_method:?}"
        );
    }
    assert!(
        by_method.iter().any(|(m, _)| m == "tools/call"),
        "the `tools/call` phase must be covered by the assertion above: {by_method:?}"
    );
    assert!(
        stub.targets().iter().all(|t| !t.contains(SECRET_VALUE)),
        "the credential must not reach the request target: {:?}",
        stub.targets()
    );
    assert!(
        stub.queries().iter().all(|q| q.is_empty()),
        "nothing may be appended to the operator's url: {:?}",
        stub.queries()
    );

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

/// An upstream 302 must not redirect a keyed connector to another host: ureq preserves
/// custom headers such as `X-API-Key` while following redirects.
#[tokio::test]
async fn an_upstream_redirect_cannot_send_a_header_api_key_to_another_host() {
    let sink = StubServer::start(StubMode::Normal).await;
    let redirector = RedirectServer::start(sink.url()).await;
    let b = boot().await;
    let dir = write_connector(&b.plugins_dir, &redirector.url(), 5_000, 0o600);

    // This is the vulnerable placement: ureq already special-cases
    // `Authorization`, but not a manifest-selected custom header name.
    let mut manifest = connector_manifest_json(&redirector.url(), Budgets::uniform(5_000));
    manifest["mcp_http"]["api_key_in"] = json!("header:X-API-Key");
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;
    let err = host
        .spawn(CONNECTOR_ID)
        .await
        .expect_err("a redirecting MCP endpoint must fail closed");
    assert!(
        matches!(err, HostError::ConnectorUnavailable { .. }),
        "got {err:?}"
    );

    assert!(
        redirector.request_count() > 0,
        "the first hop must be contacted or the zero-request sink assertion is vacuous"
    );
    assert!(
        sink.methods().is_empty(),
        "the redirect target must receive zero requests, got {:?}",
        sink.methods()
    );

    let status = host
        .status(CONNECTOR_ID)
        .await
        .expect("a refused redirect must leave an observable runtime status");
    let PluginRuntimeStatus::Unavailable { reason } = &status.status else {
        panic!("expected Unavailable, got {:?}", status.status);
    };
    assert!(
        !reason.contains(SECRET_VALUE),
        "last_error leaked the key: {reason}"
    );
    assert!(!host.running_plugin_ids().await.contains(CONNECTOR_ID));
}

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

/// The key must appear in none of the three sinks a `ureq` transport error reaches;
/// `ureq::Error`'s `Display` prints the full URL, so it is only ever formatted via `kind()`.
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

        // Sink 1: the `/enable` response body, read as raw text.
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

        // Sink 4: a `tools_call` failure, which becomes track transcript text.
        let manifest = state.plugin.registry().get(CONNECTOR_ID).unwrap();
        let credential = calm_server::plugin_host::HttpCredential::parse(SECRET_VALUE)
            .expect("the fixture credential must satisfy the HTTP-credential rules");
        let block = manifest.mcp_http.as_ref().unwrap();
        let url = calm_server::plugin_host::manifest::resolve_mcp_http_url(
            block,
            &serde_json::Map::new(),
        )
        .expect("the fixture url resolves");
        let client = calm_server::plugin_host::HttpMcpClient::new(
            CONNECTOR_ID,
            &url,
            block,
            Some(&credential),
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
        // And a `Debug` of the client itself.
        let dbg = format!("{client:?}");
        assert!(!dbg.contains(SECRET_VALUE), "{label}: {dbg}");

        // The failures must still SAY something useful.
        assert!(
            reason.contains("127.0.0.1") || reason.contains("timed out"),
            "{label}: reason must remain diagnosable: {reason}"
        );
    }
}

// `cli-query` execution: the connector under test is a script the test writes and pins by absolute path.

const CLI_ID: &str = "cli-longbridge";
const CLI_TOOL: &str = "quote";

/// Write an executable script and return its absolute path.
fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// Install a `cli-query` connector directory inside `plugins_dir`.
fn write_cli_connector(plugins_dir: &Path, command: &str, args: &[&str]) -> PathBuf {
    let dir = plugins_dir.join(CLI_ID);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        json!({
            "manifest_version": 1,
            "kind": "cli-query",
            "id": CLI_ID,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Longbridge",
            "cli_query": {
                "command": command,
                "timeout_ms": 5_000,
                "tools": [{
                    "name": CLI_TOOL,
                    "description": "Get a quote",
                    "input_schema": {
                        "type": "object",
                        "properties": { "symbol": { "type": "string" } },
                        "required": ["symbol"],
                        "additionalProperties": false
                    },
                    "args": args
                }]
            }
        })
        .to_string(),
    )
    .unwrap();
    dir
}

#[tokio::test]
async fn cli_query_installs_and_enables_and_publishes_its_tool() {
    let b = boot().await;
    let bin = tempfile::tempdir().unwrap();
    let script = write_script(bin.path(), "lb.sh", "#!/bin/sh\necho \"quote:$1\"\n");
    let dir = write_cli_connector(
        &b.plugins_dir,
        &script.display().to_string(),
        &["quote", "{{symbol}}"],
    );

    let host = b.host();
    let state = b.state(Arc::clone(&host));
    let (status, body) = post_json(
        &state,
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": dir.display().to_string() } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "install failed: {body}");

    let (status, body) =
        post_json(&state, &format!("/api/plugins/{CLI_ID}/enable"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "enable failed: {body}");

    let st = state
        .plugin
        .status(CLI_ID)
        .await
        .expect("observable status");
    assert!(
        matches!(st.status, PluginRuntimeStatus::Running),
        "got {:?}",
        st.status
    );
    assert!(state.plugin.running_plugin_ids().await.contains(CLI_ID));

    // The tool materialized BEFORE the live insert.
    let order = state
        .plugin
        .connector_spawn_order(CLI_ID)
        .expect("a connector spawn must record its ordering");
    assert!(
        order.materialized_before_live_insert(),
        "materialization must precede the live insert: {order:?}"
    );

    let tools = boot_audit_tool_names(&state.plugin).await;
    assert!(
        tools.contains(&format!("{CLI_ID}::{CLI_TOOL}")),
        "the declared tool must be visible: {tools:?}"
    );

    // cli-query is read-only by contract, so every published tool carries `readOnlyHint: true`;
    // Codex under `approval_policy: never` refuses tools with no annotations.
    let manifest = host.registry().get(CLI_ID).expect("registry entry");
    assert!(
        !manifest.exposes_tools.is_empty(),
        "the annotation check must not be vacuous"
    );
    for tool in &manifest.exposes_tools {
        assert_eq!(
            tool.annotations,
            Some(json!({ "readOnlyHint": true })),
            "cli-query tool `{}` must publish readOnlyHint: true; without it \
             (annotations: None) Codex under approval_policy: never refuses the call (#1744)",
            tool.name
        );
    }

    let client = state
        .plugin
        .connector_client(CLI_ID)
        .await
        .expect("running connector must expose a client");
    let ConnectorClient::Cli(cli) = &client else {
        panic!("expected a Cli connector client, got {client:?}");
    };
    assert_eq!(cli.program(), script.as_path());
    assert!(cli.program().is_absolute());
    // `mcp_client()` stays stdio-only: a cli-query connector is not an app.
    assert!(state.plugin.mcp_client(CLI_ID).await.is_none());
    assert_eq!(client.variant_name(), "cli-query");
}

#[tokio::test]
async fn cli_query_tools_call_runs_the_binary_and_returns_its_stdout() {
    let b = boot().await;
    let bin = tempfile::tempdir().unwrap();
    // Prints each argv element on its own line, so the test sees exactly how the template was rendered.
    let script = write_script(
        bin.path(),
        "argv.sh",
        "#!/bin/sh\nfor a in \"$@\"; do echo \"arg:$a\"; done\n",
    );
    write_cli_connector(
        &b.plugins_dir,
        &script.display().to_string(),
        &["quote", "{{symbol}}"],
    );
    let host = b.host();
    seed_row(&b, CLI_ID).await;
    host.spawn(CLI_ID)
        .await
        .expect("cli-query connector spawns");

    let client = host.connector_client(CLI_ID).await.expect("client");
    let ConnectorClient::Cli(cli) = &client else {
        panic!("expected a Cli client, got {client:?}");
    };

    // A value full of shell metacharacters, to prove there is no shell.
    let symbol = "700.HK; rm -rf / && echo $HOME";
    let res = cli
        .tools_call(CLI_TOOL, json!({ "symbol": symbol }))
        .await
        .expect("tools/call");
    assert_eq!(res.is_error, Some(false));
    let text = res.content[0].text.clone().unwrap();
    assert_eq!(
        text,
        format!("arg:quote\narg:{symbol}\n"),
        "the slot must render as exactly one argv element"
    );

    // Unknown argument keys are ignored, not an error (v0 does no full
    // JSON-Schema validation).
    let res = cli
        .tools_call(CLI_TOOL, json!({ "symbol": "X", "extra": "Y" }))
        .await
        .expect("tools/call");
    assert_eq!(res.content[0].text.as_deref(), Some("arg:quote\narg:X\n"));

    // A missing required slot is a refusal that names it — never an empty argv
    // element handed to the binary.
    let err = cli
        .tools_call(CLI_TOOL, json!({}))
        .await
        .expect_err("a missing slot must be refused");
    assert!(err.message.contains("symbol"), "{}", err.message);
}

/// A non-zero exit is the child's verdict, not a transport failure: `isError: true` plus the output.
#[tokio::test]
async fn cli_query_non_zero_exit_is_is_error_true_with_the_output() {
    let b = boot().await;
    let bin = tempfile::tempdir().unwrap();
    let script = write_script(
        bin.path(),
        "fail.sh",
        "#!/bin/sh\necho 'partial rows'\necho 'upstream refused' >&2\nexit 7\n",
    );
    write_cli_connector(
        &b.plugins_dir,
        &script.display().to_string(),
        &["quote", "{{symbol}}"],
    );
    let host = b.host();
    seed_row(&b, CLI_ID).await;
    host.spawn(CLI_ID).await.expect("connector spawns");

    let client = host.connector_client(CLI_ID).await.expect("client");
    let ConnectorClient::Cli(cli) = &client else {
        panic!("expected a Cli client, got {client:?}");
    };
    let res = cli
        .tools_call(CLI_TOOL, json!({ "symbol": "X" }))
        .await
        .expect("a failing command must NOT surface as a transport error");
    assert_eq!(res.is_error, Some(true));
    let joined: String = res
        .content
        .iter()
        .filter_map(|c| c.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("partial rows"), "{joined}");
    assert!(joined.contains("upstream refused"), "{joined}");
    assert!(
        joined.contains('7'),
        "the exit status must be reported: {joined}"
    );

    // The connector stays Running: one failing query is not a dead connector.
    assert!(host.running_plugin_ids().await.contains(CLI_ID));
}

/// An unresolvable bare command is a 503 whose reason names the service PATH and the directories searched.
#[tokio::test]
async fn cli_query_unresolvable_command_is_a_503_naming_the_path() {
    let b = boot().await;
    let dir = write_cli_connector(
        &b.plugins_dir,
        "definitely-not-a-real-binary-1164",
        &["quote", "{{symbol}}"],
    );

    let state = b.state(b.host());
    let (status, body) = post_json(
        &state,
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": dir.display().to_string() } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "install failed: {body}");

    let (status, body) =
        post_json(&state, &format!("/api/plugins/{CLI_ID}/enable"), json!({})).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "an unresolvable command must be a 503, got {status}: {body}"
    );
    let rendered = body.to_string();
    assert!(
        rendered.contains("PATH"),
        "the reason must name PATH: {rendered}"
    );
    assert!(
        rendered.contains("definitely-not-a-real-binary-1164"),
        "the reason must name the command: {rendered}"
    );
    let service_path = std::env::var("PATH").unwrap_or_default();
    let first_dir = service_path
        .split(':')
        .find(|s| !s.is_empty())
        .unwrap_or("/");
    assert!(
        rendered.contains(first_dir),
        "the reason must list the directories searched ({first_dir}): {rendered}"
    );

    let st = state
        .plugin
        .status(CLI_ID)
        .await
        .expect("observable status");
    assert!(
        matches!(st.status, PluginRuntimeStatus::Unavailable { .. }),
        "got {:?}",
        st.status
    );
    assert!(!state.plugin.running_plugin_ids().await.contains(CLI_ID));
}

#[tokio::test]
async fn cli_query_missing_secret_is_a_503_that_names_the_key_and_the_file() {
    let b = boot().await;
    let bin = tempfile::tempdir().unwrap();
    let script = write_script(bin.path(), "lb.sh", "#!/bin/sh\necho hi\n");
    let dir = b.plugins_dir.join(CLI_ID);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        json!({
            "manifest_version": 1,
            "kind": "cli-query",
            "id": CLI_ID,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Longbridge",
            "cli_query": {
                "command": script.display().to_string(),
                "secret_env": ["LB_TOKEN"],
                "tools": [{
                    "name": CLI_TOOL,
                    "input_schema": { "type": "object", "properties": {} },
                    "args": ["quote"]
                }]
            }
        })
        .to_string(),
    )
    .unwrap();

    let state = b.state(b.host());
    post_json(
        &state,
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": dir.display().to_string() } }),
    )
    .await;
    let (status, body) =
        post_json(&state, &format!("/api/plugins/{CLI_ID}/enable"), json!({})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let rendered = body.to_string();
    assert!(rendered.contains("LB_TOKEN"), "{rendered}");
    assert!(rendered.contains("secrets.json"), "{rendered}");
}

/// `neige.*` callbacks and forge dispatch stay app-only for `cli-query` too.
#[tokio::test]
async fn cli_query_connectors_are_refused_app_only_surfaces() {
    let b = boot().await;
    let bin = tempfile::tempdir().unwrap();
    let script = write_script(bin.path(), "lb.sh", "#!/bin/sh\necho hi\n");
    write_cli_connector(&b.plugins_dir, &script.display().to_string(), &["quote"]);
    let host = b.host();
    seed_row(&b, CLI_ID).await;
    host.spawn(CLI_ID).await.expect("connector spawns");

    let err = host
        .dispatch_neige_callback(CLI_ID, "neige.overlay.set", json!({}), None)
        .await
        .expect_err("neige.* must be refused for a cli-query connector");
    assert_eq!(err.code, -32002, "{err:?}");
    assert!(
        err.message.contains("cli-query"),
        "the refusal must name the KIND: {}",
        err.message
    );

    // `rotate-token` keeps its non-app 4xx: a connector has no plugin token.
    let state = b.state(Arc::clone(&host));
    let (status, _) = post_json(
        &state,
        &format!("/api/plugins/{CLI_ID}/rotate-token"),
        json!({}),
    )
    .await;
    assert!(
        status.is_client_error(),
        "rotate-token on a connector must be a 4xx, got {status}"
    );

    // …and a process-less connector reports an EMPTY stderr tail (the id is
    // live, so this is `Some(vec![])`, not `None`) and stops cleanly.
    assert_eq!(
        host.stderr_tail(CLI_ID, 10).await,
        Some(Vec::new()),
        "a connector has no child process, so it has no stderr"
    );
    host.stop(CLI_ID).await.expect("stop must succeed");
    assert!(!host.running_plugin_ids().await.contains(CLI_ID));
}

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
    // Boot autospawn swallows the error, so the runtime entry is an operator's only signal.
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

// Visibility keys off `status` alone; `running_plugin_ids` is the single gate both
// discovery and dispatch consult.

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

#[tokio::test]
async fn connector_tools_are_materialized_before_the_id_becomes_running() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    host.spawn(CONNECTOR_ID).await.expect("connector spawns");

    // Structural, not sampled: the two production steps are adjacent synchronous blocks, so
    // `connector_spawn_order` stamps a monotonic tick after each.
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

    assert!(host.running_plugin_ids().await.contains(CONNECTOR_ID));
    assert_eq!(
        host.registry()
            .get(CONNECTOR_ID)
            .map(|m| m.exposes_tools.len()),
        Some(2)
    );

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

    // Plant a token row so a stray delete would be observable.
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

/// An id the registry does not know must fail CLOSED, not fall through the kind guard
/// into the token delete + restart.
#[tokio::test]
async fn rotate_token_with_no_registry_entry_has_no_side_effects() {
    let b = boot().await;
    let state = b.state(b.host());

    // A plugin ROW exists but the registry does not know the id.
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
    // The exact code, not just "some 4xx".
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unprovable kind must fail CLOSED as a 404, got {status}: {body}"
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

/// `rotate-token` answers the documented status for the two HTTP-reachable cells, with
/// the ids on the operator's kill switch.
#[tokio::test]
async fn rotate_token_over_http_keeps_its_codes_for_ids_on_the_kill_switch() {
    const GHOST: &str = "test.rotate.ghost";

    let b = boot().await;
    // A registered connector. Never brought up: rotation refuses on `kind`
    // before any network contact, so an unroutable url is the honest fixture.
    write_connector(
        &b.plugins_dir,
        "http://127.0.0.1:1/never-contacted",
        1_000,
        0o600,
    );
    // …and an id with a `plugins` row but no manifest on disk.
    seed_row(&b, CONNECTOR_ID).await;
    seed_row(&b, GHOST).await;

    let host = b.host_with_disabled(vec![CONNECTOR_ID.to_string(), GHOST.to_string()]);
    assert!(
        host.registry().get(CONNECTOR_ID).is_some() && host.registry().get(GHOST).is_none(),
        "fixture: the connector must be registered and the ghost must not"
    );
    let state = b.state(host);

    // Token rows on both, so a stray delete anywhere is visible.
    for id in [CONNECTOR_ID, GHOST] {
        b.repo
            .plugin_token_set(id, "planted-hash", i64::MAX)
            .await
            .expect("plant token row");
    }

    let (status, body) = post_json(
        &state,
        &format!("/api/plugins/{GHOST}/rotate-token"),
        json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an id the registry does not know is a 404 even when it is also on the \
         kill switch — being config-disabled is not rotation's opening \
         question. Got {status}: {body}"
    );

    let (status, body) = post_json(
        &state,
        &format!("/api/plugins/{CONNECTOR_ID}/rotate-token"),
        json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "rotating a connector is a 400 even when it is also on the kill \
         switch. Got {status}: {body}"
    );

    // Both refusals precede the delete and the restart, so nothing moved.
    for id in [CONNECTOR_ID, GHOST] {
        assert_eq!(
            b.repo.plugin_token_get(id).await.unwrap(),
            Some(("planted-hash".to_string(), i64::MAX)),
            "{id}: the token row must NOT have been touched"
        );
        assert!(
            state.plugin.status(id).await.is_none(),
            "{id}: nothing may have been started"
        );
    }
}

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
    // This is the call `AppState::new` awaits inline; the outer timeout keeps a hang from wedging the suite.
    tokio::time::timeout(Duration::from_secs(5), host.autospawn_enabled())
        .await
        .expect("boot autospawn never returned against a hung upstream");
    let elapsed = started.elapsed();

    // 400 ms per request × 2 round trips + 500 ms slack; what this fails on is an unbounded bring-up.
    assert!(
        elapsed < Duration::from_secs(3),
        "boot autospawn took {elapsed:?} against a hung upstream with a 400ms \
         per-request budget — the bring-up is not bounded"
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

    assert!(
        host.running_plugin_ids().await.contains("app-echo"),
        "one unreachable connector must not stop other plugins from starting"
    );
}

/// A healthy upstream that merely stalls on `initialize` (best-effort) must still come up
/// Running; the outer bound must cover TWO round trips.
#[tokio::test]
async fn a_slow_but_healthy_upstream_still_comes_up_running() {
    let stub = StubServer::start(StubMode::HangInitialize).await;
    let b = boot().await;
    // `initialize` consumes this whole per-request budget; `tools/list` then answers immediately.
    let timeout_ms = 1_000;
    write_connector(&b.plugins_dir, &stub.url(), timeout_ms, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    host.spawn(CONNECTOR_ID).await.expect(
        "a healthy upstream that is merely slow on the best-effort \
                 `initialize` must still come up",
    );

    let status = host.status(CONNECTOR_ID).await.expect("runtime entry");
    assert!(
        matches!(status.status, PluginRuntimeStatus::Running),
        "expected Running, got {:?}",
        status.status
    );
    assert!(host.running_plugin_ids().await.contains(CONNECTOR_ID));
    // Both round trips really happened — otherwise this would pass for a
    // connector that never tried `initialize` at all.
    assert!(
        stub.methods().contains(&"initialize".to_string()),
        "methods: {:?}",
        stub.methods()
    );
    assert!(
        stub.methods().contains(&"tools/list".to_string()),
        "methods: {:?}",
        stub.methods()
    );
    let tools = boot_audit_tool_names(&host).await;
    assert!(tools.iter().any(|t| t.ends_with(ALLOWED_TOOL)), "{tools:?}");
}

/// Boot latency must not scale with the number of unreachable connectors.
#[tokio::test]
async fn many_unreachable_connectors_do_not_scale_boot_latency() {
    let stub = StubServer::start(StubMode::Hang).await;
    let b = boot().await;
    // 6 connectors × (2 × 900ms per-request + slack) would be well over 10s
    // serially; the overall budget must cut it far shorter than that.
    const N: usize = 6;
    let timeout_ms = 900;
    for i in 0..N {
        let id = format!("dead-connector-{i}");
        let dir = b.plugins_dir.join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut manifest = connector_manifest_json(&stub.url(), Budgets::uniform(timeout_ms));
        manifest["id"] = json!(id);
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let secrets = dir.join("secrets.json");
        std::fs::write(&secrets, json!({ SECRET_NAME: SECRET_VALUE }).to_string()).unwrap();
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
        seed_row(&b, &id).await;
    }
    let host = b.host();

    // Drive the real loop with a budget small enough to observe it firing; production supplies 30 s.
    let budget = Duration::from_secs(2);
    let started = Instant::now();
    tokio::time::timeout(
        Duration::from_secs(60),
        host.autospawn_enabled_within(budget),
    )
    .await
    .expect("boot autospawn never returned");
    let elapsed = started.elapsed();

    // 8 s fails loudly if the loop bound is removed and still passes with the per-connector bound doing its job.
    assert!(
        elapsed < Duration::from_secs(8),
        "boot autospawn took {elapsed:?} for {N} unreachable connectors with a \
         {budget:?} connector budget — the loop is not bounded as a whole"
    );
    // Every one of them is observable as a failure, not silently skipped —
    // including the ones that never got their turn.
    let mut budget_refusals = 0;
    for i in 0..N {
        let id = format!("dead-connector-{i}");
        let status = host
            .status(&id)
            .await
            .unwrap_or_else(|| panic!("{id} must leave an observable entry"));
        let PluginRuntimeStatus::Unavailable { reason } = &status.status else {
            panic!("{id}: {:?}", status.status);
        };
        if reason.contains("budget") {
            budget_refusals += 1;
        }
    }
    assert!(
        budget_refusals > 0,
        "with a {budget:?} budget and {N} hung connectors at least one must be \
         refused BY the budget — otherwise this test never exercised it"
    );
}

/// A lone connector whose own cap exceeds the loop budget must be refused for its own
/// upstream failure, with the same reason `/enable` gives.
#[tokio::test]
async fn boot_and_enable_agree_when_one_connector_outlasts_the_loop_budget() {
    let stub = StubServer::start(StubMode::Hang).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 400, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    // Smaller than this connector's own 1.3 s cap.
    tokio::time::timeout(
        Duration::from_secs(20),
        host.autospawn_enabled_within(Duration::from_millis(300)),
    )
    .await
    .expect("boot autospawn never returned");

    let boot_status = host.status(CONNECTOR_ID).await.expect("runtime entry");
    let PluginRuntimeStatus::Unavailable {
        reason: boot_reason,
    } = &boot_status.status
    else {
        panic!("expected Unavailable, got {:?}", boot_status.status);
    };
    assert!(
        !boot_reason.contains("budget"),
        "boot must refuse a lone connector for its own upstream failure, not \
         for a loop budget it is the only claimant on — and the reason may not \
         blame earlier connectors that do not exist: {boot_reason}"
    );
    assert!(
        boot_reason.contains("tools/list"),
        "the reason must name what actually failed: {boot_reason}"
    );

    // The same manifest through the enable path (no loop budget at all).
    let enable_reason = match host.spawn(CONNECTOR_ID).await {
        Err(HostError::ConnectorUnavailable { reason, .. }) => reason,
        other => panic!("expected ConnectorUnavailable, got {other:?}"),
    };
    assert_eq!(
        boot_reason, &enable_reason,
        "boot and enable must give the same answer for the same manifest"
    );
}

/// A boot budget elapsing after the live insert but before `emit_state(Running)` completes
/// must not overwrite the entry with `Unavailable`. The stub gates `tools/list` and the test
/// holds the repo's write transaction to park the spawn in exactly that window.
#[tokio::test]
async fn a_connector_that_came_up_is_not_overwritten_by_the_elapsing_boot_budget() {
    let (release_gate, gate) = oneshot::channel::<()>();
    let stub = StubServer::start_gated(StubMode::Normal, Some(gate)).await;
    let b = boot().await;
    // 3 s per bring-up request ⇒ a 6.5 s per-connector cap, which the loop budget below must
    // exceed; 3 s is flake headroom for the gate/lock sequence below, not milliseconds.
    write_connector_with(&b.plugins_dir, &stub.url(), Budgets::uniform(3_000), 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    // > (2 × 3 s + 500 ms slack) + 500 ms, so `autospawn_enabled_within` uses it verbatim rather than widening it.
    const BUDGET: Duration = Duration::from_millis(7_500);
    let loop_host = Arc::clone(&host);
    let autospawn = tokio::spawn(async move { loop_host.autospawn_enabled_within(BUDGET).await });

    // The connector has reached `tools/list`; its reply is held by the gate.
    stub.wait_for_tools_list().await;

    // Take the DB writer lock (`write_in_tx` opens BEGIN IMMEDIATE), so the
    // `emit_state(Running)` that follows the live insert cannot complete.
    let (held_tx, held_rx) = oneshot::channel::<()>();
    let (release_db, db_rx) = oneshot::channel::<()>();
    let repo = b.repo.clone();
    let holder = tokio::spawn(async move {
        repo.write_in_tx(Box::new(move |_tx| {
            Box::pin(async move {
                let _ = held_tx.send(());
                let _ = db_rx.await;
                Ok(())
            })
        }))
        .await
    });
    held_rx.await.expect("write tx never opened");

    // Let the spawn finish its network half. It will materialize, publish the
    // live `Running` entry, and then block on the event write.
    release_gate.send(()).expect("stub gate receiver gone");

    // Sit past the budget while the spawn is parked in that window.
    sleep(BUDGET + Duration::from_millis(750)).await;

    // The live entry must still be the successful one.
    let status = host
        .status(CONNECTOR_ID)
        .await
        .expect("connector must still have a runtime entry");
    assert!(
        matches!(status.status, PluginRuntimeStatus::Running),
        "an elapsed boot budget must not regress a connector that already came \
         up; got {:?}",
        status.status
    );
    assert!(
        host.running_plugin_ids().await.contains(CONNECTOR_ID),
        "a Running connector must stay in the running set — otherwise every \
         materialized tool silently disappears"
    );
    let tools = boot_audit_tool_names(&host).await;
    assert!(
        tools.iter().any(|t| t.ends_with(ALLOWED_TOOL)),
        "its materialized tools must stay visible: {tools:?}"
    );
    // The client survived too: `Unavailable` sets `mcp: None`, so this is the
    // load-bearing half of "the connector is still usable".
    let client = host
        .connector_client(CONNECTOR_ID)
        .await
        .expect("the live HTTP client must not have been dropped");
    assert!(matches!(client, ConnectorClient::Http(_)), "{client:?}");

    let _ = release_db.send(());
    let _ = holder.await;
    tokio::time::timeout(Duration::from_secs(30), autospawn)
        .await
        .expect("autospawn never returned")
        .expect("autospawn task panicked");

    let status = host.status(CONNECTOR_ID).await.expect("runtime entry");
    assert!(
        matches!(status.status, PluginRuntimeStatus::Running),
        "{:?}",
        status.status
    );
}

/// Boot must stay bounded however large the operator's `tools/call` budget is.
#[tokio::test]
async fn an_absurd_tools_call_budget_cannot_stall_boot() {
    let stub = StubServer::start(StubMode::Hang).await;
    let b = boot().await;
    write_connector_with(
        &b.plugins_dir,
        &stub.url(),
        Budgets {
            // Ten minutes per tool call — legal, and irrelevant to boot.
            call_ms: 600_000,
            // …while bring-up is what the boot path actually waits on.
            bringup_ms: Some(400),
        },
        0o600,
    );
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(20), host.autospawn_enabled())
        .await
        .expect("boot autospawn never returned; the bring-up bound is not independent");
    let elapsed = started.elapsed();

    // 400 ms × 2 + 500 ms slack = 1.3 s. Nothing about the 600 s call budget
    // may appear in this number.
    assert!(
        elapsed < Duration::from_secs(5),
        "boot took {elapsed:?} — the tools/call budget is leaking onto the boot path"
    );
    let status = host.status(CONNECTOR_ID).await.expect("observable entry");
    let PluginRuntimeStatus::Unavailable { reason } = &status.status else {
        panic!("expected Unavailable, got {:?}", status.status);
    };
    assert!(
        reason.contains("tools/list") || reason.contains("bringup_timeout_ms"),
        "the reason must name what failed: {reason}"
    );
}

/// No manifest that loads can make one connector's bring-up cap exceed
/// [`MAX_CONNECTOR_BRINGUP_BUDGET`]; drives the real `Manifest::parse` and `connector_bringup_budget`.
#[test]
fn no_loadable_manifest_can_exceed_the_bringup_cap() {
    use calm_server::plugin_host::manifest::{
        MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS as CEILING, Manifest,
    };
    use calm_server::plugin_host::{MAX_CONNECTOR_BRINGUP_BUDGET, connector_bringup_budget};

    let hostile = [
        json!({}),
        json!({ "request_timeout_ms": 600_000 }),
        json!({ "request_timeout_ms": u32::MAX }),
        json!({ "bringup_timeout_ms": CEILING }),
        json!({ "bringup_timeout_ms": CEILING, "request_timeout_ms": 600_000 }),
        json!({ "bringup_timeout_ms": 0, "request_timeout_ms": 600_000 }),
        // …and the values a validator must REFUSE outright.
        json!({ "bringup_timeout_ms": CEILING + 1 }),
        json!({ "bringup_timeout_ms": u64::MAX }),
    ];
    let mut loaded = 0;
    let mut refused = 0;
    for extra in hostile {
        let mut m = connector_manifest_base("https://x.example/mcp", 10_000);
        m["mcp_http"]
            .as_object_mut()
            .unwrap()
            .remove("request_timeout_ms");
        for (k, v) in extra.as_object().unwrap() {
            m["mcp_http"][k] = v.clone();
        }
        match Manifest::parse(&m.to_string()) {
            Err(_) => refused += 1,
            Ok(parsed) => {
                loaded += 1;
                let budget = connector_bringup_budget(&parsed);
                assert!(
                    budget <= MAX_CONNECTOR_BRINGUP_BUDGET,
                    "{m} yields a {budget:?} bring-up cap, over the \
                     {MAX_CONNECTOR_BRINGUP_BUDGET:?} bound"
                );
            }
        }
    }
    // Neither half may be vacuous: some of these must load, and some must be
    // refused rather than silently clamped.
    assert!(loaded >= 6, "only {loaded} manifests loaded");
    assert_eq!(refused, 2, "the over-ceiling values must be refused");

    // …and the other connector kind: `cli_query.timeout_ms` is uncapped, yet must still yield
    // the fixed `CLI_QUERY_BRINGUP_BUDGET`.
    use calm_server::plugin_host::manifest::CLI_QUERY_MAX_OUTPUT_BYTES_CEILING as OUTPUT_CEILING;
    let cli_manifest = |extra: &Value| {
        let mut m = json!({
            "manifest_version": 1,
            "kind": "cli-query",
            "id": CONNECTOR_ID,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "LB Query",
            "cli_query": {
                "command": "longbridge",
                "tools": [{ "name": "quote", "input_schema": {}, "args": ["quote"] }],
            }
        });
        for (k, v) in extra.as_object().unwrap() {
            m["cli_query"][k] = v.clone();
        }
        m
    };

    let mut cli_loaded = 0;
    for extra in [
        json!({}),
        json!({ "timeout_ms": 600_000 }),
        json!({ "timeout_ms": u32::MAX }),
        json!({ "timeout_ms": u64::MAX }),
        // The ceiling itself must load; the refusal of anything past it is asserted below.
        json!({ "timeout_ms": 0, "max_output_bytes": OUTPUT_CEILING }),
        json!({ "search_path_extra": ["/opt/lb/bin"], "env_allow": ["TZ", "no_proxy"] }),
    ] {
        let m = cli_manifest(&extra);
        let parsed =
            Manifest::parse(&m.to_string()).unwrap_or_else(|e| panic!("{m} must load, got {e}"));
        cli_loaded += 1;
        let budget = connector_bringup_budget(&parsed);
        assert!(
            budget <= MAX_CONNECTOR_BRINGUP_BUDGET,
            "{m} yields a {budget:?} bring-up cap, over the \
             {MAX_CONNECTOR_BRINGUP_BUDGET:?} bound"
        );
        // Not merely under the cap: independent of the call budget entirely.
        assert_eq!(
            budget,
            calm_server::plugin_host::CLI_QUERY_BRINGUP_BUDGET,
            "{m}: the cli-query bring-up budget must be the fixed constant"
        );
    }
    assert_eq!(cli_loaded, 6, "the cli-query arm must not be vacuous");

    // …and the values a `cli-query` validator must REFUSE outright.
    for extra in [
        json!({ "env_allow": ["GH_TOKEN"] }),
        json!({ "env_allow": ["SSH_AUTH_SOCK"] }),
    ] {
        let m = cli_manifest(&extra);
        assert!(
            Manifest::parse(&m.to_string()).is_err(),
            "{m} must be refused: a forge credential has no safe fallback"
        );
    }

    // …while an over-ceiling `max_output_bytes` LOADS and is CLAMPED: `load_from_dir` re-parses on
    // boot and only warns past a failure, so a parse-time refusal would make an installed connector vanish.
    for extra in [
        json!({ "max_output_bytes": OUTPUT_CEILING + 1 }),
        json!({ "max_output_bytes": u64::MAX }),
    ] {
        let m = cli_manifest(&extra);
        let parsed = Manifest::parse(&m.to_string())
            .unwrap_or_else(|e| panic!("{m} must load and clamp, not be refused: {e}"));
        assert_eq!(
            parsed.cli_query.as_ref().unwrap().max_output_bytes(),
            OUTPUT_CEILING,
            "{m} must be clamped to the ceiling"
        );
    }
}

#[tokio::test]
async fn a_long_running_tools_call_outlives_the_bringup_budget() {
    let stub = StubServer::start(StubMode::SlowToolsCall).await;
    let b = boot().await;
    write_connector_with(
        &b.plugins_dir,
        &stub.url(),
        Budgets {
            call_ms: 20_000,
            bringup_ms: Some(400),
        },
        0o600,
    );
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;
    host.spawn(CONNECTOR_ID)
        .await
        .expect("a prompt upstream must come up inside a 400 ms bring-up budget");

    let ConnectorClient::Http(client) = host
        .connector_client(CONNECTOR_ID)
        .await
        .expect("live client")
    else {
        panic!("expected the http connector client");
    };
    let started = Instant::now();
    let out = client
        .tools_call(ALLOWED_TOOL, json!({ "page": 1 }))
        .await
        .expect("a long-running tool call must not be cut off by the bring-up budget");
    let elapsed = started.elapsed();

    // The upstream really was slow — otherwise the deadline was never tested.
    assert!(
        elapsed >= SLOW_TOOLS_CALL,
        "the fixture must actually outlast the bring-up budget, took {elapsed:?}"
    );
    let text = serde_json::to_string(&out).unwrap();
    assert!(text.contains(&format!("rows for {ALLOWED_TOOL}")), "{text}");
}

/// Success-path leak: echoed credentials in `tools/list` descriptions and `tools/call` results
/// become `ExposedTool` entries and transcript payloads; this is the JSON-tree scrub, not the 4xx arm.
#[tokio::test]
async fn a_success_path_that_echoes_the_credential_never_leaks_the_key() {
    let stub = StubServer::start(StubMode::EchoAuthInResults).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;
    host.spawn(CONNECTOR_ID).await.expect("connector spawns");

    // The fixture really did put the key on the wire AND echo it back.
    assert!(
        stub.auth_headers().iter().any(|a| a.contains(SECRET_VALUE)),
        "the fixture must actually send the key: {:?}",
        stub.auth_headers()
    );

    // 1. The materialized tool catalog — what agents and operators read.
    let manifest = host.registry().get(CONNECTOR_ID).expect("registry entry");
    let catalog = serde_json::to_string(&manifest.exposes_tools).unwrap();
    assert!(
        catalog.contains("upstream saw"),
        "the fixture must have echoed into the description: {catalog}"
    );
    assert!(!catalog.contains(SECRET_VALUE), "{catalog}");
    assert!(catalog.contains("<redacted>"), "{catalog}");

    // 2. The `tools/call` result — what reaches the track transcript.
    let ConnectorClient::Http(client) = host
        .connector_client(CONNECTOR_ID)
        .await
        .expect("live client")
    else {
        panic!("expected the http connector client");
    };
    let out = client.tools_call(ALLOWED_TOOL, json!({})).await.unwrap();
    let text = serde_json::to_string(&out).unwrap();
    assert!(text.contains("upstream saw"), "fixture check: {text}");
    assert!(!text.contains(SECRET_VALUE), "{text}");
    assert!(text.contains("<redacted>"), "{text}");
}

/// `publish_unavailable` returning `false` is a snapshot taken under the process-table lock;
/// a `stop()` landing after the release must not be overwritten by a stale `Running`.
#[tokio::test]
async fn the_boot_budget_reconcile_does_not_resurrect_a_stopped_connector() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;
    host.spawn(CONNECTOR_ID).await.expect("connector spawns");
    assert!(
        host.reaffirm_running(CONNECTOR_ID).await,
        "sanity: it is up"
    );

    // The concurrent `stop()` of the race, resolved to its completed form.
    host.stop(CONNECTOR_ID).await.expect("stop");
    assert!(host.status(CONNECTOR_ID).await.is_none());

    // Now the boot-budget arm gets around to re-emitting. It must not.
    assert!(
        !host.reaffirm_running(CONNECTOR_ID).await,
        "a connector that is gone must not be re-announced as Running"
    );

    // …and the last word in the event log is still `disabled`.
    let states = plugin_state_events(&b).await;
    assert_eq!(
        states.last().map(String::as_str),
        Some("disabled"),
        "the persisted+broadcast state must end at disabled: {states:?}"
    );
    assert!(!host.running_plugin_ids().await.contains(CONNECTOR_ID));
}

/// The mirror case, so the test above cannot pass by never emitting.
#[tokio::test]
async fn the_boot_budget_reconcile_does_re_emit_for_a_live_connector() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;
    host.spawn(CONNECTOR_ID).await.expect("connector spawns");

    let before = plugin_state_events(&b).await.len();
    assert!(host.reaffirm_running(CONNECTOR_ID).await);
    let after = plugin_state_events(&b).await;
    assert_eq!(after.len(), before + 1, "{after:?}");
    assert_eq!(after.last().map(String::as_str), Some("running"));
}

/// Every `PluginState` state string recorded for [`CONNECTOR_ID`], oldest first, from the persisted log.
async fn plugin_state_events(b: &Boot) -> Vec<String> {
    b.repo
        .events_since(0, 500)
        .await
        .expect("events")
        .into_iter()
        .filter_map(|(_, _, _, event)| match event {
            calm_server::event::Event::PluginState { id, state, .. } if id == CONNECTOR_ID => {
                Some(state)
            }
            _ => None,
        })
        .collect()
}

/// Drives the real `spawn` against an upstream whose 4xx body echoes the credential header,
/// padded so the key straddles `MAX_UPSTREAM_DETAIL_CHARS`: clamp-then-scrub leaks
/// `KEY_STRADDLE_TAIL` chars, scrub-then-clamp none.
#[tokio::test]
async fn a_4xx_body_echoing_the_credential_never_leaks_a_partial_key() {
    let stub = StubServer::start(StubMode::EchoAuthIn4xx).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 2_000, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    let reason = match host.spawn(CONNECTOR_ID).await {
        Err(HostError::ConnectorUnavailable { reason, .. }) => reason,
        other => panic!("expected ConnectorUnavailable, got {other:?}"),
    };

    // The upstream really did echo the key back — otherwise this proves nothing.
    assert!(
        stub.auth_headers().iter().any(|a| a.contains(SECRET_VALUE)),
        "the fixture must actually put the key on the wire: {:?}",
        stub.auth_headers()
    );
    // The truncation really happened at the boundary the key straddles.
    assert!(
        reason.contains("truncated"),
        "reason was not clamped: {reason}"
    );

    let leaked_prefix = &SECRET_VALUE[..KEY_STRADDLE_TAIL];
    assert!(
        !reason.contains(leaked_prefix),
        "a {KEY_STRADDLE_TAIL}-char credential prefix survived into last_error: {reason}"
    );
    assert!(!reason.contains(SECRET_VALUE), "{reason}");
    assert!(reason.contains("<redacted>"), "{reason}");

    // And the same string is what the operator sees over HTTP.
    let state = b.state(Arc::clone(&host));
    let (code, body) = get_text(&state, &format!("/api/plugins/{CONNECTOR_ID}")).await;
    assert_eq!(code, StatusCode::OK);
    assert!(!body.contains(leaked_prefix), "{body}");
}

/// The gated `tools/list` reply pins `spawn_under` inside the guard; without it the spawn
/// could finish first and every assertion would pass without observing the lock.
#[tokio::test]
async fn uninstall_is_refused_while_a_connector_spawn_is_in_flight() {
    let (release, gate) = oneshot::channel::<()>();
    let stub = StubServer::start_gated(StubMode::Normal, Some(gate)).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;
    assert!(host.registry().get(CONNECTOR_ID).is_some());

    // Start the spawn; it will block inside `tools/list`, holding the guard.
    let spawn_host = Arc::clone(&host);
    let spawning = tokio::spawn(async move { spawn_host.spawn(CONNECTOR_ID).await });
    stub.wait_for_tools_list().await;

    // ... and while it is on the wire, the operator uninstalls.
    let err = host
        .uninstall(CONNECTOR_ID)
        .await
        .expect_err("uninstall must be refused while the spawn holds the lock");
    assert_eq!(
        err.code(),
        "plugin_busy",
        "the refusal must be distinguishable from `plugin_conflict`: got {err:?}"
    );
    assert_eq!(err.status(), StatusCode::CONFLICT);

    // Fail closed: DB row, registry entry and token row are all untouched.
    assert!(
        b.repo
            .plugin_get_by_id(CONNECTOR_ID)
            .await
            .unwrap()
            .is_some(),
        "a refused uninstall must not delete the plugin row"
    );
    assert!(
        host.registry().get(CONNECTOR_ID).is_some(),
        "a refused uninstall must not remove the registry entry"
    );

    let _ = release.send(());
    tokio::time::timeout(Duration::from_secs(20), spawning)
        .await
        .expect("spawn never returned")
        .expect("spawn task panicked")
        .expect("the spawn must still succeed: the refusal happened to the OTHER caller");

    // The caller retries — reject semantics mean nothing resumed on its own.
    host.uninstall(CONNECTOR_ID)
        .await
        .expect("uninstall succeeds once the lock is free");

    assert!(host.registry().get(CONNECTOR_ID).is_none());
    assert!(host.registry().is_empty());
    assert!(
        b.repo
            .plugin_get_by_id(CONNECTOR_ID)
            .await
            .unwrap()
            .is_none(),
        "the plugin row must be gone"
    );
    assert!(
        host.status(CONNECTOR_ID).await.is_none(),
        "no live/reserved entry may survive the uninstall"
    );
    assert!(!host.running_plugin_ids().await.contains(CONNECTOR_ID));
}

/// Barrier: the FIRST stub's gated `tools/list`. The terminal assertion is that the new
/// endpoint really received the handshake and the new allow-list materialized.
#[tokio::test]
async fn reload_is_refused_while_a_spawn_is_in_flight_then_repoints_the_connector() {
    let (release, gate) = oneshot::channel::<()>();
    let old_stub = StubServer::start_gated(StubMode::Normal, Some(gate)).await;
    let new_stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &old_stub.url(), 5_000, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    let spawn_host = Arc::clone(&host);
    let spawning = tokio::spawn(async move { spawn_host.spawn(CONNECTOR_ID).await });
    old_stub.wait_for_tools_list().await;

    // Point the on-disk manifest at the new endpoint, restricted to ONE tool,
    // then reload while the old spawn is still on the wire.
    write_connector_with(
        &b.plugins_dir,
        &new_stub.url(),
        Budgets::uniform(5_000),
        0o600,
    );
    let err = host
        .reload(CONNECTOR_ID)
        .await
        .expect_err("reload must be refused while the spawn holds the lock");
    assert_eq!(err.code(), "plugin_busy", "got {err:?}");
    assert_eq!(
        host.registry().get(CONNECTOR_ID).map(|m| m
            .mcp_http
            .as_ref()
            .expect("connector block")
            .url
            .clone()),
        Some(old_stub.url()),
        "a refused reload must not have republished the manifest"
    );

    let _ = release.send(());
    tokio::time::timeout(Duration::from_secs(20), spawning)
        .await
        .expect("spawn never returned")
        .expect("spawn task panicked")
        .expect("spawn must succeed");

    assert!(
        new_stub.methods().is_empty(),
        "nothing has hit the new endpoint yet"
    );

    host.reload(CONNECTOR_ID)
        .await
        .expect("reload succeeds once the lock is free");

    // The retry did the real work: the NEW endpoint was handshaken.
    assert!(
        new_stub.methods().contains(&"tools/list".to_string()),
        "the reloaded connector must have queried the new endpoint: {:?}",
        new_stub.methods()
    );
    assert_eq!(
        host.registry()
            .get(CONNECTOR_ID)
            .and_then(|m| m.mcp_http.as_ref().map(|b| b.url.clone())),
        Some(new_stub.url())
    );
    assert!(
        matches!(
            host.status(CONNECTOR_ID).await.map(|s| s.status),
            Some(PluginRuntimeStatus::Running)
        ),
        "the reloaded connector must be Running"
    );
}

#[tokio::test]
async fn connector_card_creation_is_a_4xx_that_names_the_real_reason() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;
    host.spawn(CONNECTOR_ID).await.expect("connector spawns");
    let track_id = b.seed_track().await;
    let state = b.state(Arc::clone(&host));

    // Drive the REAL route, not the two accessors.
    let resp = cards_app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{track_id}/cards"))
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
        state
            .repo
            .cards_by_track(&track_id)
            .await
            .unwrap()
            .is_empty(),
        "no card may have been written"
    );

    // A genuinely-absent plugin still gets the 404 — the two cases must not
    // have collapsed into one.
    let resp = cards_app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{track_id}/cards"))
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

/// A slow event store must not hold boot: a foreign `BEGIN IMMEDIATE` holds the DB writer so
/// every emission the loop attempts parks, and boot must still return inside the ceiling the
/// loop computes for itself.
#[tokio::test]
async fn a_slow_event_store_cannot_hold_boot_past_the_phase_ceiling() {
    use calm_server::plugin_host::{
        Manifest, connector_bringup_budget, connector_phase_ceiling, widened_connector_budget,
    };

    let stub = StubServer::start(StubMode::Hang).await;
    let b = boot().await;
    const N: usize = 4;
    for i in 0..N {
        let id = format!("dead-connector-{i}");
        let dir = b.plugins_dir.join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut manifest = connector_manifest_json(
            &stub.url(),
            Budgets {
                call_ms: 10_000,
                bringup_ms: Some(200),
            },
        );
        manifest["id"] = json!(id);
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let secrets = dir.join("secrets.json");
        std::fs::write(&secrets, json!({ SECRET_NAME: SECRET_VALUE }).to_string()).unwrap();
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
        seed_row(&b, &id).await;
    }
    let host = b.host();

    // Hold the DB writer for much longer than the ceiling, so every
    // `log_pure_event` inside the loop blocks.
    const DB_HELD: Duration = Duration::from_secs(8);
    let (held_tx, held_rx) = oneshot::channel::<()>();
    let (release_db, db_rx) = oneshot::channel::<()>();
    let repo = b.repo.clone();
    let holder = tokio::spawn(async move {
        repo.write_in_tx(Box::new(move |_tx| {
            Box::pin(async move {
                let _ = held_tx.send(());
                let _ = tokio::time::timeout(DB_HELD, db_rx).await;
                Ok(())
            })
        }))
        .await
    });
    held_rx.await.expect("write tx never opened");

    // The ceiling is COMPUTED from the two production expressions the loop evaluates, never
    // restated as a number here.
    const BUDGET: Duration = Duration::from_millis(1_000);
    let widest = connector_bringup_budget(
        &Manifest::parse(
            &connector_manifest_json(
                &stub.url(),
                Budgets {
                    call_ms: 10_000,
                    bringup_ms: Some(200),
                },
            )
            .to_string(),
        )
        .expect("the fixture manifest the loop will read must parse"),
    );
    let ceiling = connector_phase_ceiling(widened_connector_budget(BUDGET, widest));
    // The composed number is also pinned as a literal so a drift in the formula has to be acknowledged here.
    assert_eq!(
        ceiling,
        Duration::from_millis(1_900),
        "the connector-phase ceiling for this fixture (1 s budget, 200 ms \
         per-request bring-up) is (2 × 200 ms + 500 ms slack) + 500 ms widening \
         slack + 500 ms reconcile tail = 1.9 s; if production's formula moved, \
         change it here deliberately"
    );
    let started = Instant::now();
    tokio::time::timeout(DB_HELD * 2, host.autospawn_enabled_within(BUDGET))
        .await
        .expect("boot autospawn never returned at all");
    let elapsed = started.elapsed();

    // The fixture has to have been in force for this to mean anything.
    assert!(
        elapsed < DB_HELD,
        "boot took {elapsed:?}, i.e. it waited for the event store to free up — \
         the bound covers the spawn step only, not the emissions after it"
    );
    // The loop must actually have run out its budget, or the upper bound below is satisfied by
    // a loop that did nothing.
    assert!(
        elapsed >= BUDGET,
        "boot returned in {elapsed:?}, faster than the {BUDGET:?} budget it was \
         given — the hang fixture is no longer in force"
    );
    // Tolerance covers scheduling jitter around the fence only (~4 ms measured); a whole extra
    // step moves elapsed by hundreds of ms.
    const JITTER: Duration = Duration::from_millis(250);
    assert!(
        elapsed < ceiling + JITTER,
        "boot took {elapsed:?} against a {ceiling:?} connector-phase ceiling \
         (+{JITTER:?} jitter allowance)"
    );

    // Every connector still has a terminal live entry: that half of the transition is a synchronous table write.
    for i in 0..N {
        let id = format!("dead-connector-{i}");
        let status = host
            .status(&id)
            .await
            .unwrap_or_else(|| panic!("{id} must leave an observable entry"));
        assert!(
            matches!(status.status, PluginRuntimeStatus::Unavailable { .. }),
            "{id}: {:?}",
            status.status
        );
    }

    let _ = release_db.send(());
    let _ = holder.await;
}

/// The documented ceiling and the computed ceiling are one expression; the lifecycle lock does
/// not enter it because every acquisition on the boot path happens inside the `timeout_at` fence.
#[test]
fn the_connector_phase_ceiling_is_the_documented_one() {
    use calm_server::plugin_host::{
        CONNECTOR_AUTOSPAWN_BUDGET, CONNECTOR_LOOP_WIDENING_MARGIN, MAX_CONNECTOR_AUTOSPAWN_WALL,
        MAX_CONNECTOR_BRINGUP_BUDGET, connector_phase_ceiling,
    };

    // 2 × 15 s (the validated per-request bring-up ceiling) + 500 ms
    // per-connector slack.
    assert_eq!(MAX_CONNECTOR_BRINGUP_BUDGET, Duration::from_millis(30_500));
    // …widened by the LOOP margin, then the reconcile tail. This is the number the docs state.
    assert_eq!(MAX_CONNECTOR_AUTOSPAWN_WALL, Duration::from_millis(31_500));
    // `CONNECTOR_LOOP_WIDENING_MARGIN` and `CONNECTOR_BRINGUP_SLACK` are both 500 ms today, so no
    // test can tell them apart; this line must name the constant that actually feeds the expression.
    assert_eq!(
        MAX_CONNECTOR_AUTOSPAWN_WALL,
        connector_phase_ceiling(MAX_CONNECTOR_BRINGUP_BUDGET + CONNECTOR_LOOP_WIDENING_MARGIN)
    );
    // And the floor is a floor: the widened budget is never below the constant.
    assert!(MAX_CONNECTOR_AUTOSPAWN_WALL > connector_phase_ceiling(CONNECTOR_AUTOSPAWN_BUDGET));
}

/// The spawn is pinned with its live `Running` entry published and its `running` emission not
/// yet committed; a real `stop` in that window must be refused with `LifecycleBusy`. The held
/// `write_in_tx` blocks EVERY write in the database, so do not drive a second plugin here.
#[tokio::test]
async fn a_stop_cannot_split_a_spawn_between_its_table_write_and_its_emission() {
    let (release_gate, gate) = oneshot::channel::<()>();
    let stub = StubServer::start_gated(StubMode::Normal, Some(gate)).await;
    let b = boot().await;
    write_connector_with(&b.plugins_dir, &stub.url(), Budgets::uniform(5_000), 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    let spawn_host = Arc::clone(&host);
    let spawning = tokio::spawn(async move { spawn_host.spawn(CONNECTOR_ID).await });
    stub.wait_for_tools_list().await;

    // Park every emission: no `plugin.state` write can commit while this is held.
    let (held_tx, held_rx) = oneshot::channel::<()>();
    let (release_db, db_rx) = oneshot::channel::<()>();
    let repo = b.repo.clone();
    let holder = tokio::spawn(async move {
        repo.write_in_tx(Box::new(move |_tx| {
            Box::pin(async move {
                let _ = held_tx.send(());
                let _ = db_rx.await;
                Ok(())
            })
        }))
        .await
    });
    held_rx.await.expect("write tx never opened");

    // Let the spawn finish its network half: it publishes the live `Running` entry, then parks
    // inside its `running` emission, still holding the lifecycle guard.
    release_gate.send(()).expect("stub gate receiver gone");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if matches!(
            host.status(CONNECTOR_ID).await.map(|s| s.status),
            Some(PluginRuntimeStatus::Running)
        ) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "spawn never reached the live insert"
        );
        sleep(Duration::from_millis(5)).await;
    }

    // Bounded on purpose: a `stop` that got into the critical section would park on the held DB
    // writer and turn a red into a hang.
    let sh = Arc::clone(&host);
    let stopping = tokio::spawn(async move { sh.stop(CONNECTOR_ID).await });
    let err = tokio::time::timeout(Duration::from_secs(5), stopping)
        .await
        .expect(
            "stop did not answer within 5 s: it must be refused at the entry, \
             not admitted into the spawn's critical section",
        )
        .expect("stop task panicked")
        .expect_err("stop must be refused inside the spawn's critical section");
    assert!(
        matches!(err, HostError::LifecycleBusy(ref id) if id == CONNECTOR_ID),
        "expected LifecycleBusy, got {err:?}"
    );
    // Reject semantics: nothing happened, and nothing resumes on its own.
    assert!(
        matches!(
            host.status(CONNECTOR_ID).await.map(|s| s.status),
            Some(PluginRuntimeStatus::Running)
        ),
        "a refused stop must not have touched the live table"
    );

    let _ = release_db.send(());
    let _ = holder.await;
    tokio::time::timeout(Duration::from_secs(20), spawning)
        .await
        .expect("spawn never returned")
        .expect("spawn task panicked")
        .expect("spawn must succeed");

    // Explicit retry — the loser did not queue.
    host.stop(CONNECTOR_ID)
        .await
        .expect("stop must succeed now");

    assert!(host.status(CONNECTOR_ID).await.is_none());
    let states = plugin_state_events(&b).await;
    assert_eq!(
        states.iter().rev().take(2).rev().collect::<Vec<_>>(),
        vec!["running", "disabled"],
        "the event tail must be running → disabled, got {states:?}"
    );
}

/// `scrub_value` does not descend into JSON numbers, so a number-shaped credential is refused at
/// the source; the rule is the JSON number grammar (`-1234567` too), driven through the real `spawn`.
#[tokio::test]
async fn a_number_shaped_credential_never_reaches_the_wire() {
    for numeric in ["12345678", "-1234567"] {
        let stub = StubServer::start(StubMode::Normal).await;
        let b = boot().await;
        let dir = write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
        let secrets = dir.join("secrets.json");
        std::fs::write(&secrets, json!({ SECRET_NAME: numeric }).to_string()).unwrap();
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
        let host = b.host();
        seed_row(&b, CONNECTOR_ID).await;

        let err = match host.spawn(CONNECTOR_ID).await {
            Err(HostError::ConnectorUnavailable { reason, .. }) => reason,
            other => {
                panic!("{numeric:?} must not bring a connector up: {other:?}")
            }
        };
        assert!(
            err.contains("parses as a JSON number"),
            "{numeric:?}: the refusal must say what is wrong: {err}"
        );
        // The refusal itself must not quote the credential — it is persisted
        // and broadcast as `PluginState.last_error`.
        assert!(!err.contains(numeric), "{numeric:?}: {err}");
        // Nothing was sent: the client is never constructed.
        assert!(
            stub.queries().is_empty(),
            "{numeric:?} reached the wire: {:?}",
            stub.queries()
        );
        assert!(!host.running_plugin_ids().await.contains(CONNECTOR_ID));
    }
}

/// One scrub pass never rescans what it wrote, so a credential overlapping `<redacted>` (e.g.
/// `redacted>y`) could be re-formed from its own redaction; such credentials are refused at the
/// source, through the real `spawn`.
#[tokio::test]
async fn a_credential_overlapping_the_redaction_marker_never_reaches_the_wire() {
    for (overlapping, direction) in [
        ("redacted>y", "begins with a suffix of the marker"),
        ("abcdef-<", "ends with a prefix of the marker"),
        ("sk-<redacted>-x", "contains the marker"),
        ("edacted>#", "is a piece of the marker"),
    ] {
        let stub = StubServer::start(StubMode::Normal).await;
        let b = boot().await;
        let dir = write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
        let secrets = dir.join("secrets.json");
        std::fs::write(&secrets, json!({ SECRET_NAME: overlapping }).to_string()).unwrap();
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
        let host = b.host();
        seed_row(&b, CONNECTOR_ID).await;

        let err = match host.spawn(CONNECTOR_ID).await {
            Err(HostError::ConnectorUnavailable { reason, .. }) => reason,
            other => {
                panic!("{overlapping:?} ({direction}) must not bring a connector up: {other:?}")
            }
        };
        assert!(
            err.contains("overlaps the marker"),
            "{overlapping:?} ({direction}): the refusal must say what is wrong: {err}"
        );
        // Persisted and broadcast as `PluginState.last_error`: it may quote
        // neither the credential nor — since some of these ARE pieces of it —
        // the marker.
        assert!(!err.contains(overlapping), "{overlapping:?}: {err}");
        assert!(!err.contains("<redacted>"), "{overlapping:?}: {err}");
        // Nothing was sent: the client is never constructed.
        assert!(
            stub.queries().is_empty(),
            "{overlapping:?} reached the wire: {:?}",
            stub.queries()
        );
        assert!(!host.running_plugin_ids().await.contains(CONNECTOR_ID));
    }
}

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

// Configuration reaching a `cli-query` connector: the script echoes its argv and environment,
// so what is asserted is what the CHILD received.

const CLI_CONFIG_ID: &str = "cli-configured";

/// Install a `cli-query` connector that consumes configuration two ways: an argv slot
/// (`{{config.endpoint}}`) and an env key (`config_env`). `required` decides the manifest
/// version (3 if a rollback could lose something, else 2).
fn write_configured_cli_connector(plugins_dir: &Path, command: &str, required: bool) -> PathBuf {
    let dir = plugins_dir.join(CLI_CONFIG_ID);
    std::fs::create_dir_all(&dir).unwrap();
    let mut config_schema = json!({
        "type": "object",
        "properties": {
            "endpoint": { "type": "string", "default": "https://default.example" },
            "LB_ACCOUNT": { "type": "string" }
        },
        "additionalProperties": false
    });
    if required {
        config_schema["required"] = json!(["LB_ACCOUNT"]);
    }
    std::fs::write(
        dir.join("manifest.json"),
        json!({
            "manifest_version": if required { 3 } else { 2 },
            "kind": "cli-query",
            "id": CLI_CONFIG_ID,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Configured",
            "config_schema": config_schema,
            "cli_query": {
                "command": command,
                "timeout_ms": 5_000,
                "config_env": ["LB_ACCOUNT"],
                "tools": [{
                    "name": CLI_TOOL,
                    "input_schema": {
                        "type": "object",
                        "properties": { "symbol": { "type": "string" } },
                        "required": ["symbol"],
                        "additionalProperties": false
                    },
                    "args": ["quote", "{{symbol}}", "--url", "{{config.endpoint}}"]
                }]
            }
        })
        .to_string(),
    )
    .unwrap();
    dir
}

async fn patch_json(state: &AppState, path: &str, body: Value) -> (StatusCode, Value) {
    let resp = app(state.clone())
        .oneshot(
            Request::builder()
                .method("PATCH")
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

/// The pair is the point: either half alone is satisfiable by an implementation that merges
/// the two sources and happens to order them the tested way.
#[tokio::test]
async fn cli_query_configuration_fills_argv_and_env_and_an_agent_cannot_displace_it() {
    let b = boot().await;
    let bin = tempfile::tempdir().unwrap();
    // Echo every argv element and the one configured env key, so the
    // assertions are about what the CHILD got.
    let script = write_script(
        bin.path(),
        "argv-env.sh",
        "#!/bin/sh\nfor a in \"$@\"; do echo \"arg:$a\"; done\necho \"env:LB_ACCOUNT=${LB_ACCOUNT-<unset>}\"\n",
    );
    let dir = write_configured_cli_connector(&b.plugins_dir, &script.display().to_string(), false);

    let host = b.host();
    let state = b.state(Arc::clone(&host));
    let (status, body) = post_json(
        &state,
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": dir.display().to_string() } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "install failed: {body}");

    // The operator writes configuration through the real write path.
    let (status, body) = patch_json(
        &state,
        &format!("/api/plugins/{CLI_CONFIG_ID}/config"),
        json!({ "endpoint": "https://operator.example", "LB_ACCOUNT": "acct-42" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "config write failed: {body}");

    let (status, body) = post_json(
        &state,
        &format!("/api/plugins/{CLI_CONFIG_ID}/enable"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "enable failed: {body}");

    let client = state
        .plugin
        .connector_client(CLI_CONFIG_ID)
        .await
        .expect("running connector must expose a client");
    let ConnectorClient::Cli(cli) = &client else {
        panic!("expected a Cli client, got {client:?}");
    };

    let res = cli
        .tools_call(
            CLI_TOOL,
            // The attack: an argument named exactly like the config slot.
            json!({ "symbol": "700.HK", "config.endpoint": "https://attacker.example" }),
        )
        .await
        .expect("tools/call");
    assert_eq!(res.is_error, Some(false));
    let text = res.content[0].text.clone().unwrap();
    assert_eq!(
        text,
        "arg:quote\narg:700.HK\narg:--url\narg:https://operator.example\n\
         env:LB_ACCOUNT=acct-42\n",
        "the config slot must carry the operator's value, the config_env key must \
         reach the child, and the agent's `config.endpoint` argument must land nowhere"
    );
    assert!(
        !text.contains("attacker"),
        "an agent-supplied `config.*` argument reached the child: {text}"
    );
}

/// A manifest `default` is delivered by the kernel at bring-up, not written into the DB.
#[tokio::test]
async fn a_manifest_default_reaches_the_child_without_any_operator_write() {
    let b = boot().await;
    let bin = tempfile::tempdir().unwrap();
    let script = write_script(
        bin.path(),
        "argv.sh",
        "#!/bin/sh\nfor a in \"$@\"; do echo \"arg:$a\"; done\n",
    );
    write_configured_cli_connector(&b.plugins_dir, &script.display().to_string(), false);
    let host = b.host();
    seed_row(&b, CLI_CONFIG_ID).await;
    host.spawn(CLI_CONFIG_ID).await.expect("connector spawns");

    let client = host.connector_client(CLI_CONFIG_ID).await.expect("client");
    let ConnectorClient::Cli(cli) = &client else {
        panic!("expected a Cli client, got {client:?}");
    };
    let res = cli
        .tools_call(CLI_TOOL, json!({ "symbol": "X" }))
        .await
        .expect("tools/call");
    let text = res.content[0].text.clone().unwrap();
    assert_eq!(
        text, "arg:quote\narg:X\narg:--url\narg:https://default.example\n",
        "the manifest default must be in force with no operator write"
    );

    // …and the DB still records that the operator chose nothing.
    let row = b
        .repo
        .plugin_get_by_id(CLI_CONFIG_ID)
        .await
        .unwrap()
        .expect("row");
    assert_eq!(row.user_config, json!({}), "defaults must not be persisted");
}

#[tokio::test]
async fn a_cli_connector_missing_required_configuration_lands_unavailable() {
    let b = boot().await;
    let bin = tempfile::tempdir().unwrap();
    let script = write_script(bin.path(), "ok.sh", "#!/bin/sh\necho ok\n");
    write_configured_cli_connector(&b.plugins_dir, &script.display().to_string(), true);
    let host = b.host();
    seed_row(&b, CLI_CONFIG_ID).await;

    let err = host
        .spawn(CLI_CONFIG_ID)
        .await
        .expect_err("a connector missing required configuration must not come up");
    // The SAME variant the `app` path raises for the same failure class.
    assert!(
        matches!(err, HostError::MissingRequiredConfig { .. }),
        "got {err:?}"
    );
    let status = host.status(CLI_CONFIG_ID).await.expect("status");
    let PluginRuntimeStatus::Unavailable { reason } = &status.status else {
        panic!("expected Unavailable, got {:?}", status.status);
    };
    // The wording is shared too, verbatim: `last_error` renders the same way for both kinds.
    assert_eq!(
        reason,
        "missing required configuration: LB_ACCOUNT. Set it under Settings › \
         Plugins, then start the plugin again.",
        "the reason must be `config::missing_required_reason`'s, verbatim"
    );
    assert!(!host.running_plugin_ids().await.contains(CLI_CONFIG_ID));

    // The same connector comes up once the operator supplies the key — so the
    // refusal above is about the configuration, not about the fixture.
    b.repo
        .plugin_update_user_config(CLI_CONFIG_ID, json!({ "LB_ACCOUNT": "acct-42" }))
        .await
        .expect("configure");
    let host = b.host();
    host.spawn(CLI_CONFIG_ID)
        .await
        .expect("a fully configured connector comes up");
    assert!(host.running_plugin_ids().await.contains(CLI_CONFIG_ID));
}

/// `DROP TABLE plugins` through the pool the host holds breaks the first DB read on the
/// `cli-query` spawn path; the spawn must be refused (not proceed on `{}`), the refusal must be
/// observable through `status()`, and `last_error` must say the store could not be read.
#[tokio::test]
async fn a_cli_connector_whose_config_store_is_unreadable_lands_unavailable() {
    let b = boot().await;
    let bin = tempfile::tempdir().unwrap();
    let script = write_script(bin.path(), "ok.sh", "#!/bin/sh\necho ok\n");
    write_configured_cli_connector(&b.plugins_dir, &script.display().to_string(), false);
    let host = b.host();
    seed_row(&b, CLI_CONFIG_ID).await;

    sqlx::query("DROP TABLE plugins")
        .execute(b.sqlx.pool())
        .await
        .expect("drop the plugins table out from under the host");

    let err = host
        .spawn(CLI_CONFIG_ID)
        .await
        .expect_err("a spawn that cannot read stored configuration must not proceed");
    assert!(
        matches!(err, HostError::ConfigUnreadable { .. }),
        "the same variant the `app` path raises for the same failure: got {err:?}"
    );
    assert!(
        err.to_string()
            .contains("could not read stored configuration"),
        "the refusal must name the cause: {err}"
    );

    let status = host
        .status(CLI_CONFIG_ID)
        .await
        .expect("the failure must be observable, not a connector that looks unenabled");
    let PluginRuntimeStatus::Unavailable { reason } = &status.status else {
        panic!("expected Unavailable, got {:?}", status.status);
    };
    assert!(
        reason.contains("could not read stored configuration"),
        "`last_error` must say the store failed, not that configuration is \
         missing: {reason}"
    );
    assert!(!host.running_plugin_ids().await.contains(CLI_CONFIG_ID));
}

// Configuration reaching an `mcp-http` connector's url, and the origin lock keyed connectors
// get for it. What is asserted is the request the STUB received.

const HTTP_CONFIG_ID: &str = "mcp-configured";

/// Write an `mcp-http` connector whose url carries `{{config.*}}` slots; `keyed` is the ONLY
/// difference between the two halves of the tiering pair.
fn write_configured_http_connector(
    plugins_dir: &Path,
    url: &str,
    keyed: bool,
    required: bool,
) -> PathBuf {
    let dir = plugins_dir.join(HTTP_CONFIG_ID);
    std::fs::create_dir_all(&dir).unwrap();
    let mut config_schema = json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "rev": { "type": "string" },
            "endpoint": { "type": "string" }
        },
        "additionalProperties": false
    });
    if required {
        config_schema["required"] = json!(["path"]);
    }
    let mut block = json!({
        "url": url,
        "tools_allow": [ALLOWED_TOOL],
        "request_timeout_ms": 5_000,
    });
    if keyed {
        block["api_key_secret"] = json!(SECRET_NAME);
        block["api_key_in"] = json!("bearer");
    }
    std::fs::write(
        dir.join("manifest.json"),
        json!({
            "manifest_version": if required { 3 } else { 2 },
            "kind": "mcp-http",
            "id": HTTP_CONFIG_ID,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Configured Upstream",
            "config_schema": config_schema,
            "mcp_http": block,
        })
        .to_string(),
    )
    .unwrap();
    if keyed {
        let secrets = dir.join("secrets.json");
        let mut f = std::fs::File::create(&secrets).unwrap();
        f.write_all(json!({ SECRET_NAME: SECRET_VALUE }).to_string().as_bytes())
            .unwrap();
        drop(f);
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    dir
}

/// `http://<addr>` + a literal tail, written without `format!` so the doubled
/// braces of a `{{config.*}}` slot stay readable.
fn slotted_url(addr: std::net::SocketAddr, tail: &str) -> String {
    let mut s = String::from("http://");
    s.push_str(&addr.to_string());
    s.push_str(tail);
    s
}

/// The reason recorded in the `unavailable` terminal state, or a panic naming
/// what the connector actually did.
async fn unavailable_reason(host: &Arc<PluginHost>, id: &str) -> String {
    let status = host
        .status(id)
        .await
        .expect("a refused bring-up must still be observable through status()");
    match &status.status {
        PluginRuntimeStatus::Unavailable { reason } => reason.clone(),
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn mcp_http_configuration_fills_the_path_and_query_of_a_keyed_connector() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    let dir = write_configured_http_connector(
        &b.plugins_dir,
        &slotted_url(stub.addr, "/{{config.path}}?rev={{config.rev}}"),
        true,
        false,
    );

    let host = b.host();
    let state = b.state(Arc::clone(&host));
    let (status, body) = post_json(
        &state,
        "/api/plugins/install",
        json!({ "source": { "kind": "local_path", "path": dir.display().to_string() } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "install failed: {body}");

    let (status, body) = patch_json(
        &state,
        &format!("/api/plugins/{HTTP_CONFIG_ID}/config"),
        json!({ "path": "v2/mcp", "rev": "7" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "config write failed: {body}");

    let (status, body) = post_json(
        &state,
        &format!("/api/plugins/{HTTP_CONFIG_ID}/enable"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "enable failed: {body}");
    assert!(
        state
            .plugin
            .running_plugin_ids()
            .await
            .contains(HTTP_CONFIG_ID)
    );

    let targets = stub.targets();
    assert!(!targets.is_empty(), "the stub was never contacted");
    for target in &targets {
        // The request target is EXACTLY the rendered url, with nothing appended; `starts_with` would
        // pass on a client that appended something else.
        assert_eq!(
            target, "/v2/mcp?rev=7",
            "the configured path and query must be what reached the upstream, \
             and nothing may be appended to them"
        );
    }
    // The credential still went, in the header; without this the test would pass on a connector
    // that sent no credential at all.
    let auths = stub.auth_headers();
    assert!(
        auths.iter().all(|a| a == &format!("Bearer {SECRET_VALUE}")),
        "every request must carry the credential as `Bearer <key>`: {auths:?}"
    );
}

/// Negative half + tiering as ONE test: same url template, same configured value, one manifest
/// holds `api_key_secret` and one does not.
#[tokio::test]
async fn only_an_unkeyed_connector_may_have_its_host_configured() {
    let stub = StubServer::start(StubMode::Normal).await;

    // --- keyed: refused, and nothing was sent -----------------------------
    let b = boot().await;
    write_configured_http_connector(
        &b.plugins_dir,
        "http://{{config.endpoint}}/mcp",
        true,
        false,
    );
    let host = b.host();
    seed_row(&b, HTTP_CONFIG_ID).await;
    b.repo
        .plugin_update_user_config(HTTP_CONFIG_ID, json!({ "endpoint": stub.addr.to_string() }))
        .await
        .expect("configure");

    host.spawn(HTTP_CONFIG_ID)
        .await
        .expect_err("a keyed connector's host is not a configurable field");
    let reason = unavailable_reason(&host, HTTP_CONFIG_ID).await;
    assert!(
        reason.contains("origin is locked") && reason.contains("api_key_secret"),
        "the refusal must name the rule and why it applies: {reason}"
    );
    assert!(!host.running_plugin_ids().await.contains(HTTP_CONFIG_ID));
    assert!(
        stub.targets().is_empty(),
        "the credential must not have been sent to the configured host at all: {:?}",
        stub.targets()
    );

    // --- unkeyed: the SAME configuration comes up -------------------------
    let b = boot().await;
    write_configured_http_connector(
        &b.plugins_dir,
        "http://{{config.endpoint}}/mcp",
        false,
        false,
    );
    let host = b.host();
    seed_row(&b, HTTP_CONFIG_ID).await;
    b.repo
        .plugin_update_user_config(HTTP_CONFIG_ID, json!({ "endpoint": stub.addr.to_string() }))
        .await
        .expect("configure");
    host.spawn(HTTP_CONFIG_ID)
        .await
        .expect("with no api_key_secret the whole url is configurable");
    assert!(host.running_plugin_ids().await.contains(HTTP_CONFIG_ID));
    assert!(
        stub.targets().iter().any(|t| t == "/mcp"),
        "the configured host must have been contacted: {:?}",
        stub.targets()
    );
}

/// A WHATWG retargeting case injected as a CONFIGURATION VALUE must be refused as hard as in a
/// manifest, which requires the rendered url to go through `validate_mcp_http_url` itself.
#[tokio::test]
async fn a_configured_url_value_is_refused_by_the_manifests_own_validator() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    write_configured_http_connector(
        &b.plugins_dir,
        &slotted_url(stub.addr, "/{{config.path}}"),
        true,
        false,
    );
    let host = b.host();
    seed_row(&b, HTTP_CONFIG_ID).await;
    b.repo
        .plugin_update_user_config(HTTP_CONFIG_ID, json!({ "path": r"mcp\evil.example" }))
        .await
        .expect("configure");

    host.spawn(HTTP_CONFIG_ID)
        .await
        .expect_err("a backslash retargets the request under WHATWG parsing");
    let reason = unavailable_reason(&host, HTTP_CONFIG_ID).await;
    assert!(
        reason.contains("mcp_http.url") && reason.contains("backslashes"),
        "the refusal must be the url validator's own, naming the field: {reason}"
    );
    assert!(stub.targets().is_empty(), "nothing may have been sent");
}

#[tokio::test]
async fn an_mcp_http_connector_missing_required_configuration_lands_unavailable() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    write_configured_http_connector(
        &b.plugins_dir,
        &slotted_url(stub.addr, "/{{config.path}}"),
        true,
        true,
    );
    let host = b.host();
    seed_row(&b, HTTP_CONFIG_ID).await;

    let err = host
        .spawn(HTTP_CONFIG_ID)
        .await
        .expect_err("a connector missing required configuration must not come up");
    assert!(
        matches!(err, HostError::MissingRequiredConfig { .. }),
        "the same variant the other two kinds raise: got {err:?}"
    );
    assert_eq!(
        unavailable_reason(&host, HTTP_CONFIG_ID).await,
        "missing required configuration: path. Set it under Settings › \
         Plugins, then start the plugin again.",
        "the reason must be `config::missing_required_reason`'s, verbatim"
    );
    assert!(stub.targets().is_empty(), "nothing may have been sent");

    // …and it comes up once the operator supplies the key, so the refusal is
    // about the configuration and not about the fixture.
    b.repo
        .plugin_update_user_config(HTTP_CONFIG_ID, json!({ "path": "mcp" }))
        .await
        .expect("configure");
    let host = b.host();
    host.spawn(HTTP_CONFIG_ID)
        .await
        .expect("a fully configured connector comes up");
    assert!(host.running_plugin_ids().await.contains(HTTP_CONFIG_ID));
}

/// Every `ConnectorKind` variant, taken from serde's own derived error message so a new variant
/// appears here with nobody touching this file; if serde changes the message the caller fails loudly.
fn all_connector_kinds() -> Vec<String> {
    let err = serde_json::from_value::<calm_server::plugin_host::ConnectorKind>(json!(
        "__no_such_kind__"
    ))
    .expect_err("an unknown kind must not deserialize")
    .to_string();
    let list = err
        .split_once("expected one of ")
        .unwrap_or_else(|| panic!("serde's variant list is no longer parseable from: {err}"))
        .1;
    list.split(',')
        .map(|s| s.trim().trim_end_matches('.').trim_matches('`').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// For every `ConnectorKind` there is a real spawn through `PluginHost::spawn`, and every one
/// passed through `config_for_spawn_or_unavailable`; the universe is serde's, so this is set
/// equality. The fail-closed half is `spawn_admitted`'s `debug_assert!`.
#[tokio::test]
async fn every_connector_kind_spawns_through_the_shared_config_gate() {
    let stub = StubServer::start(StubMode::Normal).await;
    let b = boot().await;
    let bin = tempfile::tempdir().unwrap();
    let script = write_script(bin.path(), "ok.sh", "#!/bin/sh\necho ok\n");

    const APP_ID: &str = "echo-app";
    write_app_plugin(&b.plugins_dir, APP_ID);
    write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    write_configured_cli_connector(&b.plugins_dir, &script.display().to_string(), false);

    let host = b.host();
    let kinds = all_connector_kinds();
    assert!(
        kinds.len() >= 3 && kinds.iter().any(|k| k == "app"),
        "serde's variant list did not parse into kinds: {kinds:?}"
    );

    let mut driven = Vec::new();
    for kind in &kinds {
        let id = match kind.as_str() {
            "app" => APP_ID,
            "mcp-http" => CONNECTOR_ID,
            "cli-query" => CLI_CONFIG_ID,
            other => panic!(
                "#1284 §4.7: no spawn fixture for `kind: {other}`. Every kind's spawn \
                 path must go through `config_for_spawn_or_unavailable`; add a fixture \
                 here rather than trusting the kinds that existed when this was written"
            ),
        };
        seed_row(&b, id).await;
        host.spawn(id)
            .await
            .unwrap_or_else(|e| panic!("`{kind}` fixture must spawn: {e}"));
        assert!(
            host.config_gate_ran(id),
            "`{kind}` spawned without passing the shared configuration gate — \
             its effective config and its `required` verdict came from a second \
             copy of the ⊕, which is what §4.7 exists to prevent"
        );
        driven.push(kind.clone());
    }
    assert_eq!(driven, kinds, "every kind must have been driven");
}

/// The guard's miss branch, which no correctly-wired spawn reaches; `cfg(debug_assertions)`
/// because the teeth ARE `debug_assert!`.
#[cfg(debug_assertions)]
#[tokio::test]
#[should_panic(expected = "§4.7")]
async fn the_config_gate_guard_panics_when_the_witness_is_absent() {
    let b = boot().await;
    let host = b.host();
    assert!(!host.config_gate_ran("never-spawned"));
    host.assert_config_gate_ran(
        "never-spawned",
        calm_server::plugin_host::ConnectorKind::McpHttp,
    );
}

/// The release build's half of the same trade-off: a breach is COUNTED, not only logged. Runs
/// through `catch_unwind` because in a debug build the record is written and then the process panics.
#[cfg(debug_assertions)]
#[tokio::test]
async fn a_config_gate_breach_is_counted_not_only_logged() {
    let b = boot().await;
    let host = b.host();
    assert_eq!(host.config_gate_breaches("never-spawned"), 0);
    let host_for_guard = std::sync::Arc::clone(&host);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        host_for_guard.assert_config_gate_ran(
            "never-spawned",
            calm_server::plugin_host::ConnectorKind::McpHttp,
        );
    }))
    .is_err();
    assert!(panicked, "a debug build must still fail loudly");
    assert_eq!(
        host.config_gate_breaches("never-spawned"),
        1,
        "the breach must outlive the log line: this count is the only evidence a \
         release build leaves behind"
    );
}

#[path = "connector_mcp_setup.rs"]
mod mcp_setup;
