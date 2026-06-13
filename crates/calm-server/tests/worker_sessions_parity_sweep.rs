use std::sync::atomic::{AtomicU64, Ordering};

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::model::{NewCard, NewCove, NewWave, new_id, now_ms};
use serde_json::json;

#[tokio::test]
async fn worker_sessions_parity_sweep_detects_unmirrored_runtime() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    // This test deliberately seeds an unmirrored runtime so the sweep can flag it.
    repo.disable_worker_session_parity_on_drop_for_test();
    let cove = repo
        .cove_create(NewCove {
            name: "parity-sweep".into(),
            color: "#101010".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "parity sweep".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    let runtime_id = new_id();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO runtimes
           (id, card_id, kind, agent_provider, status, created_at_ms, updated_at_ms)
           VALUES (?1, ?2, 'codex', 'codex', 'running', ?3, ?3)"#,
    )
    .bind(&runtime_id)
    .bind(card.id.as_str())
    .bind(now)
    .execute(repo.pool())
    .await
    .unwrap();

    let counter = AtomicU64::new(0);
    let divergences = calm_server::worker_sessions_parity_sweep::sweep(repo.pool(), &counter)
        .await
        .unwrap();

    assert_eq!(divergences, 1);
    assert_eq!(counter.load(Ordering::Relaxed), 1);
}
