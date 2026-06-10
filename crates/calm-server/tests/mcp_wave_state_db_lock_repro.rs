//! 回归测试：保证 audited writes 用 BEGIN IMMEDIATE，并发 wave update 不再 SQLITE_BUSY_SNAPSHOT.
//! Stress mix: 8 update tasks x 8 event tasks x 100 iterations.
//!
//! Run locally with:
//!   cargo test -p calm-server --test mcp_wave_state_db_lock_repro -- --nocapture

use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::wave_state::TOOL_UPDATE_WAVE_STATE;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::wave_cove_cache::WaveCoveCache;
use serde_json::{Value, json};
use tokio::sync::{Barrier, Mutex};

const UPDATE_TASKS: usize = 8;
const EVENT_TASKS: usize = 8;
const ITERS: usize = 100;

struct Boot {
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    repo: Arc<dyn Repo>,
    cove_id: CoveId,
    wave_id: WaveId,
    spec_card_id: CardId,
    worker_card_id: CardId,
    _dir: tempfile::TempDir,
}

async fn boot() -> Boot {
    let dir = tempfile::tempdir().expect("temp dir");
    let db_path = dir.path().join("repro.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let sqlx_repo = Arc::new(SqlxRepo::open(&url).await.expect("open sqlite file"));
    let repo: Arc<dyn Repo> = sqlx_repo;

    let cove = repo
        .cove_create(NewCove {
            name: "mcp-wave-lock-repro".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "initial".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let spec = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "spec".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    let worker = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();

    let roles = CardRoleCache::new();
    roles.insert(spec.id.clone(), CardRole::Spec, wave.id.clone());
    roles.insert(worker.id.clone(), CardRole::Worker, wave.id.clone());
    let coves = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&coves).await.unwrap();

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        events: EventBus::new(),
        write: calm_server::state::WriteContext::new(roles, coves),
        daemon_token_hash: None,
    });
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);

    Boot {
        ctx,
        registry: Arc::new(registry),
        repo,
        cove_id: cove.id,
        wave_id: wave.id,
        spec_card_id: spec.id,
        worker_card_id: worker.id,
        _dir: dir,
    }
}

async fn call_update(boot: &Boot, task: usize, iter: usize) -> Result<Value, RpcError> {
    let identity = ToolCallIdentity {
        card_id: boot.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        wave_id: Some(boot.wave_id.as_str().to_string()),
        thread_id: format!("spec-{task}"),
    };
    let handler = boot.registry.lookup(TOOL_UPDATE_WAVE_STATE).unwrap();
    handler(
        boot.ctx.clone(),
        identity,
        json!({ "title": format!("wave-{task}-{iter}") }),
    )
    .await
}

async fn append_worker_event(boot: &Boot, task: usize, iter: usize) -> anyhow::Result<()> {
    let scope = EventScope::Card {
        card: boot.worker_card_id.clone(),
        wave: boot.wave_id.clone(),
        cove: boot.cove_id.clone(),
    };
    #[allow(deprecated)]
    boot.repo
        .log_pure_event(
            ActorId::AiCodex(boot.worker_card_id.clone()),
            scope,
            None,
            &boot.ctx.events,
            boot.ctx.write.role_cache(),
            boot.ctx.write.cove_cache(),
            Event::TaskFailed {
                idempotency_key: format!("worker-{task}-{iter}"),
                reason: "db-lock-repro".into(),
                agent_message: None,
            },
        )
        .await?;
    Ok(())
}

fn is_busy_error(s: &str) -> bool {
    s.contains("database is locked") || s.contains("BUSY") || s.contains("SQLITE_BUSY_SNAPSHOT")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_update_wave_state_no_database_locked() {
    let boot = Arc::new(boot().await);
    let barrier = Arc::new(Barrier::new(UPDATE_TASKS + EVENT_TASKS));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut joins = Vec::new();

    for task in 0..UPDATE_TASKS {
        let boot = Arc::clone(&boot);
        let barrier = Arc::clone(&barrier);
        let errors = Arc::clone(&errors);
        joins.push(tokio::spawn(async move {
            barrier.wait().await;
            for iter in 0..ITERS {
                if let Err(e) = call_update(&boot, task, iter).await {
                    errors.lock().await.push(e.to_string());
                }
                tokio::task::yield_now().await;
            }
        }));
    }

    for task in 0..EVENT_TASKS {
        let boot = Arc::clone(&boot);
        let barrier = Arc::clone(&barrier);
        let errors = Arc::clone(&errors);
        joins.push(tokio::spawn(async move {
            barrier.wait().await;
            for iter in 0..ITERS {
                if let Err(e) = append_worker_event(&boot, task, iter).await {
                    errors.lock().await.push(e.to_string());
                }
                tokio::task::yield_now().await;
            }
        }));
    }

    for join in joins {
        join.await.expect("task panicked");
    }

    let errors = errors.lock().await;
    let busy = errors.iter().filter(|e| is_busy_error(e)).count();
    println!(
        "busy/snapshot errors: {busy}; total errors: {}",
        errors.len()
    );
    assert_eq!(
        busy,
        0,
        "expected no SQLite busy/snapshot errors; got {busy} busy errors out of {} total errors: {errors:#?}",
        errors.len()
    );
    assert!(errors.is_empty(), "unexpected non-busy errors: {errors:#?}");
}
