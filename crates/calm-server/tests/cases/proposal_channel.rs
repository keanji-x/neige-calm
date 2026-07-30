//! Issue #955 §5 (PR-b) — acceptance oracle for the ④ proposal channel.
//!
//! Drives the two halves against ONE repo + bus, the way production wires
//! them: the plugin side through `plugin_host::callbacks::dispatch` (real
//! `CallbackCtx`, real manifest gate, real write transactions) and the
//! adjudication side through the assembled axum protected router (real
//! session + actor middleware).
//!
//! What each test pins, and why it is the interesting assertion:
//!
//! | test | invariant |
//! |---|---|
//! | `submit_then_accept_applies_and_attributes_to_the_plugin` | §5.2.2 + §5.3 + §5.6: the report actually changes, the decision and the change share one transaction, and the edit is attributed `author = "plugin"` + `author_plugin_id` — with EXACTLY one `CardUpdated`/`WaveReportEdited` pair |
//! | `withdraw_releases_the_quota_slot` | §5.2/§5.6: a plugin can always dig itself out of a full pending quota |
//! | `spec_edit_between_submit_and_accept_resolves_stale` | §5.2.1/§5.6: the authoritative anchoring verdict happens inside the accept tx, and a stale verdict leaves the report untouched |
//! | `pending_quota_is_enforced_at_submit` | §5.2: the ceiling is real, not advisory |
//! | `idem_key_dedup_returns_the_original_proposal` | §5.2: pending-scoped idempotency writes nothing and answers with the original id |
//! | `non_user_rest_caller_cannot_adjudicate` | §5.1/§5.4: accept/reject are the human's alone |
//! | `another_plugins_proposal_cannot_be_withdrawn` | §5.4: the factual ownership half of the withdraw gate |
//! | `channel_is_closed_without_the_manifest_grant` | §5.2: empty `permissions.proposals` closes all three methods, including the read |

#![cfg(unix)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EditAuthor;
use calm_server::event::EventBus;
use calm_server::ids::{ActorId, CardId, WaveId};
use calm_server::model::{CardRole, NewCard, NewCove, NewPlugin, NewWave};
use calm_server::plugin_host::PluginHost;
use calm_server::plugin_host::callbacks::{CallbackCtx, dispatch};
use calm_server::plugin_host::manifest::Manifest;
use calm_server::plugin_host::mcp::{McpClient, RpcError};
use calm_server::plugin_host::registry::PluginRegistry;
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::wave_cove_cache::WaveCoveCache;
use calm_server::wave_report::{WaveReportPayload, persist_report, resolve_report_for_wave};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tower::ServiceExt;

const PLUGIN_ID: &str = "dev.neige.proposer";
const OTHER_PLUGIN_ID: &str = "dev.neige.bystander";

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Boot {
    state: AppState,
    auth_state: AuthState,
    repo: Arc<dyn Repo>,
    route_repo: Arc<dyn calm_server::db::RouteRepo>,
    registry: Arc<PluginRegistry>,
    events: Arc<EventBus>,
    mcp: Arc<McpClient>,
    subs: Arc<Mutex<Vec<calm_server::plugin_host::callbacks::SubscriptionRecord>>>,
    write: WriteContext,
    wave_id: WaveId,
    report_card_id: CardId,
}

/// Manifest granting `permissions.proposals: ["report"]` — the ④ channel's
/// only gate. `grant` false leaves the field absent (deny by default).
fn manifest(id: &str, grant: bool) -> Manifest {
    let permissions = if grant {
        json!({ "proposals": ["report"], "events_subscribe": ["*"] })
    } else {
        json!({ "events_subscribe": ["*"] })
    };
    let json = json!({
        "manifest_version": 1,
        "id": id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Proposer",
        "entrypoint": { "command": "bin/stub" },
        "permissions": permissions,
    });
    Manifest::parse(&json.to_string()).expect("manifest parses")
}

/// Minimal in-process MCP peer: answers `initialize`, then echoes empty
/// results. The proposal callbacks never push notifications, so this only
/// needs to exist for `CallbackCtx` to be constructible.
async fn stub_mcp_client() -> Arc<McpClient> {
    let (kernel, plugin) = tokio::io::duplex(64 * 1024);
    let (k_r, k_w) = tokio::io::split(kernel);
    let (p_r, p_w) = tokio::io::split(plugin);
    tokio::spawn(async move {
        let mut reader = BufReader::new(p_r);
        let mut writer = p_w;
        let mut buf = String::new();
        loop {
            buf.clear();
            if reader.read_line(&mut buf).await.unwrap_or(0) == 0 {
                return;
            }
            let Ok(v) = serde_json::from_str::<Value>(buf.trim()) else {
                continue;
            };
            let Some(id) = v.get("id").cloned() else {
                continue;
            };
            let result = if v.get("method").and_then(Value::as_str) == Some("initialize") {
                json!({
                    "protocolVersion": "2025-11-25",
                    "serverInfo": { "name": "stub", "version": "0.0.0" },
                    "capabilities": {}
                })
            } else {
                json!({})
            };
            let mut s =
                serde_json::to_string(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
                    .unwrap();
            s.push('\n');
            let _ = writer.write_all(s.as_bytes()).await;
            let _ = writer.flush().await;
        }
    });
    McpClient::connect_with_auth(k_r, k_w, None)
        .await
        .expect("stub connect")
}

async fn boot() -> Boot {
    boot_with_grant(true).await
}

async fn boot_with_grant(grant: bool) -> Boot {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    for id in [PLUGIN_ID, OTHER_PLUGIN_ID] {
        repo.plugin_install(NewPlugin {
            id: id.into(),
            version: "0.1.0".into(),
            install_path: format!("/tmp/{id}"),
            manifest: json!({}),
            enabled: true,
            user_config: json!({}),
        })
        .await
        .expect("seed plugin row");
    }
    let cove = repo
        .cove_create(NewCove {
            name: "proposals".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id.clone(),
            title: "proposal wave".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let report_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "wave-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(WaveReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();

    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(
        report_card.id.clone(),
        CardRole::ReportCard,
        wave.id.clone(),
    );
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let write = WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone());

    let events = EventBus::new();
    let registry = Arc::new(PluginRegistry::empty());
    registry.insert(manifest(PLUGIN_ID, grant), None);
    registry.insert(manifest(OTHER_PLUGIN_ID, grant), None);

    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::clone(&registry),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-proposal-channel"),
            Vec::new(),
            events.clone(),
            write.clone(),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        Some(wave_cove_cache),
    );
    let auth_state = AuthState::new(AuthConfig {
        username: Some("alice".into()),
        password: Some("hunter2".into()),
        dev_autologin: false,
        display_name: "alice".into(),
    });
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();

    Boot {
        state,
        auth_state,
        repo,
        route_repo,
        registry,
        events: Arc::new(events),
        mcp: stub_mcp_client().await,
        subs: Arc::new(Mutex::new(Vec::new())),
        write,
        wave_id: wave.id,
        report_card_id: report_card.id,
    }
}

impl Boot {
    fn ctx(&self, plugin_id: &str) -> CallbackCtx<'_> {
        CallbackCtx {
            plugin_id: static_plugin_id(plugin_id),
            repo: Arc::clone(&self.route_repo),
            event_bus: Arc::clone(&self.events),
            registry: Arc::clone(&self.registry),
            mcp: Arc::clone(&self.mcp),
            subscriptions: Arc::clone(&self.subs),
            call_id: None,
            write: self.write.clone(),
        }
    }

    async fn call(&self, plugin_id: &str, method: &str, params: Value) -> Result<Value, RpcError> {
        dispatch(&self.ctx(plugin_id), method, params).await
    }

    /// Current report blocks straight from the persisted CRDT projection.
    async fn blocks(&self) -> Vec<calm_server::wave_report::ReportBlock> {
        let (_, card, _) = resolve_report_for_wave(self.route_repo.as_ref(), self.wave_id.as_str())
            .await
            .expect("resolve report");
        let payload: WaveReportPayload =
            serde_json::from_value(card.payload.clone()).expect("payload decodes");
        payload.blocks.unwrap_or_default()
    }

    async fn body(&self) -> String {
        let (_, card, _) = resolve_report_for_wave(self.route_repo.as_ref(), self.wave_id.as_str())
            .await
            .expect("resolve report");
        let payload: WaveReportPayload =
            serde_json::from_value(card.payload.clone()).expect("payload decodes");
        payload.body
    }

    /// Every persisted event, in log order, as `(kind, payload)`.
    async fn event_log(&self) -> Vec<(String, Value)> {
        self.repo
            .events_since(0, i64::MAX)
            .await
            .expect("read events")
            .into_iter()
            .map(|(_, _, _, event)| (event.kind_tag().to_string(), event.payload_value()))
            .collect()
    }

    /// A spec-authored edit that touches the report's LAST block — the
    /// "someone moved the anchor under the proposal" half of the stale
    /// test. Goes through the production writer, so the block's rev really
    /// bumps (an appended non-H1 line extends the trailing block rather
    /// than minting a new one).
    async fn spec_append_to_last_block(&self, extra: &str) {
        let (wave, card, payload) =
            resolve_report_for_wave(self.route_repo.as_ref(), self.wave_id.as_str())
                .await
                .expect("resolve report");
        let mut next = payload.clone();
        next.body = format!("{}\n{extra}\n", payload.body.trim_end());
        persist_report(
            self.route_repo.as_ref(),
            &self.state.events,
            &self.write,
            ActorId::AiCodex(self.report_card_id.clone()),
            EditAuthor::Spec,
            wave,
            card,
            payload,
            next,
            None,
            None,
            false,
        )
        .await
        .expect("spec edit applies");
    }
}

/// `CallbackCtx::plugin_id` borrows for the ctx's lifetime; the tests only
/// ever pass one of the two `'static` id constants, so this maps a
/// caller-supplied `&str` back onto the constant it names (no unsafe, no
/// lifetime laundering).
fn static_plugin_id(s: &str) -> &'static str {
    match s {
        PLUGIN_ID => PLUGIN_ID,
        OTHER_PLUGIN_ID => OTHER_PLUGIN_ID,
        other => panic!("unknown test plugin id {other}"),
    }
}

// ---------------------------------------------------------------------------
// REST helpers
// ---------------------------------------------------------------------------

fn app(state: AppState, auth_state: AuthState) -> axum::Router {
    let protected_rest = routes::protected_router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            auth_state.clone(),
            auth::require_session,
        ));
    axum::Router::new()
        .merge(protected_rest)
        .merge(routes::public_router())
        .with_state(state)
        .merge(auth::router().with_state(auth_state))
}

async fn login(app: &axum::Router) -> String {
    let body = serde_json::to_vec(&json!({ "username": "alice", "password": "hunter2" })).unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "login must succeed");
    let raw = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie")
        .to_str()
        .unwrap();
    raw.split(';').next().unwrap().to_string()
}

/// One authenticated REST call. `actor` `None` = default (`user`).
async fn rest(
    app: &axum::Router,
    cookie: &str,
    method: &str,
    uri: &str,
    actor: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie);
    if let Some(actor) = actor {
        builder = builder.header("X-Calm-Actor", actor);
    }
    let resp = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

// ---------------------------------------------------------------------------
// Callback helpers
// ---------------------------------------------------------------------------

/// `neige.report.get` → `(blocks, doc_heads)`.
async fn report_get(b: &Boot, plugin_id: &str) -> (Vec<Value>, String) {
    let out = b
        .call(
            plugin_id,
            "neige.report.get",
            json!({ "wave_id": b.wave_id.as_str() }),
        )
        .await
        .expect("report.get succeeds");
    let blocks = out["blocks"].as_array().cloned().expect("blocks array");
    let heads = out["doc_heads"].as_str().expect("doc_heads").to_string();
    (blocks, heads)
}

fn submit_params(b: &Boot, heads: &str, ops: Value, idem: &str) -> Value {
    json!({
        "subject": { "kind": "report", "wave_id": b.wave_id.as_str() },
        "base_doc_heads": heads,
        "ops": ops,
        "note": "proposed by the acceptance oracle",
        "idem_key": idem,
    })
}

/// Replace an existing block's prose — the anchored-op shape the stale
/// test needs.
fn replace_op(block_id: &str, if_rev: u32, markdown: &str) -> Value {
    json!([{
        "op": "upsert_block",
        "block_id": block_id,
        "kind": "prose",
        "payload": { "markdown": markdown },
        "if_rev": if_rev,
    }])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn submit_then_accept_applies_and_attributes_to_the_plugin() {
    let b = boot().await;
    let app = app(b.state.clone(), b.auth_state.clone());
    let cookie = login(&app).await;

    let (blocks, heads) = report_get(&b, PLUGIN_ID).await;
    assert!(!blocks.is_empty(), "seed report must have a block");
    let block_id = blocks[0]["id"].as_str().unwrap().to_string();
    let rev = blocks[0]["rev"].as_u64().unwrap() as u32;

    // Two ops: replace the seed block AND create a new one anchored after
    // it via a temp id, so the kernel-minted-id path is exercised too.
    let ops = json!([
        {
            "op": "upsert_block",
            "block_id": block_id,
            "kind": "prose",
            "payload": { "markdown": "# Findings\n\nrewritten by the app\n" },
            "if_rev": rev,
        },
        {
            "op": "upsert_block",
            "temp_id": "extra",
            "kind": "prose",
            "payload": { "markdown": "# Appendix\n\nappended by the app\n" },
            "anchor": { "after_block_id": block_id },
        }
    ]);
    let out = b
        .call(
            PLUGIN_ID,
            "neige.proposal.submit",
            submit_params(&b, &heads, ops, "k1"),
        )
        .await
        .expect("submit succeeds");
    let proposal_id = out["proposal_id"]
        .as_str()
        .expect("proposal_id")
        .to_string();

    // It shows up as pending for the human.
    let (status, listed) = rest(
        &app,
        &cookie,
        "GET",
        &format!("/api/waves/{}/proposals", b.wave_id.as_str()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["proposals"].as_array().unwrap().len(), 1);
    assert_eq!(listed["proposals"][0]["proposal_id"], proposal_id);
    assert_eq!(listed["proposals"][0]["plugin_id"], PLUGIN_ID);

    let events_before = b.event_log().await.len();

    let (status, resolved) = rest(
        &app,
        &cookie,
        "POST",
        &format!("/api/proposals/{proposal_id}/accept"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resolved}");
    assert_eq!(resolved["decision"], "accepted", "{resolved}");

    // The report really changed, and the temp block got a durable id.
    let body = b.body().await;
    assert!(body.contains("rewritten by the app"), "{body}");
    assert!(body.contains("appended by the app"), "{body}");
    let after = b.blocks().await;
    assert_eq!(after.len(), 2, "{after:?}");
    assert!(
        after.iter().all(|blk| blk.id.starts_with("b_")),
        "kernel-minted ids only: {after:?}"
    );

    // §5.3/§5.6 — the accept transaction emitted EXACTLY one
    // CardUpdated + one WaveReportEdited, plus the one decision event.
    let log = b.event_log().await;
    let tail = &log[events_before..];
    let kinds: Vec<&str> = tail.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        kinds.iter().filter(|k| **k == "card.updated").count(),
        1,
        "exactly one CardUpdated: {kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| **k == "wave.report_edited").count(),
        1,
        "exactly one WaveReportEdited: {kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| **k == "proposal.resolved").count(),
        1,
        "exactly one decision: {kinds:?}"
    );
    let edited = tail
        .iter()
        .find(|(k, _)| k == "wave.report_edited")
        .map(|(_, p)| p)
        .expect("report edited event");
    assert_eq!(edited["author"], "plugin", "{edited}");
    assert_eq!(edited["author_plugin_id"], PLUGIN_ID, "{edited}");
    let decision = tail
        .iter()
        .find(|(k, _)| k == "proposal.resolved")
        .map(|(_, p)| p)
        .expect("decision event");
    assert_eq!(decision["decision"], "accepted");
    assert_eq!(decision["plugin_id"], PLUGIN_ID, "submitter attribution");

    // Re-accepting is a 409 (in-tx pending re-check).
    let (status, _) = rest(
        &app,
        &cookie,
        "POST",
        &format!("/api/proposals/{proposal_id}/accept"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    // And it left the pending list.
    let (_, listed) = rest(
        &app,
        &cookie,
        "GET",
        &format!("/api/waves/{}/proposals", b.wave_id.as_str()),
        None,
    )
    .await;
    assert!(listed["proposals"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn pending_quota_is_enforced_at_submit() {
    let b = boot().await;
    let (blocks, heads) = report_get(&b, PLUGIN_ID).await;
    let block_id = blocks[0]["id"].as_str().unwrap().to_string();
    let rev = blocks[0]["rev"].as_u64().unwrap() as u32;

    for i in 0..4 {
        b.call(
            PLUGIN_ID,
            "neige.proposal.submit",
            submit_params(
                &b,
                &heads,
                replace_op(&block_id, rev, &format!("# v{i}\n\nbody {i}\n")),
                &format!("k{i}"),
            ),
        )
        .await
        .unwrap_or_else(|e| panic!("submit {i} must succeed: {e:?}"));
    }
    let err = b
        .call(
            PLUGIN_ID,
            "neige.proposal.submit",
            submit_params(
                &b,
                &heads,
                replace_op(&block_id, rev, "# v5\n\nover quota\n"),
                "k5",
            ),
        )
        .await
        .expect_err("5th pending proposal must be refused");
    assert_eq!(err.code, -32003, "quota code: {err:?}");
    assert!(err.message.contains("pending proposals"), "{err:?}");
}

#[tokio::test]
async fn withdraw_releases_the_quota_slot() {
    let b = boot().await;
    let (blocks, heads) = report_get(&b, PLUGIN_ID).await;
    let block_id = blocks[0]["id"].as_str().unwrap().to_string();
    let rev = blocks[0]["rev"].as_u64().unwrap() as u32;

    let mut ids = Vec::new();
    for i in 0..4 {
        let out = b
            .call(
                PLUGIN_ID,
                "neige.proposal.submit",
                submit_params(
                    &b,
                    &heads,
                    replace_op(&block_id, rev, &format!("# v{i}\n\nbody {i}\n")),
                    &format!("k{i}"),
                ),
            )
            .await
            .expect("submit");
        ids.push(out["proposal_id"].as_str().unwrap().to_string());
    }
    // Quota is full.
    assert!(
        b.call(
            PLUGIN_ID,
            "neige.proposal.submit",
            submit_params(&b, &heads, replace_op(&block_id, rev, "# x\n\nx\n"), "k5"),
        )
        .await
        .is_err(),
        "quota must be full before withdraw"
    );
    // Withdraw one → the slot comes back.
    let out = b
        .call(
            PLUGIN_ID,
            "neige.proposal.withdraw",
            json!({ "proposal_id": ids[0] }),
        )
        .await
        .expect("withdraw succeeds");
    assert_eq!(out, json!({}));
    b.call(
        PLUGIN_ID,
        "neige.proposal.submit",
        submit_params(&b, &heads, replace_op(&block_id, rev, "# x\n\nx\n"), "k5"),
    )
    .await
    .expect("5th submit succeeds after withdraw");

    // The withdrawal is in the log with the plugin as actor + submitter.
    let log = b.event_log().await;
    let withdrawn = log
        .iter()
        .find(|(k, p)| k == "proposal.resolved" && p["decision"] == "withdrawn")
        .map(|(_, p)| p)
        .expect("withdrawn event");
    assert_eq!(withdrawn["proposal_id"], ids[0]);
    assert_eq!(withdrawn["plugin_id"], PLUGIN_ID);

    // Withdrawing twice is a conflict, not a silent success.
    let err = b
        .call(
            PLUGIN_ID,
            "neige.proposal.withdraw",
            json!({ "proposal_id": ids[0] }),
        )
        .await
        .expect_err("double withdraw must conflict");
    assert_eq!(err.code, -32409, "{err:?}");
}

#[tokio::test]
async fn spec_edit_between_submit_and_accept_resolves_stale() {
    let b = boot().await;
    let app = app(b.state.clone(), b.auth_state.clone());
    let cookie = login(&app).await;

    let (blocks, heads) = report_get(&b, PLUGIN_ID).await;
    let last = blocks.last().expect("seed report has blocks");
    let block_id = last["id"].as_str().unwrap().to_string();
    let rev = last["rev"].as_u64().unwrap() as u32;
    let out = b
        .call(
            PLUGIN_ID,
            "neige.proposal.submit",
            submit_params(
                &b,
                &heads,
                replace_op(&block_id, rev, "# App\n\nthe app's version\n"),
                "k1",
            ),
        )
        .await
        .expect("submit");
    let proposal_id = out["proposal_id"].as_str().unwrap().to_string();

    // The spec touches the very block the proposal anchored on.
    b.spec_append_to_last_block("a spec sentence the app never saw")
        .await;
    let body_after_spec = b.body().await;
    let events_before = b.event_log().await.len();

    let (status, resolved) = rest(
        &app,
        &cookie,
        "POST",
        &format!("/api/proposals/{proposal_id}/accept"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resolved}");
    assert_eq!(resolved["decision"], "stale", "{resolved}");
    assert!(
        resolved["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("conflict"),
        "reason explains the failed anchor: {resolved}"
    );

    // The report is untouched and NO report event was emitted.
    assert_eq!(b.body().await, body_after_spec, "report must not change");
    let tail: Vec<String> = b
        .event_log()
        .await
        .split_off(events_before)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(
        tail,
        vec!["proposal.resolved".to_string()],
        "stale is one event, no report change: {tail:?}"
    );
}

#[tokio::test]
async fn idem_key_dedup_returns_the_original_proposal() {
    let b = boot().await;
    let (blocks, heads) = report_get(&b, PLUGIN_ID).await;
    let block_id = blocks[0]["id"].as_str().unwrap().to_string();
    let rev = blocks[0]["rev"].as_u64().unwrap() as u32;

    let first = b
        .call(
            PLUGIN_ID,
            "neige.proposal.submit",
            submit_params(&b, &heads, replace_op(&block_id, rev, "# a\n\na\n"), "same"),
        )
        .await
        .expect("first submit");
    let before = b.event_log().await.len();
    // Same key, DIFFERENT content: the pending proposal wins, and nothing
    // new is written.
    let second = b
        .call(
            PLUGIN_ID,
            "neige.proposal.submit",
            submit_params(&b, &heads, replace_op(&block_id, rev, "# b\n\nb\n"), "same"),
        )
        .await
        .expect("replay returns the original id");
    assert_eq!(first["proposal_id"], second["proposal_id"]);
    assert_eq!(
        b.event_log().await.len(),
        before,
        "an idempotent replay must not append an event"
    );

    // Resolving releases the key: the next submit under it mints a new id.
    b.call(
        PLUGIN_ID,
        "neige.proposal.withdraw",
        json!({ "proposal_id": first["proposal_id"].as_str().unwrap() }),
    )
    .await
    .expect("withdraw");
    let third = b
        .call(
            PLUGIN_ID,
            "neige.proposal.submit",
            submit_params(&b, &heads, replace_op(&block_id, rev, "# c\n\nc\n"), "same"),
        )
        .await
        .expect("key released after resolution");
    assert_ne!(first["proposal_id"], third["proposal_id"]);
}

#[tokio::test]
async fn non_user_rest_caller_cannot_adjudicate() {
    let b = boot().await;
    let app = app(b.state.clone(), b.auth_state.clone());
    let cookie = login(&app).await;
    let (blocks, heads) = report_get(&b, PLUGIN_ID).await;
    let block_id = blocks[0]["id"].as_str().unwrap().to_string();
    let rev = blocks[0]["rev"].as_u64().unwrap() as u32;
    let out = b
        .call(
            PLUGIN_ID,
            "neige.proposal.submit",
            submit_params(&b, &heads, replace_op(&block_id, rev, "# a\n\na\n"), "k1"),
        )
        .await
        .expect("submit");
    let proposal_id = out["proposal_id"].as_str().unwrap().to_string();

    for verb in ["accept", "reject"] {
        let (status, body) = rest(
            &app,
            &cookie,
            "POST",
            &format!("/api/proposals/{proposal_id}/{verb}"),
            Some("ai:codex"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{verb}: {body}");
    }
    // Still pending, and the human can still resolve it.
    let (status, resolved) = rest(
        &app,
        &cookie,
        "POST",
        &format!("/api/proposals/{proposal_id}/reject"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resolved}");
    assert_eq!(resolved["decision"], "rejected");
}

#[tokio::test]
async fn another_plugins_proposal_cannot_be_withdrawn() {
    let b = boot().await;
    let (blocks, heads) = report_get(&b, PLUGIN_ID).await;
    let block_id = blocks[0]["id"].as_str().unwrap().to_string();
    let rev = blocks[0]["rev"].as_u64().unwrap() as u32;
    let out = b
        .call(
            PLUGIN_ID,
            "neige.proposal.submit",
            submit_params(&b, &heads, replace_op(&block_id, rev, "# a\n\na\n"), "k1"),
        )
        .await
        .expect("submit");
    let proposal_id = out["proposal_id"].as_str().unwrap().to_string();

    let err = b
        .call(
            OTHER_PLUGIN_ID,
            "neige.proposal.withdraw",
            json!({ "proposal_id": proposal_id }),
        )
        .await
        .expect_err("another plugin must not withdraw this proposal");
    assert_eq!(err.code, -32001, "permission-denied code: {err:?}");
    // Untouched: the owner can still withdraw it.
    b.call(
        PLUGIN_ID,
        "neige.proposal.withdraw",
        json!({ "proposal_id": proposal_id }),
    )
    .await
    .expect("owner withdraw still works");
}

#[tokio::test]
async fn channel_is_closed_without_the_manifest_grant() {
    let b = boot_with_grant(false).await;
    // The read baseline is gated too (§5.2) — a plugin that cannot
    // propose has no reason to read the report.
    let err = b
        .call(
            PLUGIN_ID,
            "neige.report.get",
            json!({ "wave_id": b.wave_id.as_str() }),
        )
        .await
        .expect_err("report.get denied without the grant");
    assert_eq!(err.code, -32001, "{err:?}");
    let err = b
        .call(
            PLUGIN_ID,
            "neige.proposal.submit",
            submit_params(&b, "ah1:whatever", json!([]), "k1"),
        )
        .await
        .expect_err("submit denied without the grant");
    assert_eq!(err.code, -32001, "{err:?}");
    let err = b
        .call(
            PLUGIN_ID,
            "neige.proposal.withdraw",
            json!({ "proposal_id": "pp_nope" }),
        )
        .await
        .expect_err("withdraw denied without the grant");
    assert_eq!(err.code, -32001, "{err:?}");
}

#[tokio::test]
async fn terminal_wave_refuses_new_proposals() {
    // §5.5 submit-time checkpoint: a terminal wave's report is closed.
    let b = boot().await;
    let (_, heads) = report_get(&b, PLUGIN_ID).await;
    let (wave, card, payload) = resolve_report_for_wave(b.route_repo.as_ref(), b.wave_id.as_str())
        .await
        .expect("resolve report");
    // Drive the wave to a terminal lifecycle through the report writer's
    // lifecycle hook (the production path).
    persist_report(
        b.route_repo.as_ref(),
        &b.state.events,
        &b.write,
        ActorId::User,
        EditAuthor::User,
        wave,
        card,
        payload.clone(),
        payload,
        None,
        // Draft → Canceled is the user-authorized terminal edge.
        Some(calm_server::model::WaveLifecycle::Canceled),
        false,
    )
    .await
    .expect("terminalize wave");

    let err = b
        .call(
            PLUGIN_ID,
            "neige.proposal.submit",
            submit_params(
                &b,
                &heads,
                json!([{
                    "op": "upsert_block",
                    "temp_id": "t",
                    "kind": "prose",
                    "payload": { "markdown": "# x\n\nx\n" },
                    "anchor": "at_end",
                }]),
                "k1",
            ),
        )
        .await
        .expect_err("terminal wave must refuse a proposal");
    assert!(err.message.contains("terminal"), "{err:?}");
}
