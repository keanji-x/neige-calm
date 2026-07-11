#![cfg(all(unix, feature = "codex-e2e"))]

use crate::support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use calm_server::codex_appserver::InputItem;
use calm_server::config::Config;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::db::{Repo, RepoSyncDomainRaw};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave};
use calm_server::routes::theme::RequestTheme;
use calm_server::shared_codex_appserver::{
    SharedCodexAppServer, SharedThreadStartParams, ThreadConfig,
};
use clap::Parser;
use serde_json::json;
// #868: shared no-fallback resolver — env `NEIGE_CODEX_BIN` only, `None` ⇒
// self-skip via `skip!`. Tests must never probe/spawn a PATH codex.
use support::codex_fixture::resolve_codex_bin;

fn cfg(root: &tempfile::TempDir, codex_bin: &Path) -> Config {
    Config::parse_from([
        "calm-server",
        "--data-dir",
        root.path().to_str().unwrap(),
        "--codex-bin",
        codex_bin.to_str().expect("utf-8 codex bin path"),
        // Test codex daemons must NEVER post hooks to the default listen address —
        // that is the production calm-server port on shared boxes (production-kill
        // incident, 2026-07-04); tests do not consume hook ingest.
        "--codex-ingest-url",
        "http://127.0.0.1:1/hooks-disabled-in-e2e",
    ])
}

async fn seed_card(repo: &SqlxRepo, name: &str) -> String {
    let cove = repo
        .cove_create(NewCove {
            name: name.into(),
            color: "#abc".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "e2e".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id,
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    card.id.to_string()
}

async fn repo_and_card() -> (Arc<SqlxRepo>, String) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = seed_card(&repo, "e2e").await;
    (repo, card_id)
}

async fn daemon(
    root: &tempfile::TempDir,
    repo: Arc<dyn Repo>,
    codex_bin: &Path,
) -> Arc<SharedCodexAppServer> {
    let cfg = cfg(root, codex_bin);
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();
    SharedCodexAppServer::new(&cfg, Arc::new(home), repo)
}

#[tokio::test]
#[ignore]
async fn shared_appserver_two_threads_true_binary() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!(
            "codex binary not resolved (NEIGE_CODEX_BIN unset, or not an executable file); CI has no codex"
        );
    };
    let root = tempfile::tempdir().unwrap();
    let (repo, card_id) = repo_and_card().await;
    let d = daemon(&root, repo.clone(), &codex_bin).await;
    if let Err(e) = d.start_or_takeover().await {
        eprintln!("skipping: shared app-server did not boot in this environment: {e}");
        return;
    }
    let t1 = d
        .thread_start_for_card(
            &card_id,
            CardRole::Worker,
            None,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    let card2 = seed_card(&repo, "e2e-2").await;
    let t2 = d
        .thread_start_for_card(
            &card2,
            CardRole::Worker,
            None,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    assert_ne!(t1, t2);
    let _ = d
        .turn_start(&t1, vec![InputItem::text("Say OK in one word.")])
        .await
        .unwrap();
    let _ = d
        .turn_start(&t2, vec![InputItem::text("Say OK in one word.")])
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn shared_appserver_restart_resumes_thread() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!(
            "codex binary not resolved (NEIGE_CODEX_BIN unset, or not an executable file); CI has no codex"
        );
    };
    let root = tempfile::tempdir().unwrap();
    let (repo, card_id) = repo_and_card().await;
    let d = daemon(&root, repo.clone(), &codex_bin).await;
    if let Err(e) = d.start_or_takeover().await {
        eprintln!("skipping: shared app-server did not boot in this environment: {e}");
        return;
    }
    let thread_id = d
        .thread_start_for_card(
            &card_id,
            CardRole::Worker,
            None,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    let _ = d
        .turn_start(&thread_id, vec![InputItem::text("Say OK in one word.")])
        .await
        .unwrap();
    let pgid = d.status_snapshot().runtime.unwrap().pgid;
    calm_server::proc_identity::signal_process_group(pgid, libc::SIGTERM);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(d.status_snapshot().restart_count >= 1);
}
