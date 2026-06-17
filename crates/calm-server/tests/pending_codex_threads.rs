use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, session_projection_by_id_tx, session_start_runtime_tx, session_supersede_and_start_tx,
};
use calm_server::event::{Event, EventBus};
use calm_server::model::{NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::operation::codex_adapter::CodexAdapter;
use calm_server::operation::{
    Operation, OperationCompletionBus, Phase, PhaseTag, ProviderAdapter, SpawnCtx,
    SqlxOperationRepo, TxOutput,
};
use calm_server::pending_codex_threads::{
    PendingEntry, PendingThreadStartRegistry, spawn_periodic_expire_task,
};
use calm_server::session_projection_lookup::project_runtime_into_card_payload;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{CodexClient, DaemonClient};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::wave_cove_cache::WaveCoveCache;
use calm_types::worker::WorkerSessionId;
use serde_json::json;
use tokio::sync::Mutex;

async fn runtime_by_id_tx_snapshot(
    repo: &SqlxRepo,
    runtime_id: &str,
) -> Option<calm_server::session_projection_repo::WorkerSessionProjection> {
    let id = runtime_id.to_string();
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_projection_by_id_tx(&mut tx, &id).await.unwrap();
    tx.commit().await.unwrap();
    runtime
}

async fn boot() -> (Arc<SqlxRepo>, EventBus, String) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "pending".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "pending".into(),
            sort: None,
            cwd: "/workspace".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    (repo, EventBus::new(), wave.id.to_string())
}

async fn boot_pending_server() -> (
    Arc<SqlxRepo>,
    Arc<PendingThreadStartRegistry>,
    Arc<SharedCodexAppServer>,
    String,
) {
    let (repo, events, wave_id) = boot().await;
    let registry = Arc::new(PendingThreadStartRegistry::new(repo.clone(), events));
    let server = SharedCodexAppServer::new_stub_with_pending(repo.clone(), Some(registry.clone()));
    (repo, registry, server, wave_id)
}

async fn seed_card(repo: &SqlxRepo, wave_id: &str, terminal_id: &str) -> String {
    seed_card_with_runtime_kind(repo, wave_id, terminal_id, WorkerSessionKind::CodexCard).await
}

async fn insert_terminal(repo: &SqlxRepo, card_id: &str, terminal_id: &str) {
    let theme = calm_server::routes::theme::RequestTheme::default_dark();
    sqlx::query(
        r#"INSERT INTO terminals
               (id, card_id, program, cwd, env, pid, theme_fg, theme_bg, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8)"#,
    )
    .bind(terminal_id)
    .bind(card_id)
    .bind("bash")
    .bind("/workspace")
    .bind("{}")
    .bind(theme.fg_arg())
    .bind(theme.bg_arg())
    .bind(0_i64)
    .execute(repo.pool())
    .await
    .unwrap();
}

async fn seed_card_with_runtime_kind(
    repo: &SqlxRepo,
    wave_id: &str,
    terminal_id: &str,
    runtime_kind: WorkerSessionKind,
) -> String {
    let card = repo
        .card_create(NewCard {
            wave_id: wave_id.into(),
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    insert_terminal(repo, card.id.as_str(), terminal_id).await;

    start_runtime_for_card(repo, card.id.as_str(), terminal_id, runtime_kind).await;
    card.id.to_string()
}

async fn runtime_id_for_card(repo: &SqlxRepo, card_id: &str) -> String {
    repo.session_projection_active_for_card(&card_id.to_string())
        .await
        .unwrap()
        .expect("active runtime")
        .id
}

async fn start_runtime_for_card(
    repo: &SqlxRepo,
    card_id: &str,
    terminal_id: &str,
    runtime_kind: WorkerSessionKind,
) -> String {
    start_runtime_for_card_with_thread(repo, card_id, terminal_id, runtime_kind, None).await
}

async fn start_runtime_for_card_with_thread(
    repo: &SqlxRepo,
    card_id: &str,
    terminal_id: &str,
    runtime_kind: WorkerSessionKind,
    thread_id: Option<&str>,
) -> String {
    let runtime_id = new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card_id.to_string(),
            kind: runtime_kind,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::TurnPending,
            terminal_run_id: Some(terminal_id.to_string()),
            thread_id: thread_id.map(str::to_owned),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    runtime_id
}

async fn supersede_runtime_for_card(
    repo: &SqlxRepo,
    old_runtime_id: &str,
    card_id: &str,
    terminal_id: &str,
    runtime_kind: WorkerSessionKind,
) -> String {
    let runtime_id = new_id();
    let old_runtime_id = old_runtime_id.to_string();
    let mut tx = repo.pool().begin().await.unwrap();
    session_supersede_and_start_tx(
        &mut tx,
        &old_runtime_id,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card_id.to_string(),
            kind: runtime_kind,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::TurnPending,
            terminal_run_id: Some(terminal_id.to_string()),
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    runtime_id
}

async fn projected_card(repo: &SqlxRepo, card_id: &str) -> calm_server::model::Card {
    let mut card = repo.card_get(card_id).await.unwrap().expect("card row");
    project_runtime_into_card_payload(repo, &mut card)
        .await
        .unwrap();
    card
}

fn entry_with_runtime_id(
    card_id: &str,
    wave_id: &str,
    terminal_id: &str,
    runtime_id: &str,
) -> PendingEntry {
    PendingEntry::new(
        card_id.to_string(),
        Some(wave_id.to_string()),
        terminal_id.to_string(),
        runtime_id.to_string(),
    )
}

async fn entry(repo: &SqlxRepo, card_id: &str, wave_id: &str, terminal_id: &str) -> PendingEntry {
    let runtime_id = runtime_id_for_card(repo, card_id).await;
    entry_with_runtime_id(card_id, wave_id, terminal_id, &runtime_id)
}

async fn seed_pending(
    repo: &SqlxRepo,
    registry: &PendingThreadStartRegistry,
    wave_id: &str,
    terminal_id: &str,
) -> String {
    let card_id = seed_card(repo, wave_id, terminal_id).await;
    registry
        .register(entry(repo, &card_id, wave_id, terminal_id).await)
        .await
        .unwrap();
    card_id
}

fn dummy_codex_operation() -> Operation {
    Operation {
        id: new_id(),
        operation_key: new_id(),
        kind: "codex-create".into(),
        idempotency_key: None,
        payload_hash: "pending-runtime-compensation-test".into(),
        target_type: "runtime".into(),
        target_id: None,
        target: json!({}),
        payload: json!({}),
        tx_output: None,
        phase: Phase::Pending,
        phase_detail: None,
        attempt: 0,
        last_error: None,
        compensation_state: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_artifacts: None,
        parked_at_ms: None,
        parked_deadline_ms: None,
    }
}

#[tokio::test]
async fn register_and_bind_in_arrival_order() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let a = seed_card(&repo, &wave_id, "term-a").await;
    let b = seed_card(&repo, &wave_id, "term-b").await;

    registry
        .register(entry(&repo, &a, &wave_id, "term-a").await)
        .await
        .unwrap();
    registry
        .register(entry(&repo, &b, &wave_id, "term-b").await)
        .await
        .unwrap();

    assert_eq!(
        registry.on_thread_started("T-1").await.unwrap(),
        Some(a.clone())
    );
    assert_eq!(
        registry.on_thread_started("T-2").await.unwrap(),
        Some(b.clone())
    );

    let runtime_a = repo
        .session_projection_active_for_card(&a)
        .await
        .unwrap()
        .expect("runtime a");
    let runtime_b = repo
        .session_projection_active_for_card(&b)
        .await
        .unwrap()
        .expect("runtime b");
    assert_eq!(runtime_a.thread_id.as_deref(), Some("T-1"));
    assert_eq!(runtime_b.thread_id.as_deref(), Some("T-2"));
}

#[tokio::test]
async fn register_is_idempotent_by_card_and_runtime_without_reordering() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let a = seed_card(&repo, &wave_id, "term-a").await;
    let b = seed_card(&repo, &wave_id, "term-b").await;
    let runtime_a = runtime_id_for_card(&repo, &a).await;

    registry
        .register(entry_with_runtime_id(&a, &wave_id, "term-a", &runtime_a))
        .await
        .unwrap();
    registry
        .register(entry(&repo, &b, &wave_id, "term-b").await)
        .await
        .unwrap();
    registry
        .register(entry_with_runtime_id(&a, &wave_id, "term-a", &runtime_a))
        .await
        .unwrap();

    assert_eq!(registry.pending_count().await, 2);
    assert_eq!(
        registry.on_thread_started("T-1").await.unwrap(),
        Some(a.clone()),
        "duplicate registration must not move the first card to the back"
    );
    assert_eq!(
        registry.on_thread_started("T-2").await.unwrap(),
        Some(b.clone())
    );
}

#[tokio::test]
async fn bind_persists_to_runtime_and_projects_payload() {
    let (repo, events, wave_id) = boot().await;
    let mut rx = events.subscribe();
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let card_id = seed_card(&repo, &wave_id, "term-a").await;
    registry
        .register(entry(&repo, &card_id, &wave_id, "term-a").await)
        .await
        .unwrap();

    registry.on_thread_started("T-bind").await.unwrap();

    let card = repo.card_get(&card_id).await.unwrap().expect("card row");
    assert!(card.payload.get("codex_thread_id").is_none());
    assert!(card.payload.get("codex_thread_status").is_none());
    let runtime = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("runtime row");
    assert_eq!(runtime.thread_id.as_deref(), Some("T-bind"));
    assert_eq!(runtime.status, WorkerSessionState::Running);
    let projected = projected_card(&repo, &card_id).await;
    assert_eq!(projected.payload["codex_thread_id"], "T-bind");
    assert_eq!(projected.payload["codex_thread_status"], "started");

    let env = rx.recv().await.unwrap();
    assert!(env.id > 0, "bind event must be persisted for cursor replay");
    match env.event {
        Event::CardUpdated(card) => assert_eq!(card.id.as_str(), card_id),
        other => panic!("expected card.updated, got {other:?}"),
    }
}

#[tokio::test]
async fn bind_entry_clears_terminal_run_id() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let card_id = seed_card_with_runtime_kind(
        &repo,
        &wave_id,
        "term-bind-clear",
        WorkerSessionKind::SharedSpec,
    )
    .await;
    let runtime_id = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("active runtime")
        .id;
    registry
        .register(entry(&repo, &card_id, &wave_id, "term-bind-clear").await)
        .await
        .unwrap();

    registry.on_thread_started("T-bind-clear").await.unwrap();

    let runtime = repo
        .session_projection_by_id(&runtime_id)
        .await
        .unwrap()
        .expect("runtime row");
    assert_eq!(runtime.status, WorkerSessionState::Running);
    assert!(runtime.terminal_run_id.is_none());
    assert_eq!(runtime.thread_id.as_deref(), Some("T-bind-clear"));
}

#[tokio::test]
async fn bind_entry_keeps_terminal_run_id_for_codex_card_kind() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let card_id = seed_card(&repo, &wave_id, "term-codex-card").await;
    let runtime_id = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("active runtime")
        .id;
    registry
        .register(entry(&repo, &card_id, &wave_id, "term-codex-card").await)
        .await
        .unwrap();

    registry.on_thread_started("T-codex-card").await.unwrap();

    let runtime = repo
        .session_projection_by_id(&runtime_id)
        .await
        .unwrap()
        .expect("runtime row");
    assert_eq!(runtime.status, WorkerSessionState::Running);
    assert_eq!(
        runtime.terminal_run_id.as_deref(),
        Some("term-codex-card"),
        "CodexCard runtime must keep terminal_run_id; it is its completion identity"
    );
    assert_eq!(runtime.thread_id.as_deref(), Some("T-codex-card"));
}

#[tokio::test]
async fn on_thread_started_drops_entry_when_registered_runtime_inactive() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let card_id = seed_card_with_runtime_kind(
        &repo,
        &wave_id,
        "term-missing",
        WorkerSessionKind::SharedSpec,
    )
    .await;
    registry
        .register(entry(&repo, &card_id, &wave_id, "term-missing").await)
        .await
        .unwrap();
    repo.session_projection_complete_for_card(&card_id, WorkerSessionState::Failed)
        .await
        .unwrap();

    let bound = registry.on_thread_started("T-repark").await.unwrap();

    assert_eq!(bound, None);
    assert_eq!(registry.pending_count().await, 0);
    assert!(
        repo.session_projection_active_for_card(&card_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn on_thread_started_drops_registered_runtime_even_if_runtime_reappears() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let card_id =
        seed_card_with_runtime_kind(&repo, &wave_id, "term-retry", WorkerSessionKind::SharedSpec)
            .await;
    registry
        .register(entry(&repo, &card_id, &wave_id, "term-retry").await)
        .await
        .unwrap();
    repo.session_projection_complete_for_card(&card_id, WorkerSessionState::Failed)
        .await
        .unwrap();

    let runtime_id =
        start_runtime_for_card(&repo, &card_id, "term-retry", WorkerSessionKind::SharedSpec).await;

    assert_eq!(registry.on_thread_started("T-retry").await.unwrap(), None);
    assert_eq!(registry.pending_count().await, 0);
    let runtime = repo
        .session_projection_by_id(&runtime_id)
        .await
        .unwrap()
        .expect("reappeared runtime row");
    assert_eq!(runtime.thread_id, None);
}

#[tokio::test]
async fn expire_drops_abandoned_entries_past_ttl() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let old = seed_card(&repo, &wave_id, "term-old").await;
    let fresh = seed_card(&repo, &wave_id, "term-fresh").await;

    let mut old_entry = entry(&repo, &old, &wave_id, "term-old").await;
    old_entry.registered_at = Instant::now()
        .checked_sub(Duration::from_secs(30))
        .expect("instant subtraction");
    registry.register(old_entry).await.unwrap();
    registry
        .register(entry(&repo, &fresh, &wave_id, "term-fresh").await)
        .await
        .unwrap();

    assert_eq!(registry.expire(Duration::from_secs(10)).await, 1);
    assert_eq!(registry.pending_count().await, 1);
    let old_card = projected_card(&repo, &old).await;
    assert_eq!(old_card.payload["codex_thread_status"], "failed_to_spawn");
    assert_eq!(
        registry.on_thread_started("T-fresh").await.unwrap(),
        Some(fresh.clone())
    );
}

#[tokio::test]
async fn ttl_expire_projects_failed_status() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let card_id = seed_card(&repo, &wave_id, "term-ttl").await;
    let mut old_entry = entry(&repo, &card_id, &wave_id, "term-ttl").await;
    old_entry.registered_at = Instant::now()
        .checked_sub(Duration::from_secs(30))
        .expect("instant subtraction");
    registry.register(old_entry).await.unwrap();

    assert_eq!(registry.expire(Duration::from_secs(10)).await, 1);

    let card = projected_card(&repo, &card_id).await;
    assert_eq!(card.payload["codex_thread_status"], "failed_to_spawn");
    assert_eq!(registry.pending_count().await, 0);
}

#[tokio::test]
async fn expire_only_drops_pending_when_terminal_dead() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let card_id = seed_card(&repo, &wave_id, "term-live").await;
    registry
        .register(entry(&repo, &card_id, &wave_id, "term-live").await)
        .await
        .unwrap();

    let dropped = registry.expire_dead_pending().await;

    assert_eq!(dropped, 0);
    assert_eq!(registry.pending_count().await, 1);
}

#[tokio::test]
async fn expire_dead_pending_projects_failed_status() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let card_id = seed_card(&repo, &wave_id, "term-exit").await;
    registry
        .register(entry(&repo, &card_id, &wave_id, "term-exit").await)
        .await
        .unwrap();
    repo.terminal_set_exit("term-exit", Some(0), false)
        .await
        .unwrap();

    let dropped = registry.expire_dead_pending().await;

    assert_eq!(dropped, 1);
    assert_eq!(registry.pending_count().await, 0);
    let card = projected_card(&repo, &card_id).await;
    assert_eq!(card.payload["codex_thread_status"], "failed_to_spawn");
}

#[tokio::test]
async fn expire_dead_pending_only_expires_entry_whose_terminal_died() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let card_id =
        seed_card_with_runtime_kind(&repo, &wave_id, "term-r1", WorkerSessionKind::CodexCard).await;
    let r1 = runtime_id_for_card(&repo, &card_id).await;
    registry
        .register(entry_with_runtime_id(&card_id, &wave_id, "term-r1", &r1))
        .await
        .unwrap();

    repo.terminal_delete("term-r1").await.unwrap();
    insert_terminal(&repo, &card_id, "term-r2").await;
    let r2 = supersede_runtime_for_card(
        &repo,
        &r1,
        &card_id,
        "term-r2",
        WorkerSessionKind::CodexCard,
    )
    .await;
    registry
        .register(entry_with_runtime_id(&card_id, &wave_id, "term-r2", &r2))
        .await
        .unwrap();

    let dropped = registry.expire_dead_pending().await;

    assert_eq!(dropped, 1);
    assert_eq!(registry.pending_count().await, 1);
    let old_runtime = runtime_by_id_tx_snapshot(&repo, &r1)
        .await
        .expect("old runtime");
    assert_eq!(old_runtime.status, WorkerSessionState::Superseded);
    let replacement_runtime = repo
        .session_projection_by_id(&r2)
        .await
        .unwrap()
        .expect("replacement runtime");
    assert_eq!(replacement_runtime.status, WorkerSessionState::TurnPending);
    assert_eq!(replacement_runtime.thread_id, None);

    let bound = registry.on_thread_started("T-r2-own").await.unwrap();
    assert_eq!(bound.as_deref(), Some(card_id.as_str()));
    let replacement_runtime = repo
        .session_projection_by_id(&r2)
        .await
        .unwrap()
        .expect("replacement runtime");
    assert_eq!(replacement_runtime.status, WorkerSessionState::Running);
    assert_eq!(replacement_runtime.thread_id.as_deref(), Some("T-r2-own"));
}

#[tokio::test]
async fn expire_drops_pending_when_terminal_row_deleted() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let card_id = seed_card(&repo, &wave_id, "term-deleted").await;
    registry
        .register(entry(&repo, &card_id, &wave_id, "term-deleted").await)
        .await
        .unwrap();
    repo.terminal_delete("term-deleted").await.unwrap();

    let dropped = registry.expire_dead_pending().await;

    assert_eq!(dropped, 1);
    assert_eq!(registry.pending_count().await, 0);
}

#[tokio::test]
async fn on_thread_started_stale_drop_does_not_cross_attribute_to_live_next() {
    // Followup gate #3 (PR6 R6 P2-A): when the FRONT pending entry is
    // dropped due to a dead terminal, we MUST NOT loop with the same
    // thread_id and bind it to the next-in-queue live entry. The thread
    // belongs (soft-deterministically) to the dropped card's TUI request,
    // and binding it to a different card would cross-attribute. We orphan
    // the thread_id and let the live card wait for its OWN thread/started.
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let dead_card = seed_card(&repo, &wave_id, "term-dead").await;
    repo.terminal_set_exit("term-dead", Some(1), false)
        .await
        .unwrap();
    registry
        .register(entry(&repo, &dead_card, &wave_id, "term-dead").await)
        .await
        .unwrap();
    let live_card = seed_pending(&repo, &registry, &wave_id, "term-live").await;

    let bound = registry.on_thread_started("T-live").await.unwrap();

    // Was the front (dead) entry dropped? Yes.
    assert_eq!(
        bound, None,
        "thread_id must be orphaned, not cross-attributed"
    );
    let dead = projected_card(&repo, &dead_card).await;
    assert_eq!(dead.payload["codex_thread_status"], "failed_to_spawn");
    // The live card is still pending — it'll receive its OWN thread/started later.
    assert_eq!(registry.pending_count().await, 1);
    let live_runtime = repo
        .session_projection_active_for_card(&live_card)
        .await
        .unwrap()
        .expect("live runtime");
    assert_eq!(live_runtime.thread_id, None);
    // When the live card's OWN thread/started arrives, it binds correctly.
    let next = registry.on_thread_started("T-live-own").await.unwrap();
    assert_eq!(next.as_deref(), Some(live_card.as_str()));
    let runtime = repo
        .session_projection_active_for_card(&live_card)
        .await
        .unwrap()
        .expect("live runtime");
    assert_eq!(runtime.thread_id.as_deref(), Some("T-live-own"));
}

#[tokio::test]
async fn on_thread_started_same_card_respawn_drops_old_runtime_without_cross_attribution() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let card_id =
        seed_card_with_runtime_kind(&repo, &wave_id, "term-r1", WorkerSessionKind::CodexCard).await;
    let r1 = runtime_id_for_card(&repo, &card_id).await;
    registry
        .register(entry_with_runtime_id(&card_id, &wave_id, "term-r1", &r1))
        .await
        .unwrap();
    let live_card = seed_pending(&repo, &registry, &wave_id, "term-live-behind").await;

    let r2 = new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    session_supersede_and_start_tx(
        &mut tx,
        &r1,
        WorkerSessionInit {
            id: r2.clone(),
            card_id: card_id.clone(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::TurnPending,
            terminal_run_id: Some("term-r1".to_string()),
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let bound = registry.on_thread_started("T-r1-late").await.unwrap();

    assert_eq!(bound, None);
    assert_eq!(registry.pending_count().await, 1);
    let old_runtime = runtime_by_id_tx_snapshot(&repo, &r1)
        .await
        .expect("old runtime");
    assert_eq!(old_runtime.thread_id, None);
    let new_runtime = repo
        .session_projection_by_id(&r2)
        .await
        .unwrap()
        .expect("new runtime");
    assert_eq!(new_runtime.status, WorkerSessionState::TurnPending);
    assert_eq!(new_runtime.thread_id, None);
    let active_runtime = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("active respawned runtime");
    assert_eq!(active_runtime.id, r2);
    assert_eq!(active_runtime.status, WorkerSessionState::TurnPending);
    let new_session = repo
        .session_get(&WorkerSessionId::from(r2.clone()))
        .await
        .unwrap()
        .expect("new worker session");
    assert_eq!(new_session.thread_id, None);
    assert_eq!(new_session.agent_session_id, None);

    let next = registry.on_thread_started("T-live-own").await.unwrap();
    assert_eq!(next.as_deref(), Some(live_card.as_str()));
    let live_runtime = repo
        .session_projection_active_for_card(&live_card)
        .await
        .unwrap()
        .expect("live runtime");
    assert_eq!(live_runtime.thread_id.as_deref(), Some("T-live-own"));
}

#[tokio::test]
async fn on_thread_started_same_card_respawn_queues_and_binds_replacement_runtime() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let card_id =
        seed_card_with_runtime_kind(&repo, &wave_id, "term-r1", WorkerSessionKind::CodexCard).await;
    let r1 = runtime_id_for_card(&repo, &card_id).await;
    let r2 = new_id();

    registry
        .register(entry_with_runtime_id(&card_id, &wave_id, "term-r1", &r1))
        .await
        .unwrap();
    registry
        .register(entry_with_runtime_id(&card_id, &wave_id, "term-r1", &r2))
        .await
        .unwrap();
    assert_eq!(registry.pending_count().await, 2);

    let mut tx = repo.pool().begin().await.unwrap();
    session_supersede_and_start_tx(
        &mut tx,
        &r1,
        WorkerSessionInit {
            id: r2.clone(),
            card_id: card_id.clone(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::TurnPending,
            terminal_run_id: Some("term-r1".to_string()),
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let r1_bound = registry.on_thread_started("T-r1-late").await.unwrap();

    assert_eq!(r1_bound, None);
    assert_eq!(registry.pending_count().await, 1);
    let old_runtime = runtime_by_id_tx_snapshot(&repo, &r1)
        .await
        .expect("old runtime");
    assert_eq!(old_runtime.status, WorkerSessionState::Superseded);
    assert_eq!(old_runtime.thread_id, None);
    let active_runtime = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("active respawned runtime");
    assert_eq!(active_runtime.id, r2);
    assert_eq!(active_runtime.status, WorkerSessionState::TurnPending);
    assert_eq!(active_runtime.thread_id, None);
    let r2_session = repo
        .session_get(&WorkerSessionId::from(r2.clone()))
        .await
        .unwrap()
        .expect("replacement worker session");
    assert_eq!(r2_session.thread_id, None);
    assert_eq!(r2_session.agent_session_id, None);

    let r2_bound = registry.on_thread_started("T-r2-own").await.unwrap();

    assert_eq!(r2_bound.as_deref(), Some(card_id.as_str()));
    assert_eq!(registry.pending_count().await, 0);
    let replacement_runtime = repo
        .session_projection_by_id(&r2)
        .await
        .unwrap()
        .expect("replacement runtime");
    assert_eq!(replacement_runtime.status, WorkerSessionState::Running);
    assert_eq!(replacement_runtime.thread_id.as_deref(), Some("T-r2-own"));
    let replacement_session = repo
        .session_get(&WorkerSessionId::from(r2.clone()))
        .await
        .unwrap()
        .expect("replacement worker session");
    assert_eq!(replacement_session.thread_id.as_deref(), Some("T-r2-own"));
    assert_eq!(replacement_session.agent_session_id, None);
}

#[tokio::test]
async fn on_thread_started_stale_front_drop_orphans_only_one_per_event() {
    // Per the gate #3 mitigation: each thread/started can only drop the
    // CURRENT front entry; if the new front is also dead, it stays in the
    // queue (will be cleaned up by the next thread/started or by TTL
    // expire). This is intentional — bounded effect per event.
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    for label in ["term-dead-a", "term-dead-b"] {
        let card_id = seed_card(&repo, &wave_id, label).await;
        repo.terminal_set_exit(label, Some(1), false).await.unwrap();
        registry
            .register(entry(&repo, &card_id, &wave_id, label).await)
            .await
            .unwrap();
    }

    assert_eq!(
        registry.on_thread_started("T-orphan-1").await.unwrap(),
        None
    );
    // Only the front (term-dead-a) was dropped; term-dead-b remains.
    assert_eq!(registry.pending_count().await, 1);
    // A second thread/started drops the next one.
    assert_eq!(
        registry.on_thread_started("T-orphan-2").await.unwrap(),
        None
    );
    assert_eq!(registry.pending_count().await, 0);
}

#[tokio::test]
async fn concurrent_registrations_preserve_fifo_order() {
    let (repo, events, wave_id) = boot().await;
    let registry = Arc::new(PendingThreadStartRegistry::new(repo.clone(), events));
    let a = seed_card(&repo, &wave_id, "term-a").await;
    let b = seed_card(&repo, &wave_id, "term-b").await;
    let entry_a = entry(&repo, &a, &wave_id, "term-a").await;
    let entry_b = entry(&repo, &b, &wave_id, "term-b").await;

    let (tx, rx) = tokio::sync::oneshot::channel();
    let reg_a = registry.clone();
    let task_a = tokio::spawn(async move {
        reg_a.register(entry_a).await.unwrap();
        tx.send(()).unwrap();
    });
    let reg_b = registry.clone();
    let task_b = tokio::spawn(async move {
        rx.await.unwrap();
        reg_b.register(entry_b).await.unwrap();
    });
    task_a.await.unwrap();
    task_b.await.unwrap();

    assert_eq!(registry.on_thread_started("T-1").await.unwrap(), Some(a));
    assert_eq!(registry.on_thread_started("T-2").await.unwrap(), Some(b));
}

#[tokio::test]
async fn unknown_thread_started_when_no_pending() {
    let (repo, events, _wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo, events);

    assert_eq!(registry.on_thread_started("T-orphan").await.unwrap(), None);
    assert_eq!(registry.pending_count().await, 0);
}

#[tokio::test]
async fn already_mapped_thread_does_not_consume_pending() {
    let (repo, registry, server, wave_id) = boot_pending_server().await;
    let mapped = seed_card(&repo, &wave_id, "term-mapped").await;
    repo.session_projection_complete_for_card(&mapped, WorkerSessionState::Failed)
        .await
        .unwrap();
    start_runtime_for_card_with_thread(
        &repo,
        &mapped,
        "term-mapped",
        WorkerSessionKind::CodexCard,
        Some("T-mapped"),
    )
    .await;
    let pending_card = seed_pending(&repo, &registry, &wave_id, "term-empty").await;

    assert!(
        !server
            .handle_thread_started_notification_for_test("T-mapped")
            .await
            .unwrap()
    );

    assert_eq!(registry.pending_count().await, 1);
    assert_eq!(
        server.cached_card_for_thread("T-mapped").as_deref(),
        Some(mapped.as_str())
    );
    let pending_runtime = repo
        .session_projection_active_for_card(&pending_card)
        .await
        .unwrap()
        .expect("pending runtime");
    assert_eq!(pending_runtime.thread_id, None);
}

#[tokio::test]
async fn kernel_initiated_threads_bypass_pending_registry() {
    let (repo, registry, server, wave_id) = boot_pending_server().await;
    let card_id = seed_pending(&repo, &registry, &wave_id, "term-a").await;

    server
        .mark_kernel_initiated_thread_for_test("T-kernel")
        .await;
    assert!(
        !server
            .handle_thread_started_notification_for_test("T-kernel")
            .await
            .unwrap(),
        "kernel-initiated thread/started should dispatch normally, not bind pending"
    );

    assert_eq!(registry.pending_count().await, 1);
    let runtime = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("runtime");
    assert_eq!(runtime.thread_id, None);
}

#[tokio::test]
async fn tui_fresh_start_thread_binds_to_pending_after_kernel_initiated_skipped() {
    let (repo, registry, server, wave_id) = boot_pending_server().await;
    let card_id = seed_pending(&repo, &registry, &wave_id, "term-a").await;

    server
        .mark_kernel_initiated_thread_for_test("T-kernel")
        .await;
    assert!(
        !server
            .handle_thread_started_notification_for_test("T-kernel")
            .await
            .unwrap()
    );
    assert!(
        server
            .handle_thread_started_notification_for_test("T-tui")
            .await
            .unwrap()
    );

    assert_eq!(registry.pending_count().await, 0);
    let runtime = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("runtime");
    assert_eq!(runtime.thread_id.as_deref(), Some("T-tui"));
}

#[tokio::test]
async fn register_and_spawn_serializes_with_spawn_serial_lock() {
    let (repo, events, wave_id) = boot().await;
    let registry = Arc::new(PendingThreadStartRegistry::new(repo.clone(), events));
    let spawn_serial = Mutex::new(());
    let observed = Mutex::new(Vec::<&'static str>::new());
    let a = seed_card(&repo, &wave_id, "term-a").await;
    let b = seed_card(&repo, &wave_id, "term-b").await;
    let entry_a = entry(&repo, &a, &wave_id, "term-a").await;
    let entry_b = entry(&repo, &b, &wave_id, "term-b").await;

    let (a_registered_tx, a_registered_rx) = tokio::sync::oneshot::channel();
    let task_a = async {
        let _guard = spawn_serial.lock().await;
        registry.register(entry_a).await.unwrap();
        observed.lock().await.push("register-a");
        a_registered_tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        observed.lock().await.push("spawn-a");
    };

    let task_b = async {
        a_registered_rx.await.unwrap();
        let _guard = spawn_serial.lock().await;
        registry.register(entry_b).await.unwrap();
        observed.lock().await.push("register-b");
        observed.lock().await.push("spawn-b");
    };

    tokio::join!(task_a, task_b);
    assert_eq!(
        observed.lock().await.as_slice(),
        ["register-a", "spawn-a", "register-b", "spawn-b"]
    );
    assert_eq!(
        registry.on_thread_started("T-1").await.unwrap(),
        Some(a.clone())
    );
    assert_eq!(
        registry.on_thread_started("T-2").await.unwrap(),
        Some(b.clone())
    );
}

#[tokio::test]
async fn expire_runs_periodically_via_background_task() {
    let (repo, events, wave_id) = boot().await;
    let registry = Arc::new(PendingThreadStartRegistry::new(repo.clone(), events));
    let card_id = seed_card(&repo, &wave_id, "term-old").await;
    let mut old_entry = entry(&repo, &card_id, &wave_id, "term-old").await;
    old_entry.registered_at = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .expect("instant subtraction");
    registry.register(old_entry).await.unwrap();

    let handle = spawn_periodic_expire_task(
        registry.clone(),
        Duration::from_millis(10),
        Duration::from_millis(50),
    );
    for _ in 0..20 {
        if registry.pending_count().await == 0 {
            handle.abort();
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    handle.abort();
    assert_eq!(registry.pending_count().await, 0);
}

#[tokio::test]
async fn remove_by_runtime_drops_pending_entry() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let a = seed_card(&repo, &wave_id, "term-a").await;
    let b = seed_card(&repo, &wave_id, "term-b").await;
    let c = seed_card(&repo, &wave_id, "term-c").await;
    let runtime_b = runtime_id_for_card(&repo, &b).await;
    registry
        .register(entry(&repo, &a, &wave_id, "term-a").await)
        .await
        .unwrap();
    registry
        .register(entry(&repo, &b, &wave_id, "term-b").await)
        .await
        .unwrap();
    registry
        .register(entry(&repo, &c, &wave_id, "term-c").await)
        .await
        .unwrap();

    assert!(registry.remove_by_runtime(&runtime_b).await);
    assert_eq!(registry.pending_count().await, 2);
    assert_eq!(
        registry.on_thread_started("T-1").await.unwrap(),
        Some(a.clone())
    );
    assert_eq!(
        registry.on_thread_started("T-2").await.unwrap(),
        Some(c.clone())
    );
}

#[tokio::test]
async fn remove_by_runtime_returns_false_for_unknown() {
    let (repo, events, _wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo, events);

    assert!(!registry.remove_by_runtime("never-registered").await);
}

#[tokio::test]
async fn compensation_remove_uses_runtime_id_for_same_card_spawns() {
    let (repo, events, wave_id) = boot().await;
    let registry = Arc::new(PendingThreadStartRegistry::new(
        repo.clone(),
        events.clone(),
    ));
    let card_id =
        seed_card_with_runtime_kind(&repo, &wave_id, "term-r1", WorkerSessionKind::CodexCard).await;
    let r1 = runtime_id_for_card(&repo, &card_id).await;
    registry
        .register(entry_with_runtime_id(&card_id, &wave_id, "term-r1", &r1))
        .await
        .unwrap();

    let r2 = new_id();
    registry
        .register(entry_with_runtime_id(&card_id, &wave_id, "term-r2", &r2))
        .await
        .unwrap();
    assert_eq!(registry.pending_count().await, 2);

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let adapter = CodexAdapter::new(
        route_repo.clone(),
        Arc::new(CodexClient::new_stub()),
        SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), Some(registry.clone())),
        registry.clone(),
        Arc::new(Mutex::new(())),
        Default::default(),
        WaveCoveCache::default(),
    );
    let mut output = TxOutput::new("runtime", Some(r2.clone()), json!({}));
    output.data = json!({
        "card_id": card_id,
        "runtime_id": r2,
        "wave_id": wave_id,
        "terminal_id": "term-r2",
        "cwd": "/workspace",
        "env": {},
        "prompt": null,
    });
    let op = dummy_codex_operation();
    let compensation = adapter
        .plan_compensation(
            PhaseTag::AppServerInteract,
            "forced test compensation",
            &output,
            &op,
        )
        .await
        .unwrap();
    let pending_step = compensation
        .steps
        .iter()
        .find(|step| step.op == "pending_codex_threads_remove_by_card")
        .expect("pending removal step");
    assert_eq!(pending_step.args["runtime_id"].as_str(), Some(r2.as_str()));

    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    let spawn_ctx = SpawnCtx::new(
        route_repo,
        Arc::new(SqlxOperationRepo::new(repo.pool().clone())),
        Arc::new(DaemonClient::new_stub()),
        terminal_renderer,
        events,
        OperationCompletionBus::new(),
    );
    adapter
        .compensate_step(pending_step, &output, &op, &spawn_ctx)
        .await
        .unwrap();

    assert_eq!(registry.pending_count().await, 1);
    assert!(registry.remove_by_runtime(&r1).await);
    assert_eq!(registry.pending_count().await, 0);

    registry
        .register(entry_with_runtime_id(&card_id, &wave_id, "term-r1", &r1))
        .await
        .unwrap();
    registry
        .register(entry_with_runtime_id(&card_id, &wave_id, "term-r2", &r2))
        .await
        .unwrap();
    let mut output = TxOutput::new("runtime", Some(r1.clone()), json!({}));
    output.data = json!({
        "card_id": card_id,
        "runtime_id": r1,
        "wave_id": wave_id,
        "terminal_id": "term-r1",
        "cwd": "/workspace",
        "env": {},
        "prompt": null,
    });
    let compensation = adapter
        .plan_compensation(
            PhaseTag::AppServerInteract,
            "forced test compensation",
            &output,
            &op,
        )
        .await
        .unwrap();
    let pending_step = compensation
        .steps
        .iter()
        .find(|step| step.op == "pending_codex_threads_remove_by_card")
        .expect("pending removal step");
    assert_eq!(pending_step.args["runtime_id"].as_str(), Some(r1.as_str()));
    adapter
        .compensate_step(pending_step, &output, &op, &spawn_ctx)
        .await
        .unwrap();

    assert_eq!(registry.pending_count().await, 1);
    assert!(registry.remove_by_runtime(&r2).await);
    assert_eq!(registry.pending_count().await, 0);
}

#[tokio::test]
async fn bind_entry_emits_event_through_canonical_write_path() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let card_id = seed_pending(&repo, &registry, &wave_id, "term-a").await;

    registry.on_thread_started("T-history").await.unwrap();

    let rows = repo.events_since(0, None).await.unwrap();
    assert!(
        rows.iter().any(|(id, _version, _scope, event)| {
            *id > 0 && matches!(event, Event::CardUpdated(card) if card.id.as_str() == card_id)
        }),
        "card.updated bind event must be present in durable event history"
    );
}
