//! #891 slice ④ — registration-time workflow-id uniqueness at
//! `PluginHost::spawn`.
//!
//! Two trusted plugins declaring the same workflow id must not run at the
//! same time: the second spawn is refused with `HostError::WorkflowConflict`
//! (per-plugin failure — the autospawn loop logs and continues). The
//! uniqueness set is "running ∧ trusted": an untrusted duplicate is not
//! blocked (it never enters workflow resolution), and a STOPPED trusted
//! holder does not squat on the id.
//!
//! The trusted set is env-configured (`NEIGE_TRUSTED_FORGE_PLUGINS`), so this
//! lives in its own integration-test binary (own process) and every test
//! takes the shared `FORGE_ENV_LOCK` before mutating env — no cross-test
//! env races.

#![cfg(unix)]

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::model::NewPlugin;
use calm_server::plugin_host::{
    HostError, Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus,
};
use calm_server::wave_cove_cache::WaveCoveCache;
use serde_json::json;
use support::forge_env::{EnvGuard, FORGE_ENV_LOCK};
use tokio::sync::Barrier;
use tokio::time::{Instant, sleep, timeout};

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-echo");
const TRUSTED_A: &str = "dev.trusted-a";
const TRUSTED_B: &str = "dev.trusted-b";
const UNTRUSTED_C: &str = "dev.untrusted-c";
const TRUSTED_D: &str = "dev.trusted-d";
const SHARED_WORKFLOW_ID: &str = "shared-workflow";

#[tokio::test]
async fn duplicate_workflow_id_is_rejected_at_spawn_for_trusted_plugins_only() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let _trusted = EnvGuard::set(
        "NEIGE_TRUSTED_FORGE_PLUGINS",
        format!("{TRUSTED_A},{TRUSTED_B}"),
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let events = EventBus::new();
    let host = boot_host(&repo, tmp.path(), events.clone()).await;

    host.spawn(TRUSTED_A).await.expect("spawn first trusted");
    wait_for_running(&host, TRUSTED_A).await;

    // Second trusted plugin, same workflow id → refused before any spawn.
    // Subscribe first: the refusal must surface a failed `PluginState`
    // event (#891 review fix, design §4.4 "该插件进 Failed") so the plugin
    // doesn't silently look stopped.
    let mut state_events = events.subscribe();
    let err = host
        .spawn(TRUSTED_B)
        .await
        .expect_err("duplicate trusted workflow id must be refused");
    match &err {
        HostError::WorkflowConflict {
            plugin_id,
            workflow_id,
            held_by,
        } => {
            assert_eq!(plugin_id, TRUSTED_B);
            assert_eq!(workflow_id, SHARED_WORKFLOW_ID);
            assert_eq!(held_by, TRUSTED_A);
        }
        other => panic!("expected WorkflowConflict, got {other:?}"),
    }
    assert!(
        host.status(TRUSTED_B).await.is_none(),
        "refused spawn must leave no runtime entry behind"
    );
    let crashed = timeout(Duration::from_secs(5), async {
        loop {
            let env = state_events.recv().await.expect("event bus open");
            if let Event::PluginState {
                id,
                state,
                last_error,
            } = env.event
                && id == TRUSTED_B
            {
                break (state, last_error);
            }
        }
    })
    .await
    .expect("workflow-conflict refusal must emit a PluginState event");
    assert_eq!(
        crashed.0, "crashed",
        "refusal must surface as a failed state"
    );
    assert!(
        crashed.1.unwrap_or_default().contains(SHARED_WORKFLOW_ID),
        "failed-state event should carry the conflicting workflow id"
    );

    // Untrusted duplicate is NOT blocked: it never enters the workflow
    // resolution set, so its duplicate id is unreachable anyway.
    host.spawn(UNTRUSTED_C)
        .await
        .expect("untrusted duplicate spawns");
    wait_for_running(&host, UNTRUSTED_C).await;

    // A stopped trusted holder does not squat on the workflow id.
    host.stop(TRUSTED_A).await.expect("stop first trusted");
    host.spawn(TRUSTED_B)
        .await
        .expect("workflow id is free once the holder stopped");
    wait_for_running(&host, TRUSTED_B).await;

    host.stop(TRUSTED_B).await.expect("stop second trusted");
    host.stop(UNTRUSTED_C).await.expect("stop untrusted");
}

/// #891 review fix (spawn TOCTOU) — two barrier-synchronized concurrent
/// spawns of trusted plugins declaring the same workflow id must admit
/// exactly one. Pre-fix, both passed the (unlocked, Running-only) conflict
/// check before either inserted its processes-map entry, yielding duplicate
/// running owners and a nondeterministic `plugin_scope_for_wave` winner.
/// Also proves the loser's admission reservation is released: a third
/// same-workflow spawn conflicts against the REAL winner (a leaked
/// reservation would name the loser), and once the winner stops, the loser
/// spawns cleanly.
#[tokio::test]
async fn concurrent_duplicate_workflow_spawns_admit_exactly_one() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let _trusted = EnvGuard::set(
        "NEIGE_TRUSTED_FORGE_PLUGINS",
        format!("{TRUSTED_A},{TRUSTED_B},{TRUSTED_D}"),
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let host = boot_host(&repo, tmp.path(), EventBus::new()).await;

    let barrier = Arc::new(Barrier::new(2));
    let racing_spawn = |plugin_id: &'static str| {
        let host = Arc::clone(&host);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            host.spawn(plugin_id).await
        })
    };
    let spawn_a = racing_spawn(TRUSTED_A);
    let spawn_b = racing_spawn(TRUSTED_B);
    let results = [
        (TRUSTED_A, spawn_a.await.expect("join spawn A")),
        (TRUSTED_B, spawn_b.await.expect("join spawn B")),
    ];

    let mut winner: Option<&str> = None;
    let mut loser: Option<(&str, HostError)> = None;
    for (id, result) in results {
        match result {
            Ok(()) => {
                assert!(winner.is_none(), "both concurrent spawns won: TOCTOU");
                winner = Some(id);
            }
            Err(err) => {
                assert!(loser.is_none(), "both concurrent spawns lost");
                loser = Some((id, err));
            }
        }
    }
    let winner = winner.expect("exactly one concurrent spawn must win");
    let (loser, loser_err) = loser.expect("exactly one concurrent spawn must lose");
    match &loser_err {
        HostError::WorkflowConflict {
            plugin_id,
            workflow_id,
            held_by,
        } => {
            assert_eq!(plugin_id, loser);
            assert_eq!(workflow_id, SHARED_WORKFLOW_ID);
            assert_eq!(
                held_by, winner,
                "the conflict must be held by the actual winner"
            );
        }
        other => panic!("expected WorkflowConflict for the loser, got {other:?}"),
    }
    wait_for_running(&host, winner).await;
    assert!(
        host.status(loser).await.is_none(),
        "loser must leave neither a runtime entry nor a leaked reservation"
    );

    // Third trusted plugin, same workflow id → still refused, and the holder
    // must be the real winner. A leaked loser reservation would surface as
    // `held_by == loser` here.
    let err = host
        .spawn(TRUSTED_D)
        .await
        .expect_err("third same-workflow spawn must conflict against the winner");
    match &err {
        HostError::WorkflowConflict { held_by, .. } => {
            assert_eq!(
                held_by, winner,
                "third spawn must conflict with the real winner, not a leaked reservation"
            );
        }
        other => panic!("expected WorkflowConflict for the third spawn, got {other:?}"),
    }

    // Once the winner stops, the workflow id is free: the loser now spawns —
    // proving its failed admission left no residue.
    host.stop(winner).await.expect("stop winner");
    host.spawn(loser)
        .await
        .expect("loser spawns once the workflow id is free");
    wait_for_running(&host, loser).await;
    host.stop(loser).await.expect("stop loser");
}

async fn boot_host(repo: &Arc<SqlxRepo>, root: &Path, events: EventBus) -> Arc<PluginHost> {
    let plugins_dir = root.join("plugins");
    let plugins_data_dir = root.join("plugins-data");
    std::fs::create_dir_all(&plugins_data_dir).expect("create plugins data dir");

    let registry = PluginRegistry::empty();
    for plugin_id in [TRUSTED_A, TRUSTED_B, UNTRUSTED_C, TRUSTED_D] {
        let install_dir = plugins_dir.join(plugin_id);
        let bin_dir = install_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
        std::os::unix::fs::symlink(Path::new(ECHO_BIN), bin_dir.join("stub"))
            .expect("symlink echo stub");

        let manifest_json = json!({
            "manifest_version": 1,
            "id": plugin_id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Workflow Uniqueness Stub",
            "entrypoint": { "command": "bin/stub" },
            "workflows": [
                {
                    "id": SHARED_WORKFLOW_ID,
                    "plan_template": [
                        {
                            "key": "inspect",
                            "kind": "codex",
                            "goal": "Inspect the issue.",
                            "depends_on": []
                        }
                    ],
                    "gates": [],
                    "spec_instructions": "Use workflow {wave_id}.",
                    "card_kinds": []
                }
            ],
            "permissions": {}
        });
        let manifest: Manifest =
            Manifest::parse(&manifest_json.to_string()).expect("manifest parses");
        registry.insert(manifest, Some(install_dir.clone()));

        repo.plugin_install(NewPlugin {
            id: plugin_id.into(),
            version: "0.1.0".into(),
            install_path: install_dir.display().to_string(),
            manifest: manifest_json,
            enabled: true,
            user_config: json!({}),
        })
        .await
        .expect("seed plugin row");
    }

    let repo_dyn: Arc<dyn Repo> = repo.clone();
    Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo_dyn,
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events,
        calm_server::state::WriteContext::new(CardRoleCache::new(), WaveCoveCache::new()),
    ))
}

async fn wait_for_running(host: &Arc<PluginHost>, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = host.status(id).await
            && matches!(s.status, PluginRuntimeStatus::Running)
        {
            return;
        }
        assert!(
            Instant::now() <= deadline,
            "plugin {id} did not reach Running within 5s"
        );
        sleep(Duration::from_millis(25)).await;
    }
}
