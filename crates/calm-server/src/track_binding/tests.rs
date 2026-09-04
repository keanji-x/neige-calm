//! #1321 S1 — owner-binding drift: reproduction + acceptance.
//!
//! Every track in this file is minted by the **real create route**
//! (`POST /api/tracks`), not by `Repo::track_create`. That is the whole point
//! of the file: the pre-existing unit test
//! `planner_harness_start_adapter::tests::bound_template_descriptor_filters_running_trusted_template_binding`
//! hand-built a row with `template_id = Some(_) ∧ template_input = Some(_) ∧
//! plugin_scope = None`, a combination the create route cannot produce
//! (`routes::tracks::create_track` writes `plugin_scope =
//! bound_plugin.map(|m| m.id)` and `validate_template_input_binding` refuses
//! `template_input` without a bound plugin), so it never exercised the
//! divergence between the two owner readers.
//!
//! The trusted set is process-env (`NEIGE_TRUSTED_FORGE_PLUGINS`); nextest
//! runs one process per test, and every test here takes a guard that restores
//! the previous value on drop.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::FromRef;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};
use tower::ServiceExt;

use crate::card_role_cache::CardRoleCache;
use crate::db::prelude::*;
use crate::db::sqlite::SqlxRepo;
use crate::event::EventBus;
use crate::forge_trust::trusted_forge_plugin;
use crate::harness::HarnessRegistry;
use crate::mcp_server::registry::AppContext;
use crate::mcp_server::tool_visibility::{TrackPluginScope, plugin_scope_for_track};
use crate::model::NewPlugin;
use crate::operation::planner_harness_start_adapter::PlannerHarnessStartAdapter;
use crate::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use crate::routes;
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::state::{AppState, CodexClient, DaemonClient, RouteState, WriteContext};
use crate::track_area_cache::TrackAreaCache;

/// `CARGO_BIN_EXE_*` is only set for integration-test targets, so the lib's
/// own unit tests locate the stub the same way
/// `planner_harness_start_adapter::tests` does.
fn stub_echo_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_plugin-host-stub-echo") {
        return path.into();
    }
    if let Some(path) = option_env!("CARGO_BIN_EXE_plugin-host-stub-echo") {
        return path.into();
    }
    let current = std::env::current_exe().expect("current test executable");
    let deps_dir = current.parent().expect("test executable parent");
    let debug_dir = deps_dir.parent().expect("target debug dir");
    let candidate = debug_dir.join("plugin-host-stub-echo");
    assert!(
        candidate.exists(),
        "missing plugin-host-stub-echo at {}",
        candidate.display()
    );
    candidate
}

/// Both owners declare **this** roster id — `POST /api/tracks` only admits ids
/// in `templates::TEMPLATES`, so the shared id has to be a real one.
const SHARED_TEMPLATE_ID: &str = crate::templates::SMALL_CHANGE;

const OWNER_A: &str = "dev.trusted-owner-a";
const OWNER_B: &str = "dev.trusted-owner-b";

/// One registry entry: plugin id, the template ids it declares, and the
/// plugin-level `input_schema` (`Manifest::input_schema`, the thing
/// `template_input` is validated against).
#[derive(Clone)]
struct OwnerSpec {
    id: &'static str,
    templates: Vec<&'static str>,
    input_schema: Option<Value>,
}

fn owner(id: &'static str, templates: &[&'static str], input_schema: Option<Value>) -> OwnerSpec {
    OwnerSpec {
        id,
        templates: templates.to_vec(),
        input_schema,
    }
}

/// A's contract: `issue_url` required. B's: `spec_url` required, with
/// `additionalProperties: false`, so an A-validated input is *invalid* under
/// B. `owner_schemas_actually_disagree` pins that this fixture is not vacuous.
fn owner_a_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "issue_url": { "type": "string" } },
        "required": ["issue_url"],
        "additionalProperties": false
    })
}

fn owner_b_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "spec_url": { "type": "string" } },
        "required": ["spec_url"],
        "additionalProperties": false
    })
}

fn owner_a_input() -> Value {
    json!({ "issue_url": "https://github.com/o/r/issues/1321" })
}

fn both_owners() -> Vec<OwnerSpec> {
    vec![
        owner(OWNER_A, &[SHARED_TEMPLATE_ID], Some(owner_a_input_schema())),
        owner(OWNER_B, &[SHARED_TEMPLATE_ID], Some(owner_b_input_schema())),
    ]
}

/// The trusted set is a process-global env var, so these tests are mutually
/// exclusive within a process. Under nextest (process-per-test, what CI runs)
/// the lock is uncontended; under `cargo test` it is what keeps two guards
/// from clobbering each other.
static TRUST_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

struct TrustGuard {
    previous: Option<String>,
    expected: Vec<String>,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

impl TrustGuard {
    async fn trust(ids: &str) -> Self {
        let lock = TRUST_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let previous = std::env::var("NEIGE_TRUSTED_FORGE_PLUGINS").ok();
        // SAFETY: the lock above serializes this module's writers, and nextest
        // additionally gives every test its own process.
        unsafe { std::env::set_var("NEIGE_TRUSTED_FORGE_PLUGINS", ids) };
        Self {
            previous,
            expected: ids.split(',').map(str::to_string).collect(),
            _lock: lock,
        }
    }

    /// Re-assert the load-bearing property — *every* plugin this test needs
    /// trusted is still trusted — immediately before each decisive assertion.
    ///
    /// Without it the failure mode of a clobbered env var is a **vacuous
    /// pass**: with the successor plugin no longer trusted, "the planner must
    /// not adopt it" holds for entirely the wrong reason.
    fn check(&self) {
        for id in &self.expected {
            assert!(
                trusted_forge_plugin(id),
                "`{id}` must still be trusted; NEIGE_TRUSTED_FORGE_PLUGINS is \
                 process-global and something else in this process changed it"
            );
        }
    }
}

impl Drop for TrustGuard {
    fn drop(&mut self) {
        match self.previous.as_deref() {
            Some(previous) => unsafe { std::env::set_var("NEIGE_TRUSTED_FORGE_PLUGINS", previous) },
            None => unsafe { std::env::remove_var("NEIGE_TRUSTED_FORGE_PLUGINS") },
        }
    }
}

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<SqlxRepo>,
    host: Arc<PluginHost>,
    area_id: String,
    plugins_dir: PathBuf,
    plugins_data_dir: PathBuf,
    _tmp: tempfile::TempDir,
}

fn manifest_json_for(spec: &OwnerSpec, version: &str) -> Value {
    let mut manifest_json = json!({
        "manifest_version": 2,
        "id": spec.id,
        "version": version,
        "min_kernel_version": "0.0.1",
        "display_name": "Owner binding stub",
        "entrypoint": { "command": "bin/stub" },
        "templates": spec
            .templates
            .iter()
            .map(|id| json!({ "id": id }))
            .collect::<Vec<_>>(),
        "permissions": {}
    });
    if let Some(schema) = spec.input_schema.as_ref() {
        manifest_json["input_schema"] = schema.clone();
    }
    manifest_json
}

/// Registration is not spawning — every test drives `spawn` / `stop` itself,
/// because "who is running" is the fact the binding turns on.
fn build_host(
    repo: Arc<dyn Repo>,
    plugins_dir: &Path,
    plugins_data_dir: &Path,
    owners: &[OwnerSpec],
    version: &str,
    caches: (CardRoleCache, TrackAreaCache),
) -> Arc<PluginHost> {
    let mut builder = PluginRegistry::builder();
    for spec in owners {
        let install_dir = plugins_dir.join(spec.id);
        let manifest = Manifest::parse(&manifest_json_for(spec, version).to_string())
            .expect("owner manifest parses");
        builder = builder.with(manifest, Some(install_dir));
    }
    Arc::new(PluginHost::new_full(
        Arc::new(builder.build()),
        repo,
        plugins_dir.to_path_buf(),
        plugins_data_dir.to_path_buf(),
        Vec::new(),
        EventBus::new(),
        WriteContext::new(caches.0, caches.1),
    ))
}

async fn boot(owners: &[OwnerSpec]) -> Boot {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    repo_dyn
        .seed_track_area_cache(&track_area_cache)
        .await
        .expect("seed track-area cache");
    let area = repo_dyn
        .area_create(crate::model::NewArea {
            name: "1321-s1".into(),
            color: "#101010".into(),
            sort: None,
        })
        .await
        .expect("create area");

    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    std::fs::create_dir_all(&plugins_data_dir).expect("create plugins data dir");
    for spec in owners {
        let bin_dir = plugins_dir.join(spec.id).join("bin");
        std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
        std::os::unix::fs::symlink(stub_echo_bin(), bin_dir.join("stub"))
            .expect("symlink echo stub");
        repo_dyn
            .plugin_install(NewPlugin {
                id: spec.id.to_string(),
                version: "0.1.0".into(),
                install_path: plugins_dir.join(spec.id).display().to_string(),
                manifest: manifest_json_for(spec, "0.1.0"),
                enabled: true,
                user_config: json!({}),
            })
            .await
            .expect("seed plugin row");
    }

    let host = build_host(
        repo_dyn.clone(),
        &plugins_dir,
        &plugins_data_dir,
        owners,
        "0.1.0",
        (card_role_cache.clone(), track_area_cache.clone()),
    );

    let state = AppState::from_parts(
        repo_dyn.clone(),
        EventBus::new(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        host.clone(),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        Some(track_area_cache),
    );
    let app = routes::router()
        .layer(axum::middleware::from_fn(crate::actor::actor_middleware))
        .with_state(state.clone());

    Boot {
        app,
        state,
        repo,
        host,
        area_id: area.id.to_string(),
        plugins_dir,
        plugins_data_dir,
        _tmp: tmp,
    }
}

impl Boot {
    /// `POST /api/tracks` — the only production writer of
    /// `tracks.{template_id, template_input, plugin_scope}`.
    async fn create_track(&self, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri("/api/tracks")
            .header("content-type", "application/json")
            .header("X-Calm-Actor", "user")
            .body(Body::from(body.to_string()))
            .expect("build request");
        let response = self.app.clone().oneshot(request).await.expect("route");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn create_bound_track(&self, template_input: Option<Value>) -> String {
        let mut body = json!({
            "area_id": self.area_id,
            "title": "owner binding",
            "template_id": SHARED_TEMPLATE_ID,
            "theme": { "fg": [216, 219, 226], "bg": [15, 20, 24] },
        });
        if let Some(input) = template_input {
            body["template_input"] = input;
        }
        let (status, created) = self.create_track(body).await;
        assert_eq!(status, StatusCode::CREATED, "create failed: {created}");
        created["id"].as_str().expect("track id").to_string()
    }

    async fn stored_plugin_scope(&self, track_id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT plugin_scope FROM tracks WHERE id = ?1")
            .bind(track_id)
            .fetch_one(self.repo.pool())
            .await
            .expect("select plugin_scope")
    }

    /// Model a plugin **upgrade**: same id, same install dir, a new manifest.
    /// A fresh `PluginHost` over the new registry is how a test reaches that
    /// state without a `pub(in crate::plugin_host)` registry mutator; the
    /// readers only ever consult the host they are handed.
    fn upgraded_host(&self, owners: &[OwnerSpec]) -> Arc<PluginHost> {
        build_host(
            self.repo.clone() as Arc<dyn Repo>,
            &self.plugins_dir,
            &self.plugins_data_dir,
            owners,
            "0.2.0",
            (CardRoleCache::new(), TrackAreaCache::new()),
        )
    }

    fn adapter_on(&self, host: &Arc<PluginHost>) -> PlannerHarnessStartAdapter {
        let repo_dyn: Arc<dyn Repo> = self.repo.clone();
        PlannerHarnessStartAdapter::new(
            repo_dyn.clone(),
            SharedCodexAppServer::new_stub(repo_dyn),
            HarnessRegistry::new(),
            host.clone(),
            CardRoleCache::new(),
            TrackAreaCache::new(),
            None,
        )
    }

    fn mcp_ctx_on(&self, host: &Arc<PluginHost>) -> Arc<AppContext> {
        let repo_dyn: Arc<dyn Repo> = self.repo.clone();
        let route_repo: Arc<dyn crate::db::RouteRepo> = repo_dyn;
        let plugin_host = Arc::new(tokio::sync::OnceCell::new());
        plugin_host
            .set(host.clone())
            .map_err(|_| ())
            .expect("late-bound plugin host cell set once");
        Arc::new(AppContext {
            repo: route_repo,
            track_vcs: None,
            events: EventBus::new(),
            write: WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
            daemon_token_hash: None,
            gate_logs_dir: std::env::temp_dir().join("neige-1321-gate-logs"),
            plugin_host,
            operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
        })
    }

    fn route_state(&self) -> RouteState {
        RouteState::from_ref(&self.state)
    }
}

async fn spawn_on(host: &Arc<PluginHost>, plugin_id: &str) {
    host.spawn(plugin_id).await.expect("spawn plugin");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = host.status(plugin_id).await
            && matches!(status.status, PluginRuntimeStatus::Running)
        {
            return;
        }
        assert!(
            Instant::now() <= deadline,
            "plugin {plugin_id} did not reach Running"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

/// The joint assertion, over one host. `expected_owner = None` means "no
/// usable owner": the planner gets the vanilla prompt (no descriptor, no
/// input) and the MCP scope exposes zero plugin tools.
async fn assert_owner_agreement(
    boot: &Boot,
    host: &Arc<PluginHost>,
    track_id: &str,
    expected_owner: Option<&str>,
    trust: &TrustGuard,
) {
    trust.check();
    let bound = boot
        .adapter_on(host)
        .bound_template(track_id)
        .await
        .expect("bound_template must not error");
    let scope = plugin_scope_for_track(&boot.mcp_ctx_on(host), Some(track_id)).await;
    match expected_owner {
        Some(owner) => {
            assert!(
                bound.is_some(),
                "expected the planner to bind {owner}, got a vanilla prompt"
            );
            assert_eq!(
                scope,
                TrackPluginScope::Only(owner.to_string()),
                "expected the tool scope to be locked to {owner}"
            );
        }
        None => {
            assert!(
                bound.is_none(),
                "expected the planner to fall back to the vanilla prompt, got \
                 descriptor={:?} input={:?}",
                bound.as_ref().map(|b| b.descriptor.id.clone()),
                bound.as_ref().and_then(|b| b.input.clone()),
            );
            assert_eq!(
                scope,
                TrackPluginScope::None,
                "expected the tool scope to fail closed"
            );
        }
    }
}

/// The fixture is only meaningful if A's accepted input is genuinely rejected
/// by B's schema. Pinned separately so a later schema edit that quietly makes
/// the two compatible turns this file red here instead of turning the drift
/// tests vacuously green.
#[test]
fn owner_schemas_actually_disagree() {
    use crate::plugin_host::template_input::validate_template_input;
    validate_template_input(&owner_a_input_schema(), &owner_a_input())
        .expect("A's own input must validate under A's schema");
    let rejected = validate_template_input(&owner_b_input_schema(), &owner_a_input())
        .expect_err("A's input must NOT validate under B's schema");
    assert!(
        rejected.contains("spec_url") || rejected.contains("issue_url"),
        "the rejection should name the offending key: {rejected}"
    );
}

/// #1321 S1 reproduction — and acceptance ① + ②.
///
/// A binds the track at create time; A stops; B takes over the same template
/// id (`plugin_template_uniqueness`: a stopped trusted holder does not squat).
/// Before the fix, `bound_template` scanned *all* running trusted plugins for
/// `track.template_id` and therefore adopted **B**, injecting the input that
/// only A ever validated, while `plugin_scope_for_track` — reading
/// `track.plugin_scope` — stayed locked on the stopped **A** and failed
/// closed. Two readers, two owners.
#[tokio::test]
async fn planner_must_not_adopt_a_successor_owner_after_the_original_stops() {
    let _trust = TrustGuard::trust(&format!("{OWNER_A},{OWNER_B}")).await;
    let boot = boot(&both_owners()).await;
    spawn_on(&boot.host, OWNER_A).await;

    let track_id = boot.create_bound_track(Some(owner_a_input())).await;
    assert_eq!(
        boot.stored_plugin_scope(&track_id).await.as_deref(),
        Some(OWNER_A),
        "create must have recorded A as the owner"
    );

    boot.host.stop(OWNER_A).await.expect("stop A");
    spawn_on(&boot.host, OWNER_B).await;
    assert!(
        trusted_forge_plugin(OWNER_B),
        "B must be trusted for the takeover to be reachable"
    );

    _trust.check();
    let bound = boot
        .adapter_on(&boot.host)
        .bound_template(&track_id)
        .await
        .expect("bound_template must not error");
    assert!(
        bound.is_none(),
        "acceptance ①: the planner must not adopt B's descriptor for a track \
         owned by A; got descriptor={:?} input={:?}",
        bound.as_ref().map(|b| b.descriptor.id.clone()),
        bound.as_ref().and_then(|b| b.input.clone()),
    );

    let scope = plugin_scope_for_track(&boot.mcp_ctx_on(&boot.host), Some(track_id.as_str())).await;
    assert_eq!(
        scope,
        TrackPluginScope::None,
        "acceptance ②: the MCP tool scope must fail closed for a stopped owner"
    );

    boot.host.stop(OWNER_B).await.expect("stop B");
}

/// Acceptance ② stated as the invariant rather than as two separate values:
/// the two readers must never disagree about whether the track has a usable
/// owner. Runs the whole A→stop→B lifecycle and cross-checks at every step.
#[tokio::test]
async fn planner_and_tool_scope_agree_at_every_step_of_a_takeover() {
    let _trust = TrustGuard::trust(&format!("{OWNER_A},{OWNER_B}")).await;
    let boot = boot(&both_owners()).await;
    spawn_on(&boot.host, OWNER_A).await;
    let track_id = boot.create_bound_track(Some(owner_a_input())).await;

    // ① A running: both readers say "owned by A".
    assert_owner_agreement(&boot, &boot.host, &track_id, Some(OWNER_A), &_trust).await;

    // ② A stopped, nobody else running: both say "no usable owner".
    boot.host.stop(OWNER_A).await.expect("stop A");
    assert_owner_agreement(&boot, &boot.host, &track_id, None, &_trust).await;

    // ③ B took the id over: still "no usable owner" — B is not this track's
    //    owner, and neither reader may promote it.
    spawn_on(&boot.host, OWNER_B).await;
    assert_owner_agreement(&boot, &boot.host, &track_id, None, &_trust).await;

    // ④ A back: both say A again.
    boot.host.stop(OWNER_B).await.expect("stop B");
    spawn_on(&boot.host, OWNER_A).await;
    assert_owner_agreement(&boot, &boot.host, &track_id, Some(OWNER_A), &_trust).await;
    boot.host.stop(OWNER_A).await.expect("stop A");
}

/// Acceptance ③ — a track with `plugin_scope = NULL` stays unbound forever,
/// even once a trusted plugin declaring its `template_id` starts. The row is
/// produced the only way production can produce it: create the track while no
/// owner is running (so admission binds nothing), then start the plugin.
#[tokio::test]
async fn an_unbound_track_stays_unbound_when_a_declaring_plugin_starts_later() {
    let _trust = TrustGuard::trust(&format!("{OWNER_A},{OWNER_B}")).await;
    let boot = boot(&both_owners()).await;

    // Nobody running → the create route admits the roster id but binds no
    // plugin, so `plugin_scope` is NULL and `template_input` is refused.
    let track_id = boot.create_bound_track(None).await;
    assert_eq!(
        boot.stored_plugin_scope(&track_id).await,
        None,
        "an unowned create must leave plugin_scope NULL"
    );
    let stored_template_id: Option<String> =
        sqlx::query_scalar("SELECT template_id FROM tracks WHERE id = ?1")
            .bind(&track_id)
            .fetch_one(boot.repo.pool())
            .await
            .expect("select template_id");
    assert_eq!(
        stored_template_id.as_deref(),
        Some(SHARED_TEMPLATE_ID),
        "the row must still carry the template id — that is what makes this \
         the interesting case"
    );

    spawn_on(&boot.host, OWNER_A).await;
    _trust.check();
    let bound = boot
        .adapter_on(&boot.host)
        .bound_template(&track_id)
        .await
        .expect("bound_template must not error");
    assert!(
        bound.is_none(),
        "acceptance ③: an unbound track must not acquire an owner when a \
         plugin declaring its template id starts; got {:?}",
        bound.as_ref().map(|b| b.descriptor.id.clone())
    );
    // Unbound tracks keep the historical union of plugin tools (#1110 S4).
    assert_eq!(
        plugin_scope_for_track(&boot.mcp_ctx_on(&boot.host), Some(track_id.as_str())).await,
        TrackPluginScope::All,
        "acceptance ③: unbound stays unbound — the union, not a promotion"
    );
    boot.host.stop(OWNER_A).await.expect("stop A");
}

/// Acceptance ④ — the owner is unchanged and still running ∧ trusted, but its
/// Manifest no longer declares the track's `template_id` (a plugin upgrade
/// that dropped the template). Both readers must fail closed.
#[tokio::test]
async fn owner_that_stopped_declaring_the_template_id_fails_closed() {
    let _trust = TrustGuard::trust(OWNER_A).await;
    let owners = vec![owner(
        OWNER_A,
        &[SHARED_TEMPLATE_ID],
        Some(owner_a_input_schema()),
    )];
    let boot = boot(&owners).await;
    spawn_on(&boot.host, OWNER_A).await;
    let track_id = boot.create_bound_track(Some(owner_a_input())).await;
    assert_eq!(
        boot.stored_plugin_scope(&track_id).await.as_deref(),
        Some(OWNER_A)
    );
    boot.host.stop(OWNER_A).await.expect("stop A");

    // Upgrade A: same id, still trusted, still running — but it declares
    // `investigation` now instead of the track's template id.
    let upgraded = boot.upgraded_host(&[owner(
        OWNER_A,
        &[crate::templates::INVESTIGATION],
        Some(owner_a_input_schema()),
    )]);
    spawn_on(&upgraded, OWNER_A).await;
    assert!(
        trusted_forge_plugin(OWNER_A),
        "the owner is still trusted; only its template list changed"
    );

    _trust.check();
    let bound = boot
        .adapter_on(&upgraded)
        .bound_template(&track_id)
        .await
        .expect("bound_template must not error");
    assert!(
        bound.is_none(),
        "acceptance ④: an owner that no longer declares the template id must \
         not yield a binding; got {:?}",
        bound.as_ref().map(|b| b.descriptor.id.clone())
    );
    assert_eq!(
        plugin_scope_for_track(&boot.mcp_ctx_on(&upgraded), Some(track_id.as_str())).await,
        TrackPluginScope::None,
        "acceptance ④: the tool scope must fail closed with the binding"
    );
    upgraded.stop(OWNER_A).await.expect("stop upgraded A");
}

/// Acceptance ⑤ — same owner, same template id, but the owner's
/// `input_schema` changed and the persisted `template_input` no longer
/// satisfies it. The stale blob must not reach the planner prompt.
#[tokio::test]
async fn stale_template_input_is_revalidated_against_the_current_owner_schema() {
    let _trust = TrustGuard::trust(OWNER_A).await;
    let owners = vec![owner(
        OWNER_A,
        &[SHARED_TEMPLATE_ID],
        Some(owner_a_input_schema()),
    )];
    let boot = boot(&owners).await;
    spawn_on(&boot.host, OWNER_A).await;
    let track_id = boot.create_bound_track(Some(owner_a_input())).await;

    // The binding is live and carries the input while the schema still
    // accepts it — the control half of this test.
    let bound = boot
        .adapter_on(&boot.host)
        .bound_template(&track_id)
        .await
        .expect("bound_template must not error")
        .expect("the live owner must bind");
    assert_eq!(bound.input.as_ref(), Some(&owner_a_input()));
    boot.host.stop(OWNER_A).await.expect("stop A");

    // Owner upgrade: same plugin, same template id, incompatible schema.
    let upgraded = boot.upgraded_host(&[owner(
        OWNER_A,
        &[SHARED_TEMPLATE_ID],
        Some(owner_b_input_schema()),
    )]);
    spawn_on(&upgraded, OWNER_A).await;

    _trust.check();
    let bound = boot
        .adapter_on(&upgraded)
        .bound_template(&track_id)
        .await
        .expect("bound_template must not error");
    assert!(
        bound.is_none(),
        "acceptance ⑤: `template_input` validated against the old schema must \
         not be injected under the new one; got input={:?}",
        bound.as_ref().and_then(|b| b.input.clone())
    );
    upgraded.stop(OWNER_A).await.expect("stop upgraded A");
}

/// Create-time and run-time must answer the same question the same way: a
/// create issued *after* the owner stopped binds nothing
/// (`resolve_template_binding`), and the run-time readers agree.
#[tokio::test]
async fn create_time_and_run_time_binding_agree_for_a_stopped_owner() {
    let _trust = TrustGuard::trust(&format!("{OWNER_A},{OWNER_B}")).await;
    let boot = boot(&both_owners()).await;
    spawn_on(&boot.host, OWNER_A).await;
    let bound_track = boot.create_bound_track(Some(owner_a_input())).await;
    boot.host.stop(OWNER_A).await.expect("stop A");

    // Create time, owner gone: `template_input` is refused outright.
    let (status, body) = boot
        .create_track(json!({
            "area_id": boot.area_id,
            "title": "no owner",
            "template_id": SHARED_TEMPLATE_ID,
            "template_input": owner_a_input(),
            "theme": { "fg": [216, 219, 226], "bg": [15, 20, 24] },
        }))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "create must refuse template_input with no bound owner: {body}"
    );
    assert!(
        crate::routes::tracks::admit_template(&boot.route_state(), SHARED_TEMPLATE_ID)
            .await
            .expect("the roster id is still admissible")
            .binding
            .is_none(),
        "create-time binding must be None while the owner is stopped"
    );

    // Run time, same state: both readers fail closed on the older track.
    assert_owner_agreement(&boot, &boot.host, &bound_track, None, &_trust).await;
}
