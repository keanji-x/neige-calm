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
struct OwnerFixture {
    id: &'static str,
    templates: Vec<&'static str>,
    input_schema: Option<Value>,
}

fn owner(
    id: &'static str,
    templates: &[&'static str],
    input_schema: Option<Value>,
) -> OwnerFixture {
    OwnerFixture {
        id,
        templates: templates.to_vec(),
        input_schema,
    }
}

/// A's contract: `issue_url` required. B's: `plan_url` required, with
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
        "properties": { "plan_url": { "type": "string" } },
        "required": ["plan_url"],
        "additionalProperties": false
    })
}

fn owner_a_input() -> Value {
    json!({ "issue_url": "https://github.com/o/r/issues/1321" })
}

fn both_owners() -> Vec<OwnerFixture> {
    vec![
        owner(OWNER_A, &[SHARED_TEMPLATE_ID], Some(owner_a_input_schema())),
        owner(OWNER_B, &[SHARED_TEMPLATE_ID], Some(owner_b_input_schema())),
    ]
}

/// The trusted set is a process-global env var, so these tests are mutually
/// exclusive within a process.
///
/// 第一轮评审 MINOR-4 — this lock used to be module-private, which made the
/// comment above false under `cargo test`: `operation::child_track_adapter`'s
/// own trust guard mutates the same variable in the same lib-test process and
/// could not see a lock that lived here. Both writers now take the crate-wide
/// [`crate::forge_trust::trusted_forge_plugins_env_lock`]. (Modules that only
/// *read* the ambient value still rely on nextest's process isolation; the
/// lock does not claim otherwise.)
struct TrustGuard {
    previous: Option<String>,
    expected: Vec<String>,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

impl TrustGuard {
    async fn trust(ids: &str) -> Self {
        let lock = crate::forge_trust::trusted_forge_plugins_env_lock()
            .lock()
            .await;
        let previous = std::env::var("NEIGE_TRUSTED_FORGE_PLUGINS").ok();
        // SAFETY: the lock above serializes every writer in the lib-test
        // binary, and nextest additionally gives every test its own process.
        unsafe { std::env::set_var("NEIGE_TRUSTED_FORGE_PLUGINS", ids) };
        Self {
            previous,
            expected: ids.split(',').map(str::to_string).collect(),
            _lock: lock,
        }
    }

    /// Revoke trust for everything, without releasing the lock or losing the
    /// restore-on-drop. Models "the operator dropped this plugin out of
    /// `NEIGE_TRUSTED_FORGE_PLUGINS`" — running, registered, still the
    /// recorded owner, no longer trusted.
    fn revoke_all(&mut self) {
        // SAFETY: same lock, same process discipline as `trust`.
        unsafe { std::env::set_var("NEIGE_TRUSTED_FORGE_PLUGINS", "dev.nobody.at.all") };
        self.expected.clear();
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

fn manifest_json_for(entry: &OwnerFixture, version: &str) -> Value {
    let mut manifest_json = json!({
        "manifest_version": 2,
        "id": entry.id,
        "version": version,
        "min_kernel_version": "0.0.1",
        "display_name": "Owner binding stub",
        "entrypoint": { "command": "bin/stub" },
        "templates": entry
            .templates
            .iter()
            .map(|id| json!({ "id": id }))
            .collect::<Vec<_>>(),
        "permissions": {}
    });
    if let Some(schema) = entry.input_schema.as_ref() {
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
    owners: &[OwnerFixture],
    version: &str,
    caches: (CardRoleCache, TrackAreaCache),
) -> Arc<PluginHost> {
    let mut builder = PluginRegistry::builder();
    for entry in owners {
        let install_dir = plugins_dir.join(entry.id);
        let manifest = Manifest::parse(&manifest_json_for(entry, version).to_string())
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

async fn boot(owners: &[OwnerFixture]) -> Boot {
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
    for entry in owners {
        let bin_dir = plugins_dir.join(entry.id).join("bin");
        std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
        std::os::unix::fs::symlink(stub_echo_bin(), bin_dir.join("stub"))
            .expect("symlink echo stub");
        repo_dyn
            .plugin_install(NewPlugin {
                id: entry.id.to_string(),
                version: "0.1.0".into(),
                install_path: plugins_dir.join(entry.id).display().to_string(),
                manifest: manifest_json_for(entry, "0.1.0"),
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
    fn upgraded_host(&self, owners: &[OwnerFixture]) -> Arc<PluginHost> {
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

/// What the two readers must say, stated separately per reader.
///
/// 第一轮评审 MAJOR-2 — this used to be `Option<&str>`, i.e. one verdict for
/// both readers, which is what forced a broken *template contract* to also
/// withdraw the *tool scope*. Owner identity and contract validity are two
/// facts; acceptance ② is about the first one only, so the expectation type
/// has to be able to say "owner agreed on, contract not honored".
#[derive(Debug)]
enum Expected<'a> {
    /// Owner known and the contract holds: the planner binds *this* descriptor
    /// with *this* input, and the tool scope is `Only(owner)`.
    OwnerAndContract {
        owner: &'a str,
        template_id: &'a str,
        input: Option<Value>,
    },
    /// Owner known, contract unusable: `Only(owner)` tools, vanilla prompt.
    OwnerWithoutContract { owner: &'a str },
    /// No usable owner at all: zero plugin tools, vanilla prompt.
    NoUsableOwner,
}

/// The joint assertion, over one host.
///
/// 第一轮评审 MINOR-2 — the positive branch used to assert only
/// `bound.is_some()`, so an `Owned` that carried the *wrong* descriptor or the
/// wrong input was invisible to every test in this file. It now pins both.
async fn assert_owner_agreement(
    boot: &Boot,
    host: &Arc<PluginHost>,
    track_id: &str,
    expected: Expected<'_>,
    trust: &TrustGuard,
) {
    trust.check();
    let bound = boot
        .adapter_on(host)
        .bound_template(track_id)
        .await
        .expect("bound_template must not error");
    let scope = plugin_scope_for_track(&boot.mcp_ctx_on(host), Some(track_id)).await;
    let vanilla =
        |bound: &Option<crate::operation::planner_harness_start_adapter::BoundTemplate>,
         why: &str| {
            assert!(
                bound.is_none(),
                "{why}: expected the vanilla planner prompt, got descriptor={:?} input={:?}",
                bound.as_ref().map(|b| b.descriptor.id.clone()),
                bound.as_ref().and_then(|b| b.input.clone()),
            );
        };
    match expected {
        Expected::OwnerAndContract {
            owner,
            template_id,
            input,
        } => {
            let bound = bound.unwrap_or_else(|| {
                panic!("expected the planner to bind {owner}, got a vanilla prompt")
            });
            assert_eq!(
                bound.descriptor.id, template_id,
                "the planner must bind the track's own template, not merely *a* template"
            );
            assert_eq!(
                bound.input, input,
                "the planner must carry the track's persisted template_input verbatim"
            );
            assert_eq!(
                scope,
                TrackPluginScope::Only(owner.to_string()),
                "expected the tool scope to be locked to {owner}"
            );
        }
        Expected::OwnerWithoutContract { owner } => {
            vanilla(&bound, "the template contract is broken");
            assert_eq!(
                scope,
                TrackPluginScope::Only(owner.to_string()),
                "a broken template contract must NOT un-own the track: the owner is \
                 still running ∧ trusted, and `TrackPatch` cannot rewrite \
                 plugin_scope/template_id/template_input, so withdrawing the tools \
                 here would be an unrepairable downgrade"
            );
        }
        Expected::NoUsableOwner => {
            vanilla(&bound, "there is no usable owner");
            assert_eq!(
                scope,
                TrackPluginScope::None,
                "expected the tool scope to fail closed"
            );
        }
    }
}

/// `Expected::OwnerAndContract` for the shared fixture's happy path.
fn owned_by(owner: &str) -> Expected<'_> {
    Expected::OwnerAndContract {
        owner,
        template_id: SHARED_TEMPLATE_ID,
        input: Some(owner_a_input()),
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
        rejected.contains("plan_url") || rejected.contains("issue_url"),
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

/// Acceptance ① with the schema check taken **out of play**: A and B declare
/// the *same* `input_schema`, so the persisted input is perfectly valid under
/// the successor. Nothing but "the owner column is the owner" can reject B
/// here — which is exactly why this test exists next to the differing-schema
/// one. A mutation that reverts the owner lookup to a `template_id` scan
/// leaves the differing-schema repro green (the re-validation catches it) and
/// turns this one red.
#[tokio::test]
async fn planner_must_not_adopt_a_successor_owner_that_shares_the_original_schema() {
    let _trust = TrustGuard::trust(&format!("{OWNER_A},{OWNER_B}")).await;
    let boot = boot(&[
        owner(OWNER_A, &[SHARED_TEMPLATE_ID], Some(owner_a_input_schema())),
        // Same contract as A — the input stays valid across the takeover.
        owner(OWNER_B, &[SHARED_TEMPLATE_ID], Some(owner_a_input_schema())),
    ])
    .await;
    spawn_on(&boot.host, OWNER_A).await;
    let track_id = boot.create_bound_track(Some(owner_a_input())).await;
    assert_eq!(
        boot.stored_plugin_scope(&track_id).await.as_deref(),
        Some(OWNER_A)
    );

    boot.host.stop(OWNER_A).await.expect("stop A");
    spawn_on(&boot.host, OWNER_B).await;
    _trust.check();

    // The input would sail through B's schema — the *only* reason to refuse
    // is that B does not own this track.
    assert!(
        crate::plugin_host::template_input::validate_template_input(
            &owner_a_input_schema(),
            &owner_a_input()
        )
        .is_ok(),
        "this test is only meaningful while the successor accepts the input"
    );
    assert_owner_agreement(
        &boot,
        &boot.host,
        &track_id,
        Expected::NoUsableOwner,
        &_trust,
    )
    .await;
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

    // ① A running: both readers say "owned by A", and the planner binds A's
    //    own template with the row's own input.
    assert_owner_agreement(&boot, &boot.host, &track_id, owned_by(OWNER_A), &_trust).await;

    // ② A stopped, nobody else running: both say "no usable owner".
    boot.host.stop(OWNER_A).await.expect("stop A");
    assert_owner_agreement(
        &boot,
        &boot.host,
        &track_id,
        Expected::NoUsableOwner,
        &_trust,
    )
    .await;

    // ③ B took the id over: still "no usable owner" — B is not this track's
    //    owner, and neither reader may promote it.
    spawn_on(&boot.host, OWNER_B).await;
    assert_owner_agreement(
        &boot,
        &boot.host,
        &track_id,
        Expected::NoUsableOwner,
        &_trust,
    )
    .await;

    // ④ A back: both say A again.
    boot.host.stop(OWNER_B).await.expect("stop B");
    spawn_on(&boot.host, OWNER_A).await;
    assert_owner_agreement(&boot, &boot.host, &track_id, owned_by(OWNER_A), &_trust).await;
    boot.host.stop(OWNER_A).await.expect("stop A");
}

/// 第一轮评审 MINOR-1 — the `trusted` half of `plugin_is_eligible_owner` had
/// no carrier in this file: deleting `&& trusted_forge_plugin(plugin_id)` left
/// all 8 tests here green. Every other test reaches "not an eligible owner" by
/// *stopping* the plugin, which the `running` half alone already rejects.
///
/// Here the owner keeps running and keeps its registry entry; only the
/// operator's `NEIGE_TRUSTED_FORGE_PLUGINS` changes. Both readers must treat
/// it as no owner at all — this is the fail-closed row that survives the
/// MAJOR-2 split, and the one case where the tool scope really does go to zero.
#[tokio::test]
async fn a_running_owner_whose_trust_is_revoked_is_not_an_owner() {
    let mut trust = TrustGuard::trust(OWNER_A).await;
    let boot = boot(&[owner(
        OWNER_A,
        &[SHARED_TEMPLATE_ID],
        Some(owner_a_input_schema()),
    )])
    .await;
    spawn_on(&boot.host, OWNER_A).await;
    let track_id = boot.create_bound_track(Some(owner_a_input())).await;

    // Control: while A is trusted, both readers bind it.
    assert_owner_agreement(&boot, &boot.host, &track_id, owned_by(OWNER_A), &trust).await;

    trust.revoke_all();
    assert!(
        !trusted_forge_plugin(OWNER_A),
        "the operator's revocation must have taken effect"
    );
    assert!(
        boot.host
            .status(OWNER_A)
            .await
            .is_some_and(|status| matches!(status.status, PluginRuntimeStatus::Running)),
        "A must still be RUNNING — otherwise this test is about `running`, not \
         about `trusted`"
    );
    assert_owner_agreement(
        &boot,
        &boot.host,
        &track_id,
        Expected::NoUsableOwner,
        &trust,
    )
    .await;
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
/// that dropped the template).
///
/// 第一轮评审 MAJOR-2 — this used to assert `TrackPluginScope::None` too, i.e.
/// the template contract decided the tool scope. It must not: A is
/// demonstrably still the owner, and the three columns that would have to
/// change for that to stop being true are not writable by any API.
#[tokio::test]
async fn owner_that_stopped_declaring_the_template_id_keeps_its_tools_but_loses_the_prompt() {
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

    // acceptance ④: no descriptor reaches the prompt; the owner keeps its
    // tools because it is still the owner.
    assert_owner_agreement(
        &boot,
        &upgraded,
        &track_id,
        Expected::OwnerWithoutContract { owner: OWNER_A },
        &_trust,
    )
    .await;
    upgraded.stop(OWNER_A).await.expect("stop upgraded A");
}

/// 第一轮评审 MAJOR-1 — the run-time contract check used to be
/// `if let Some(input) = track.template_input { validate_template_input(..) }`,
/// which is one corner of the create-time matrix. This drives the corner it
/// could not see: **absent** input meeting a schema that has since grown a
/// `required` list.
///
/// The row is minted by the real create route while A declares *no*
/// `input_schema` at all — a legal create that stores `template_input = NULL`.
/// A then upgrades to A's usual schema (`required: ["issue_url"]`). The same
/// (plugin, template, input) triple would now be a 400 at create
/// (`validate_template_input_binding`'s `(Some(schema), None)` arm), so
/// run-time must not call the contract honored — a vanilla prompt is the
/// point, not a `template_input` that no schema ever accepted.
#[tokio::test]
async fn absent_input_under_a_newly_required_schema_breaks_the_contract() {
    let _trust = TrustGuard::trust(OWNER_A).await;
    // A declares no input_schema yet — so `template_input` is refused at
    // create and the row stores NULL.
    let boot = boot(&[owner(OWNER_A, &[SHARED_TEMPLATE_ID], None)]).await;
    spawn_on(&boot.host, OWNER_A).await;
    let track_id = boot.create_bound_track(None).await;
    assert_eq!(
        boot.stored_plugin_scope(&track_id).await.as_deref(),
        Some(OWNER_A),
        "create must have recorded A as the owner"
    );
    let stored_input: Option<Value> =
        sqlx::query_scalar::<_, Option<String>>("SELECT template_input FROM tracks WHERE id = ?1")
            .bind(&track_id)
            .fetch_one(boot.repo.pool())
            .await
            .expect("select template_input")
            .map(|raw| serde_json::from_str(&raw).expect("stored template_input is JSON"));
    assert_eq!(
        stored_input, None,
        "the schema-less create must have stored NULL template_input — that is \
         the branch the old run-time check skipped"
    );

    // Control: while A still declares no schema, the contract holds and the
    // planner binds the descriptor with no input.
    assert_owner_agreement(
        &boot,
        &boot.host,
        &track_id,
        Expected::OwnerAndContract {
            owner: OWNER_A,
            template_id: SHARED_TEMPLATE_ID,
            input: None,
        },
        &_trust,
    )
    .await;
    boot.host.stop(OWNER_A).await.expect("stop A");

    // A upgrades: same id, same template, but now `issue_url` is required.
    let upgraded = boot.upgraded_host(&[owner(
        OWNER_A,
        &[SHARED_TEMPLATE_ID],
        Some(owner_a_input_schema()),
    )]);
    spawn_on(&upgraded, OWNER_A).await;

    // The create route's verdict on this exact triple, read from the create
    // route's own function rather than restated here.
    let upgraded_manifest = upgraded
        .registry()
        .get(OWNER_A)
        .expect("upgraded A is registered");
    assert!(
        crate::plugin_host::template_input::validate_template_input_binding(
            Some(&upgraded_manifest),
            None,
        )
        .is_err(),
        "the fixture is only meaningful while create would refuse this triple"
    );

    assert_owner_agreement(
        &boot,
        &upgraded,
        &track_id,
        Expected::OwnerWithoutContract { owner: OWNER_A },
        &_trust,
    )
    .await;
    upgraded.stop(OWNER_A).await.expect("stop upgraded A");
}

/// Acceptance ⑤ — same owner, same template id, but the owner's
/// `input_schema` changed and the persisted `template_input` no longer
/// satisfies it. The stale blob must not reach the planner prompt.
///
/// 第一轮评审 MINOR-3 — the MCP projection of this state had no carrier: the
/// test asserted only that the planner got `None`, so mutating the stale-input
/// return point to `Unbound` left it green while the tool scope silently
/// widened to `All`. Both readers are pinned now, and (MAJOR-2) the tool scope
/// stays `Only(A)` rather than going to zero.
#[tokio::test]
async fn stale_template_input_is_rechecked_against_the_current_owner_schema() {
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
    assert_owner_agreement(&boot, &boot.host, &track_id, owned_by(OWNER_A), &_trust).await;
    boot.host.stop(OWNER_A).await.expect("stop A");

    // Owner upgrade: same plugin, same template id, incompatible schema.
    let upgraded = boot.upgraded_host(&[owner(
        OWNER_A,
        &[SHARED_TEMPLATE_ID],
        Some(owner_b_input_schema()),
    )]);
    spawn_on(&upgraded, OWNER_A).await;

    // acceptance ⑤: the blob validated under the old schema must not be
    // injected under the new one; A keeps its tools.
    assert_owner_agreement(
        &boot,
        &upgraded,
        &track_id,
        Expected::OwnerWithoutContract { owner: OWNER_A },
        &_trust,
    )
    .await;
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
    assert_owner_agreement(
        &boot,
        &boot.host,
        &bound_track,
        Expected::NoUsableOwner,
        &_trust,
    )
    .await;
}
