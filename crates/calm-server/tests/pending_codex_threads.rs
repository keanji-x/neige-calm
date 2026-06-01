use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::model::{NewCard, NewCove, NewWave};
use calm_server::pending_codex_threads::{
    PendingEntry, PendingThreadStartRegistry, spawn_periodic_expire_task,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use serde_json::json;
use tokio::sync::Mutex;

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
    repo.card_create(NewCard {
        wave_id: wave_id.into(),
        kind: "codex".into(),
        sort: None,
        payload: json!({
            "schemaVersion": 1,
            "terminal_id": terminal_id,
            "codex_thread_status": "pending_thread_start"
        }),
    })
    .await
    .unwrap()
    .id
    .to_string()
}

fn entry(card_id: &str, wave_id: &str, terminal_id: &str) -> PendingEntry {
    PendingEntry::new(
        card_id.to_string(),
        Some(wave_id.to_string()),
        terminal_id.to_string(),
    )
}

async fn seed_pending(
    repo: &SqlxRepo,
    registry: &PendingThreadStartRegistry,
    wave_id: &str,
    terminal_id: &str,
) -> String {
    let card_id = seed_card(repo, wave_id, terminal_id).await;
    registry
        .register(entry(&card_id, wave_id, terminal_id))
        .await
        .unwrap();
    card_id
}

#[tokio::test]
async fn register_and_bind_in_arrival_order() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let a = seed_card(&repo, &wave_id, "term-a").await;
    let b = seed_card(&repo, &wave_id, "term-b").await;

    registry
        .register(entry(&a, &wave_id, "term-a"))
        .await
        .unwrap();
    registry
        .register(entry(&b, &wave_id, "term-b"))
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

    assert_eq!(
        repo.card_codex_thread_get_by_card(&a)
            .await
            .unwrap()
            .unwrap()
            .thread_id,
        "T-1"
    );
    assert_eq!(
        repo.card_codex_thread_get_by_card(&b)
            .await
            .unwrap()
            .unwrap()
            .thread_id,
        "T-2"
    );
}

#[tokio::test]
async fn bind_persists_to_card_codex_threads_and_payload() {
    let (repo, events, wave_id) = boot().await;
    let mut rx = events.subscribe();
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let card_id = seed_card(&repo, &wave_id, "term-a").await;
    registry
        .register(entry(&card_id, &wave_id, "term-a"))
        .await
        .unwrap();

    registry.on_thread_started("T-bind").await.unwrap();

    let mapping = repo
        .card_codex_thread_get_by_card(&card_id)
        .await
        .unwrap()
        .expect("mapping row");
    assert_eq!(mapping.thread_id, "T-bind");
    assert_eq!(mapping.wave_id.as_deref(), Some(wave_id.as_str()));

    let card = repo.card_get(&card_id).await.unwrap().expect("card row");
    assert_eq!(card.payload["codex_thread_id"], "T-bind");
    assert_eq!(card.payload["codex_thread_status"], "started");

    let env = rx.recv().await.unwrap();
    assert!(env.id > 0, "bind event must be persisted for cursor replay");
    match env.event {
        Event::CardUpdated(card) => assert_eq!(card.id.as_str(), card_id),
        other => panic!("expected card.updated, got {other:?}"),
    }
}

#[tokio::test]
async fn expire_drops_abandoned_entries_past_ttl() {
    let (repo, events, wave_id) = boot().await;
    let registry = PendingThreadStartRegistry::new(repo.clone(), events);
    let old = seed_card(&repo, &wave_id, "term-old").await;
    let fresh = seed_card(&repo, &wave_id, "term-fresh").await;

    let mut old_entry = entry(&old, &wave_id, "term-old");
    old_entry.registered_at = Instant::now()
        .checked_sub(Duration::from_secs(30))
        .expect("instant subtraction");
    registry.register(old_entry).await.unwrap();
    registry
        .register(entry(&fresh, &wave_id, "term-fresh"))
        .await
        .unwrap();

    assert_eq!(registry.expire(Duration::from_secs(10)).await, 1);
    assert_eq!(registry.pending_count().await, 1);
    assert_eq!(
        registry.on_thread_started("T-fresh").await.unwrap(),
        Some(fresh.clone())
    );
}

#[tokio::test]
async fn concurrent_registrations_preserve_fifo_order() {
    let (repo, events, wave_id) = boot().await;
    let registry = Arc::new(PendingThreadStartRegistry::new(repo.clone(), events));
    let a = seed_card(&repo, &wave_id, "term-a").await;
    let b = seed_card(&repo, &wave_id, "term-b").await;

    let (tx, rx) = tokio::sync::oneshot::channel();
    let reg_a = registry.clone();
    let wave_a = wave_id.clone();
    let a_for_task = a.clone();
    let task_a = tokio::spawn(async move {
        reg_a
            .register(entry(&a_for_task, &wave_a, "term-a"))
            .await
            .unwrap();
        tx.send(()).unwrap();
    });
    let reg_b = registry.clone();
    let wave_b = wave_id.clone();
    let b_for_task = b.clone();
    let task_b = tokio::spawn(async move {
        rx.await.unwrap();
        reg_b
            .register(entry(&b_for_task, &wave_b, "term-b"))
            .await
            .unwrap();
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
    assert!(
        repo.card_codex_thread_get_by_card(&card_id)
            .await
            .unwrap()
            .is_none()
    );
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
    assert_eq!(
        repo.card_codex_thread_get_by_card(&card_id)
            .await
            .unwrap()
            .unwrap()
            .thread_id,
        "T-tui"
    );
}

#[tokio::test]
async fn register_and_spawn_serializes_with_spawn_serial_lock() {
    let (repo, events, wave_id) = boot().await;
    let registry = Arc::new(PendingThreadStartRegistry::new(repo.clone(), events));
    let spawn_serial = Mutex::new(());
    let observed = Mutex::new(Vec::<&'static str>::new());
    let a = seed_card(&repo, &wave_id, "term-a").await;
    let b = seed_card(&repo, &wave_id, "term-b").await;

    let (a_registered_tx, a_registered_rx) = tokio::sync::oneshot::channel();
    let task_a = async {
        let _guard = spawn_serial.lock().await;
        registry
            .register(entry(&a, &wave_id, "term-a"))
            .await
            .unwrap();
        observed.lock().await.push("register-a");
        a_registered_tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        observed.lock().await.push("spawn-a");
    };

    let task_b = async {
        a_registered_rx.await.unwrap();
        let _guard = spawn_serial.lock().await;
        registry
            .register(entry(&b, &wave_id, "term-b"))
            .await
            .unwrap();
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
    let mut old_entry = entry(&card_id, &wave_id, "term-old");
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
