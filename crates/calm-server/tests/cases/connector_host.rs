//! #1164 P1 — external connector host (`kind: mcp-http` / `cli-query`).
//!
//! Covers the design doc's §4 acceptance list, minus the items that belong to
//! later slices:
//!
//! * **#1** install + enable → Running, and still Running after a full service
//!   restart (simulated by rebuilding host + registry from disk over the same
//!   repo, which is exactly what boot does).
//! * **#4** a `cli-query` connector resolves + pins its command, comes up
//!   Running with its declared tool visible, executes it on `tools/call`, and
//!   reports a non-zero exit as `isError` rather than as a transport failure.
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
//! production projection/route functions in `mcp_server::transport`. §4 #4 —
//! `cli-query` execution — landed in #1164 P3 and is covered at the bottom of
//! this file, against a script the test writes and pins by absolute path.

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
/// Underscores on purpose (§4 #9): the id↔tool boundary is `_`.
const ALLOWED_TOOL: &str = "list_institutional_reports";
const ALLOWED_TOOL_2: &str = "get_report_detail";
/// Served upstream but NOT in `tools_allow` — must never materialize.
const DENIED_TOOL: &str = "admin_purge";

/// How many characters of the API key sit past the truncation boundary in the
/// `EchoQueryIn4xx` fixture — i.e. exactly how long a prefix the clamp-first
/// order would leak. Kept well above "a couple of characters" so the assertion
/// is about a usable credential fragment, not a coincidence.
const KEY_STRADDLE_TAIL: usize = 16;

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
    /// Healthy, but slow on `initialize` only: never answer that one (so the
    /// client pays its full PER-REQUEST timeout), then answer `tools/list`
    /// promptly. `initialize` is explicitly best-effort, so this upstream is
    /// perfectly usable — it must come up Running. It cannot when the outer
    /// bring-up bound equals ONE request's timeout.
    HangInitialize,
    /// Answer every request with a 4xx whose body **echoes the request's own
    /// query string**, padded so the API key inside it straddles the kernel's
    /// `MAX_UPSTREAM_DETAIL_CHARS` truncation boundary. This is the upstream
    /// behaviour the scrub-before-truncate rule exists for, and the only way
    /// to reach the production expression pair end-to-end.
    EchoQueryIn4xx,
    /// Healthy bring-up, then a `tools/call` that takes
    /// [`SLOW_TOOLS_CALL`] to answer — far longer than any bring-up budget a
    /// manifest is allowed to ask for. This is the report-generating tool the
    /// call timeout exists for; it must succeed.
    SlowToolsCall,
    /// Healthy, but echoes the request's own query string (API key and all)
    /// into every `tools/list` description and every `tools/call` result. The
    /// success-path leak the scrub layer exists for.
    EchoQueryInResults,
}

/// How long [`StubMode::SlowToolsCall`] takes to answer a `tools/call`.
const SLOW_TOOLS_CALL: Duration = Duration::from_millis(1_500);

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
        // Wrapped so each per-connection task can take it. Connections are
        // served CONCURRENTLY: a mode that stalls one request must not stop
        // the stub from answering the client's next connection, which is the
        // whole point of `HangInitialize`.
        let gate = Arc::new(tokio::sync::Mutex::new(gate));

        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let queries = Arc::clone(&queries);
                let methods = Arc::clone(&methods);
                let received = Arc::clone(&received);
                let gate = Arc::clone(&gate);
                tokio::spawn(async move {
                    let (target, body) = match read_request(&mut sock).await {
                        Some(v) => v,
                        None => return,
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
                        let taken = gate.lock().await.take();
                        if let Some(rx) = taken {
                            let _ = rx.await;
                        }
                    }

                    if mode == StubMode::EchoQueryIn4xx {
                        let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
                        // Place the key so it STARTS `KEY_STRADDLE_TAIL` chars
                        // before the cap and runs past it: clamp-first leaves
                        // exactly that many characters of a live credential in
                        // the message, scrub-first leaves none.
                        let key_at = query.find(SECRET_VALUE).unwrap_or(0);
                        let pad = MAX_UPSTREAM_DETAIL_CHARS - KEY_STRADDLE_TAIL - key_at;
                        let body = format!("{}{query} rejected", "x".repeat(pad));
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

                    // What an upstream that quotes our own request back looks
                    // like on the SUCCESS path. Includes the API key verbatim.
                    let echo = if mode == StubMode::EchoQueryInResults {
                        format!(
                            " [upstream saw ?{}]",
                            target.split_once('?').map(|(_, q)| q).unwrap_or("")
                        )
                    } else {
                        String::new()
                    };

                    let result = match method.as_str() {
                        "initialize" => json!({
                            "protocolVersion": "2025-06-18",
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": format!("stub-mcp{echo}"), "version": "0.8.4" }
                        }),
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
                });
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

/// The two budgets a connector manifest carries, kept together so a test that
/// cares about only one still has to say what the other is (round-4 finding A:
/// they are separate constraints and conflating them is the whole defect).
#[derive(Clone, Copy)]
struct Budgets {
    /// `mcp_http.request_timeout_ms` — the steady-state `tools/call` budget.
    call_ms: u64,
    /// `mcp_http.bringup_timeout_ms`. `None` ⇒ omit the field, exercising the
    /// derived `min(call_ms, ceiling)` default.
    bringup_ms: Option<u64>,
}

impl Budgets {
    /// The pre-split shape: one number, and the bring-up budget derived from it.
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
        self.host_with_disabled(Vec::new())
    }

    /// [`Self::host`] with `config.plugins_disabled` populated — the operator's
    /// kill switch. #1196 S1 review r5: the rotate regression r4 found was only
    /// reachable for ids on this list, so an HTTP gate for it needs a host that
    /// has one.
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
        let credential = calm_server::plugin_host::HttpCredential::parse(SECRET_VALUE)
            .expect("the fixture credential must satisfy the HTTP-credential rules");
        let client = calm_server::plugin_host::HttpMcpClient::new(
            CONNECTOR_ID,
            manifest.mcp_http.as_ref().unwrap(),
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

// ===========================================================================
// §4 #4 — `cli-query` execution (#1164 P3)
//
// The connector under test is a shell script the test writes into a temp dir
// and pins by ABSOLUTE path, which is what an operator does for a real query
// CLI. Nothing here touches the network and nothing depends on a binary being
// installed on the runner.
// ===========================================================================

const CLI_ID: &str = "cli-longbridge";
const CLI_TOOL: &str = "quote";

/// Write an executable script and return its absolute path.
fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// Install a `cli-query` connector directory inside `plugins_dir` (D9: the
/// source must live there so install hits the `src == dst` short-circuit).
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

/// §4 #1/#4 for `cli-query`: install + enable → Running, with the declared tool
/// visible through the same boot-audit read every other connector test uses.
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

    // The tool materialized BEFORE the live insert (§2.7(1)) — the same
    // structural witness the mcp-http path is held to.
    let order = state
        .plugin
        .connector_spawn_order(CLI_ID)
        .expect("a connector spawn must record its ordering");
    assert!(
        order.materialized_before_live_insert(),
        "materialization must precede the live insert: {order:?}"
    );

    // …and it is visible through the exact read the boot audit performs.
    let tools = boot_audit_tool_names(&state.plugin).await;
    assert!(
        tools.contains(&format!("{CLI_ID}::{CLI_TOOL}")),
        "the declared tool must be visible: {tools:?}"
    );

    // The client is the new variant, and the command was pinned absolute.
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

/// §4 #4 — calling the materialized tool actually runs the binary and returns
/// its stdout, with the argument substituted as one whole argv element.
#[tokio::test]
async fn cli_query_tools_call_runs_the_binary_and_returns_its_stdout() {
    let b = boot().await;
    let bin = tempfile::tempdir().unwrap();
    // Prints each argv element on its own line, so the test can see EXACTLY
    // how the template was rendered — one element, not a shell-split string.
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

/// A non-zero exit is the CHILD's verdict, not a transport failure: the call
/// succeeds and carries `isError: true` plus whatever the command printed.
/// Reporting it as an `Err` would make "the query found nothing" and "the
/// kernel could not run the query" indistinguishable to an agent.
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

/// Design R5 — an unresolvable bare command is a 503 whose reason names the
/// service PATH and the directories searched. The case this exists for is a
/// docker preview stack that simply has no such binary; "command not found"
/// alone tells the operator nothing about where the kernel looked.
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

    // Observable, like every other failed connector.
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

/// A `secret_env` key with no matching secret is a bring-up failure whose
/// reason names the key and the file — and never the value of any secret.
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

/// §2.5 — `neige.*` callbacks and forge dispatch stay app-only for the new
/// variant too. Widening either would hand a manifest-declared local binary the
/// kernel's inbound callback surface.
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
    // #1196 S1 review r5 — the exact code, not just "some 4xx". `is_client_error`
    // would have stayed green if the id had started answering 400 (or, once the
    // `plugins_disabled` variant below is in play, anything else in the 4xx
    // range); the contract this cell owes is specifically 404.
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

// ===========================================================================
// #1196 S1 review r5 — the rotate error table, driven end to end over HTTP
// ===========================================================================

/// `POST /api/plugins/{id}/rotate-token` answers the exact documented status for
/// both cells that are reachable at the HTTP layer, **with the ids on the
/// operator's kill switch** — which is the combination the r4 regression needed.
///
/// Why this test and not the two halves that already existed. `a20`
/// (`plugin_lifecycle_lock.rs`) pins which `HostError` each cell produces;
/// `routes::plugins::rotate_error_mapping_tests` pins which status each
/// `HostError` maps to. Both are real, but the conjunction of two tests is an
/// argument, not an observation: nothing ran the route. The two tests above in
/// this file *do* drive the route, but neither has a `plugins_disabled` entry,
/// so neither could see the r4 defect — the shared pre-lock probe answered
/// `Disabled` (→ 500) only for ids on that list.
///
/// Why only two cells. `a20`'s third rotate cell (a *registered app* on the kill
/// switch, which legitimately reaches the delete and then 500s) is host-only by
/// nature, and its GHOST cell is not reachable here in `a20`'s form: with no
/// `plugins` row the route's own `plugin_get_by_id` answers 404 before the host
/// is called at all, so the mapping would not be under test. The cell that IS
/// reachable is "row present, registry absent" — an uninstall mid-flight, a
/// manifest that failed to load, a moved `plugins_dir` — and that is what this
/// drives.
///
/// Mutation witnesses (each applied alone to `routes::plugins::rotate_error_to_calm`):
/// * fold the `HostError::NotFound` arm into the `other` catch-all → the ghost
///   cell goes red (500, owed 404);
/// * fold the `HostError::UnsupportedForKind` arm into the catch-all → the
///   connector cell goes red (500, owed 400).
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
    // …and an id with a `plugins` row but no manifest on disk, so the registry
    // cannot know it.
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

    // 400 ms PER REQUEST × 2 round trips + 500 ms slack = a 1.3 s ceiling on
    // this connector's bring-up. The assertion is 3 s, not 2 s: round 2 raised
    // the per-connector cap from 400 ms to 1.3 s without moving this number,
    // which left the co-installed app plugin ~0.7 s of headroom on a loaded
    // box. What this fails on is an UNBOUNDED bring-up — without the
    // per-request deadlines a hung upstream never returns at all, and without
    // the outer `tokio::time::timeout` the parts of the round trip that sit
    // outside ureq's clock are uncapped — and 3 s still fails loudly for
    // either, since both are unbounded rather than "a bit slower".
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

    // The rest of boot happened.
    assert!(
        host.running_plugin_ids().await.contains("app-echo"),
        "one unreachable connector must not stop other plugins from starting"
    );
}

/// Round-2 regression: the fix that introduced the single outer bound set it
/// to `request_timeout_ms` — ONE request's worth — while `connect_mcp_http`
/// makes TWO round trips. A healthy upstream that merely stalls on
/// `initialize` (which is explicitly best-effort, and which the probed server
/// class does not implement at all) therefore burned the entire budget on a
/// call whose failure is supposed to be ignored, and landed `Unavailable`.
///
/// This upstream serves `tools/list` correctly and must come up **Running**.
#[tokio::test]
async fn a_slow_but_healthy_upstream_still_comes_up_running() {
    let stub = StubServer::start(StubMode::HangInitialize).await;
    let b = boot().await;
    // `initialize` will consume this whole per-request budget before the
    // client gives up on it; `tools/list` then answers immediately. Total
    // wall-clock ≈ 1× timeout, which is over the old (1× timeout) outer bound
    // and comfortably inside the new one.
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
    // And the tools are really there.
    let tools = boot_audit_tool_names(&host).await;
    assert!(tools.iter().any(|t| t.ends_with(ALLOWED_TOOL)), "{tools:?}");
}

/// Boot latency must not scale with the number of unreachable connectors.
///
/// The per-connector timeout bounds ONE bring-up; `autospawn_enabled` iterates
/// serially and `AppState::new` awaits it inline, so N dead connectors used to
/// cost N × that bound. The connector portion of the loop now carries one
/// overall budget. This drives the loop with several hung connectors and
/// asserts the total stays near a single connector's cost, not N × it.
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

    // Drive the REAL loop with a budget small enough to observe it firing.
    // Production supplies `CONNECTOR_AUTOSPAWN_BUDGET` (30 s) through
    // `autospawn_enabled`, which is a one-line delegate to this function;
    // bounding 30 s from outside would make this a 30 s test that could not
    // tell the loop bound from the per-connector one.
    let budget = Duration::from_secs(2);
    let started = Instant::now();
    tokio::time::timeout(
        Duration::from_secs(60),
        host.autospawn_enabled_within(budget),
    )
    .await
    .expect("boot autospawn never returned");
    let elapsed = started.elapsed();

    // Serial-and-unbounded is ≈ N × (2 × 900ms + 500ms slack) ≈ 14s. Bounded,
    // it stops at the budget plus at most one in-flight connector's own cap.
    // 8 s fails loudly if the loop bound is removed and still passes with the
    // per-connector bound doing its job.
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

/// Round-3 F1: the loop budget and the per-connector cap were two independent
/// numbers, and `mcp_http.request_timeout_ms` has no upper bound — so a large
/// enough timeout made the LOOP bound fire first at boot while `POST /enable`
/// (which has no loop budget) used the connector's own cap. Two answers for
/// one manifest, and the boot-side reason blamed "earlier connectors" that do
/// not exist.
///
/// The connector here is the ONLY one installed and its own cap
/// (2 × 400 ms + 500 ms = 1.3 s) is deliberately larger than the loop budget
/// it is given. Boot must nonetheless refuse it for its own upstream failure,
/// with the same reason `/enable` gives. Removing the `max(...)` widening in
/// `autospawn_enabled_within` makes the two reasons differ and fails here.
#[tokio::test]
async fn boot_and_enable_agree_when_one_connector_outlasts_the_loop_budget() {
    let stub = StubServer::start(StubMode::Hang).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 400, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    // Smaller than this connector's own 1.3 s cap — the shape that used to
    // guarantee a loop-budget refusal for a lone connector.
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

/// Round-3 F0: `autospawn_enabled_within` wraps its `tokio::time::timeout`
/// around the WHOLE spawn — including `spawn_mcp_http`'s live-table insert,
/// which is what publishes `Running`. A budget elapsing after that insert but
/// before the trailing `emit_state(Running)` completes used to land in the
/// timeout arm and unconditionally overwrite the live entry with
/// `Unavailable`: the connector was genuinely up (client live, tools already
/// in the registry) yet dropped out of `running_plugin_ids`, every
/// materialized tool went invisible, and a false failure was broadcast.
///
/// Driven deterministically, without racing anything. The stub gates its
/// `tools/list` reply; the test takes the repo's write transaction before
/// releasing that gate, so the spawn runs to completion through the live
/// insert and then parks in `emit_state(Running)`'s `log_pure_event` — the
/// exact window. The budget is then allowed to elapse inside it.
#[tokio::test]
async fn a_connector_that_came_up_is_not_overwritten_by_the_elapsing_boot_budget() {
    let (release_gate, gate) = oneshot::channel::<()>();
    let stub = StubServer::start_gated(StubMode::Normal, Some(gate)).await;
    let b = boot().await;
    // 3 s per bring-up request ⇒ a 6.5 s per-connector cap, which the loop
    // budget below must exceed: the point of this test is the LOOP bound firing
    // on a connector that already came up, not the per-connector one.
    //
    // **The 3 s is flake headroom, and it is deliberately not milliseconds.**
    // The stub gates `tools/list` while this clock runs, and between
    // `wait_for_tools_list()` and `release_gate` the test polls at 5 ms
    // granularity, spawns a task, checks out a pool connection (running
    // `after_connect`), executes `BEGIN IMMEDIATE`, and round-trips a oneshot.
    // At the old 1 s that whole sequence had to finish inside one second on a
    // loaded box, and blowing it produced a timeout that looked exactly like a
    // real regression.
    write_connector_with(&b.plugins_dir, &stub.url(), Budgets::uniform(3_000), 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;

    // > (2 × 3 s + 500 ms slack) + 500 ms, so `autospawn_enabled_within` uses
    // it verbatim rather than widening it (see the F1 fix).
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

    // Still Running once everything has drained.
    let status = host.status(CONNECTOR_ID).await.expect("runtime entry");
    assert!(
        matches!(status.status, PluginRuntimeStatus::Running),
        "{:?}",
        status.status
    );
}

// ===========================================================================
// Round-4 finding A — the bring-up budget and the tools/call budget are two
// knobs with opposite constraints
// ===========================================================================

/// Boot must stay bounded **however large the operator's `tools/call` budget
/// is**, without a `min`/`max` juggling act at the spawn site.
///
/// Before the split, `connector_bringup_budget` read `request_timeout_ms`, and
/// `autospawn_enabled_within` then widened the loop budget to fit it — so
/// `"request_timeout_ms": 600000` against a black-holed upstream stalled
/// `AppState::new` for 2 × 600 s + slack ≈ 20.5 minutes, during which the
/// server does not serve. The value here is absurd on purpose.
///
/// Mutation witness: point `connector_bringup_budget` back at `timeout_ms()`
/// and this test stops finishing at all — the outer `tokio::time::timeout`
/// fires instead of the assertion.
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

/// The bound the test above exercises for one fixture, stated as the invariant
/// it actually is: **no manifest that loads** can make one connector's bring-up
/// cap exceed [`MAX_CONNECTOR_BRINGUP_BUDGET`].
///
/// This is what "bounded by construction" has to mean after three rounds of
/// adjusting constants. It drives the real `Manifest::parse` (so a value the
/// validator refuses cannot be smuggled in) and the real
/// `connector_bringup_budget` (so the formula and the constant cannot drift).
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

    // ---- …and the OTHER connector kind. --------------------------------
    //
    // #1164 P3 F9: the claim above is universal ("no manifest that loads"),
    // but every fixture so far is `mcp_http`, so `connector_bringup_budget`'s
    // `cli-query` arm was never reached and the quantifier was only asserted
    // over half its domain. `cli_query.timeout_ms` is the (uncapped) tools/call
    // budget — a manifest naming a ten-minute one must still yield the fixed
    // `CLI_QUERY_BRINGUP_BUDGET`.
    let mut cli_loaded = 0;
    for extra in [
        json!({}),
        json!({ "timeout_ms": 600_000 }),
        json!({ "timeout_ms": u32::MAX }),
        json!({ "timeout_ms": u64::MAX }),
        json!({ "timeout_ms": 0, "max_output_bytes": usize::MAX }),
        json!({ "search_path_extra": ["/opt/lb/bin"], "env_allow": ["TZ"] }),
    ] {
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
}

/// The other half of the same split: a `tools/call` that runs far longer than
/// any legal bring-up budget must still succeed.
///
/// Mutation witness: make `tools_call` spend `Phase::Bringup` and this fails
/// with a transport timeout — 1.5 s of upstream work against a 400 ms bring-up
/// deadline.
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

// ===========================================================================
// Round-4 finding B — the success path is scrubbed after parsing
// ===========================================================================

/// An upstream that quotes our own query string back inside `tools/list`
/// descriptions and `tools/call` results is the success-path leak: those
/// strings become `ExposedTool` entries agents read and wave-transcript
/// payloads. Nothing here is an error path, so `MAX_UPSTREAM_DETAIL_CHARS` and
/// the 4xx arm are not involved — this is the JSON-tree scrub.
#[tokio::test]
async fn a_success_path_that_echoes_the_query_never_leaks_the_key() {
    let stub = StubServer::start(StubMode::EchoQueryInResults).await;
    let b = boot().await;
    write_connector(&b.plugins_dir, &stub.url(), 5_000, 0o600);
    let host = b.host();
    seed_row(&b, CONNECTOR_ID).await;
    host.spawn(CONNECTOR_ID).await.expect("connector spawns");

    // The fixture really did put the key on the wire AND echo it back.
    assert!(
        stub.queries().iter().any(|q| q.contains(SECRET_VALUE)),
        "the fixture must actually send the key: {:?}",
        stub.queries()
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

    // 2. The `tools/call` result — what reaches the wave transcript.
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

// ===========================================================================
// Round-4 finding C — the reconcile decision and its emission must agree
// ===========================================================================

/// `publish_unavailable` returning `false` is a SNAPSHOT taken under the
/// process-table lock and then released. The old code emitted `Running` after
/// that release, so a `stop()` landing in the window removed the entry and
/// emitted `Disabled` — and this stale `Running` then overwrote it. The
/// persisted and broadcast state said a connector with no client and no tools
/// was running.
///
/// Driven without racing anything: the connector is stopped **before**
/// `reaffirm_running` is called, so the emission has to notice on its own that
/// its decision no longer holds.
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

    // The operator disables it. This is the concurrent `stop()` of the race,
    // resolved to its completed form so the test is deterministic.
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

/// The mirror case, so the test above cannot pass by never emitting: a
/// connector that IS up and not stopping gets its `Running` re-announced, which
/// is the whole reason the arm exists (the dropped spawn future never reached
/// its own `emit_state`).
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

/// Every `PluginState` state string recorded for [`CONNECTOR_ID`], oldest
/// first. Reads the persisted log rather than the live table, because the
/// defect being pinned is about what was PERSISTED and BROADCAST.
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

/// Round-3 F3: the scrub-before-truncate rule had only a unit test over the
/// two free functions, so swapping the production lines in `request`'s
/// blocking closure left the suite green. This drives the real `spawn` against
/// an upstream that answers 4xx with a body echoing its own query string,
/// padded so the API key straddles `MAX_UPSTREAM_DETAIL_CHARS`. Clamp-then-
/// scrub leaves `KEY_STRADDLE_TAIL` characters of a live credential in
/// `last_error`; scrub-then-clamp leaves none.
#[tokio::test]
async fn a_4xx_body_echoing_the_query_never_leaks_a_partial_key() {
    let stub = StubServer::start(StubMode::EchoQueryIn4xx).await;
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
        stub.queries().iter().any(|q| q.contains(SECRET_VALUE)),
        "the fixture must actually put the key on the wire: {:?}",
        stub.queries()
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

// ===========================================================================
// #1196 acceptance 5 (connector half) — uninstall vs an in-flight spawn
//
// Replaces `uninstall_during_an_in_flight_spawn_does_not_resurrect_the_registry_entry`,
// which reached in and called `registry_remove()` directly and carried the
// comment "there is no per-plugin lifecycle lock (risk R12)". There is one now,
// so that comment was about to become a lie and the test was about to stop
// describing anything a caller can do.
// ===========================================================================

/// The composite `uninstall` operation, run against a spawn that is on the
/// wire, must be refused **with nothing done** — and must then succeed on an
/// explicit retry.
///
/// The barrier is named: `StubServer::start_gated` holds the connector's
/// `tools/list` reply, which pins `spawn_under` inside the guard. Without it
/// the spawn could simply finish first and the uninstall would succeed on the
/// first call, and every assertion below would still pass — a test that never
/// once observed the lock. Hence the explicit `plugin_busy` assertion.
///
/// Mutation witness: drop the `try_lock_lifecycle` from `PluginHost::spawn`
/// and the first `uninstall` returns 204 instead of 409.
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

// ===========================================================================
// #1196 acceptance 8 — reload vs an in-flight spawn
// ===========================================================================

/// `reload` landing on a connector whose spawn is on the wire must be refused
/// with the manifest untouched; the retry must actually re-point the connector
/// at the new endpoint.
///
/// Barrier: the FIRST stub's gated `tools/list`. Terminal assertion: the new
/// endpoint really received `initialize` / `tools/list`, and the new
/// allow-list is what materialized — not merely "the reload returned 200".
///
/// Mutation witness: drop the `try_lock_lifecycle` from `reload` and the first
/// call returns 200, having stopped a connector another task believes it owns.
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

// ===========================================================================
// Round-5 finding 1 — the boot bound must be TOTAL
// ===========================================================================

/// A slow event store must not hold boot, however many connectors are enabled.
///
/// The previous bound wrapped `spawn` only. Everything the loop did *after* it
/// — `publish_unavailable` for connectors the budget never reached,
/// `publish_unavailable`/`reaffirm_running` in the timeout arm — was an
/// unbounded persisted emission, performed serially, once per connector. So a
/// stalled event store still stalled `AppState::new`, scaling with connector
/// count, with every "bound" in the file green.
///
/// Driven deterministically: a foreign `BEGIN IMMEDIATE` holds the DB writer
/// for far longer than the ceiling, so EVERY emission the loop attempts parks.
/// Boot must still return inside the ceiling the loop computes for itself —
/// `connector_phase_ceiling(widened_connector_budget(budget, widest))`, which
/// for this fixture is 1.9 s, not the 1.5 s an earlier version of this comment
/// claimed by forgetting the widening.
///
/// Mutation witness: fence only `self.spawn(...)` again (i.e. drop the
/// `timeout_at(phase_deadline, …)` wrapper) and this returns after the DB
/// holder releases, ~8 s, not ~2 s.
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

    // The ceiling asserted below is COMPUTED from the two production
    // expressions the loop itself evaluates — `widened_connector_budget`, then
    // `connector_phase_ceiling` — and never restated as a number here.
    //
    // Restating it was the defect: this test used to comment that a 1 s budget
    // "is used as given", derive a 1.5 s ceiling from that, and then allow 3 s.
    // The loop really adopts `widest per-connector cap + slack` = (2 × 200 ms +
    // 500 ms) + 500 ms = 1.4 s and fences the phase at 1.9 s, so a run that took
    // 2.04 s passed a test whose comment claimed it pinned 1.5 s. A second
    // arithmetic beside production's is how that happens; there is now one.
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
    // Both assertions are needed, and neither substitutes for the other. The
    // computed `ceiling` tracks whatever the loop really does, so the timing
    // assertion below cannot go stale — but a formula-following test cannot
    // detect a formula that DRIFTS: if `widened_connector_budget` quietly grew
    // another 500 ms, runtime and expectation would move together and the
    // timing assertion would still pass. So the composed number is also pinned
    // as a literal here: any change to the formula has to be acknowledged by
    // editing this line, rather than being silently absorbed.
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
    // The loop must actually have run out its budget: with four hanging
    // connectors and a DB writer nobody can take, a run materially faster than
    // the budget means the fixture stopped exercising the fence, and the upper
    // bound below would then be satisfied by a loop that did nothing.
    assert!(
        elapsed >= BUDGET,
        "boot returned in {elapsed:?}, faster than the {BUDGET:?} budget it was \
         given — the hang fixture is no longer in force"
    );
    // …and the real bound, tightly. The tolerance covers scheduling jitter
    // around the fence and nothing else: measured overshoot on this box is
    // ~4 ms (1.9039 s against the 1.9 s ceiling), and the old 1.5 s allowance
    // is what let a 2.04 s run pass a test claiming a 1.5 s bound. Anything
    // that widens the phase by a whole step — a re-widened budget, an emission
    // escaping the fence — moves elapsed by hundreds of ms and fails here.
    const JITTER: Duration = Duration::from_millis(250);
    assert!(
        elapsed < ceiling + JITTER,
        "boot took {elapsed:?} against a {ceiling:?} connector-phase ceiling \
         (+{JITTER:?} jitter allowance)"
    );

    // Observability is NOT what was given up: every connector still has a
    // terminal live entry, because that half of the transition is a synchronous
    // table write with no await in it.
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

/// The ceiling that is *documented* and the ceiling that is *computed* are one
/// expression. Rounds 1-4 stated 30 s and then 30.5 s in prose while the code
/// computed something else, because the prose was a second arithmetic.
///
/// This pins the composed number, so any change to a constant that feeds it has
/// to be an explicit decision here rather than a silent drift. Since
/// `MAX_CONNECTOR_AUTOSPAWN_WALL` is now literally
/// `connector_phase_ceiling(widened_connector_budget(...))`, the 31.5 s literal
/// below also pins `widened_connector_budget` itself — previously the constant
/// inlined its own copy of that `max` and nothing pinned the helper.
///
/// **#1196 S1 — the new lifecycle lock does not enter this formula, and that is
/// not self-evident.** Every acquisition on the boot path happens *inside*
/// `autospawn_enabled_within`'s `timeout_at` fence: `autospawn_one` takes it (or
/// waits for it) within the fenced iteration body, and the budget-exhausted arm
/// uses the **synchronous** `try_lock_lifecycle`, which cannot await and gives
/// up immediately when the lock is held. So no acquisition can extend the phase
/// past the fence, and the ceiling below is unchanged.
#[test]
fn the_connector_phase_ceiling_is_the_documented_one() {
    use calm_server::plugin_host::{
        CONNECTOR_AUTOSPAWN_BUDGET, MAX_CONNECTOR_AUTOSPAWN_WALL, MAX_CONNECTOR_BRINGUP_BUDGET,
        connector_phase_ceiling,
    };

    // 2 × 15 s (the validated per-request bring-up ceiling) + 500 ms slack.
    assert_eq!(MAX_CONNECTOR_BRINGUP_BUDGET, Duration::from_millis(30_500));
    // …widened by another slack so the per-connector bound fires first, and
    // then the reconcile tail. This is the number the docs state.
    assert_eq!(MAX_CONNECTOR_AUTOSPAWN_WALL, Duration::from_millis(31_500));
    // The wall is exactly the ceiling of the widest budget the loop can adopt,
    // never a hand-computed constant beside it.
    assert_eq!(
        MAX_CONNECTOR_AUTOSPAWN_WALL,
        connector_phase_ceiling(MAX_CONNECTOR_BRINGUP_BUDGET + Duration::from_millis(500))
    );
    // And the floor is a floor: the widened budget is never below the constant.
    assert!(MAX_CONNECTOR_AUTOSPAWN_WALL > connector_phase_ceiling(CONNECTOR_AUTOSPAWN_BUDGET));
}

// ===========================================================================
// #1196 acceptance 3 — the bad interleaving from the issue, mutation-driven
//
// Replaces `two_emitters_for_one_connector_never_interleave`, which asserted
// `peak_concurrent_state_emits() == 1`. Under the lifecycle lock that probe is
// vacuous: `stop` is now refused at the *entry*, so it never reaches an
// emission at all, and the peak would read 1 even with every emission lock
// deleted. The probe and its accessor have been retired with it.
// ===========================================================================

/// The #1196 interleaving, driven through two real paths.
///
/// The spawn is pinned with its live `Running` entry already published and its
/// `running` emission not yet committed (gated stub for the first half, a held
/// DB writer transaction for the second). A real `stop` runs in that window.
///
/// * it must be refused with `LifecycleBusy` and change **nothing**;
/// * on an explicit retry after the spawn completes, the event log's tail must
///   be `running` → `disabled`, and the live table must be empty.
///
/// Mutation witness: delete the `try_lock_lifecycle` line from
/// `PluginHost::spawn`. The `stop` then succeeds inside the window, commits
/// `disabled` first, and the parked spawn commits `running` afterwards — the
/// last word becomes `running` for a connector with no live entry, and the
/// final assertion fails.
///
/// Barrier note: the held `write_in_tx` blocks **every** write in the database,
/// not just this plugin's — including autocommit writes issued by anything else
/// in the same fixture. Do not drive a second plugin here, and do not copy this
/// barrier into a test that needs a row to change during the window (it cannot:
/// `sqlite::memory:` gives readers no snapshot isolation, so the blocked write
/// simply never commits and both orderings look identical).
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

    // Let the spawn finish its network half: it materializes, publishes the
    // live `Running` entry, and then parks inside its `running` emission —
    // still holding the lifecycle guard.
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

    // The operator disables it right there.
    //
    // Bounded on purpose. The refusal is non-blocking, so a correct `stop`
    // answers immediately; a `stop` that got *into* the critical section would
    // park on the held DB writer instead, and an unbounded `.await` here would
    // turn the mutation's red into a hang with no message.
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

// ===========================================================================
// Round-5 finding 3 — a number-shaped credential is refused at the source
// ===========================================================================

/// `scrub_value` deliberately does not descend into JSON numbers, so an
/// upstream that echoes a number-shaped credential back as `{"k": 12345678}` in
/// `structuredContent` would put it in the tool result and the wave transcript
/// with nothing to redact. Round 4 classified that as an accepted residual;
/// it is a disclosure. The credential is refused instead.
///
/// The rule is the JSON number **grammar**, not the lexical shape "all digits"
/// it was first written as: `-1234567` is just as unscrubbable and used to be
/// accepted here. Both spellings are driven through the real `spawn` (and
/// therefore the real `connect_mcp_http` → `HttpCredential::parse` boundary),
/// not through the validator in isolation.
///
/// Mutation witness: narrow `is_number_shaped` back to
/// `raw.chars().all(char::is_ascii_digit)` and the `-1234567` case comes up
/// `Running` with the credential on the wire.
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
