use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::model::{Card, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::session_projection_lookup::{
    resolve_active_thread_for_card, resolve_card_for_thread, resolve_claude_session_for_card,
};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use serde_json::{Value, json};

async fn fresh_repo() -> SqlxRepo {
    SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite repo")
}

async fn make_wave(repo: &SqlxRepo) -> calm_server::model::Wave {
    let cove = repo
        .cove_create(NewCove {
            name: "runtime lookup".into(),
            color: "#101010".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    repo.wave_create(NewWave {
        workflow_input: None,
        cove_id: cove.id,
        title: "runtime lookup".into(),
        sort: None,
        cwd: "/workspace".into(),
        workflow_id: None,
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .expect("create wave")
}

async fn make_card(repo: &SqlxRepo, kind: &str, payload: Value) -> Card {
    let wave = make_wave(repo).await;
    repo.card_create(NewCard {
        wave_id: wave.id,
        kind: kind.into(),
        sort: None,
        payload,
    })
    .await
    .expect("create card")
}

fn runtime_init(
    card_id: String,
    kind: WorkerSessionKind,
    agent_provider: Option<AgentProvider>,
) -> WorkerSessionInit {
    WorkerSessionInit {
        id: new_id(),
        card_id,
        kind,
        agent_provider,
        status: WorkerSessionState::Running,
        terminal_run_id: None,
        thread_id: None,
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        spawn_op_id: None,
        now_ms: now_ms(),
    }
}

#[tokio::test]
async fn resolve_active_thread_for_card_prefers_runtime() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex", json!({})).await;
    let mut init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::CodexCard,
        Some(AgentProvider::Codex),
    );
    init.thread_id = Some("thread-runtime".into());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(&mut tx, init).await.unwrap();
    tx.commit().await.unwrap();

    let thread = resolve_active_thread_for_card(&repo, card.id.as_str())
        .await
        .unwrap();
    assert_eq!(thread.as_deref(), Some("thread-runtime"));
}

#[tokio::test]
async fn resolve_active_thread_for_card_returns_none_without_runtime_thread() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex", json!({})).await;

    let thread = resolve_active_thread_for_card(&repo, card.id.as_str())
        .await
        .unwrap();
    assert_eq!(thread, None);
}

#[tokio::test]
async fn resolve_card_for_thread_prefers_runtime() {
    let repo = fresh_repo().await;
    let card = make_card(&repo, "codex", json!({})).await;
    let mut init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::CodexCard,
        Some(AgentProvider::Codex),
    );
    init.thread_id = Some("thread-runtime".into());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(&mut tx, init).await.unwrap();
    tx.commit().await.unwrap();

    let card_id = resolve_card_for_thread(&repo, AgentProvider::Codex, "thread-runtime")
        .await
        .unwrap();
    assert_eq!(card_id.as_deref(), Some(card.id.as_str()));
}

#[tokio::test]
async fn resolve_card_for_thread_returns_none_without_runtime_thread() {
    let repo = fresh_repo().await;

    let card_id = resolve_card_for_thread(&repo, AgentProvider::Codex, "thread-missing")
        .await
        .unwrap();
    assert_eq!(card_id, None);
}

#[tokio::test]
async fn resolve_claude_session_for_card_prefers_runtime() {
    let repo = fresh_repo().await;
    let card = make_card(
        &repo,
        "claude",
        json!({"claude_session_id": "payload-session"}),
    )
    .await;
    let mut init = runtime_init(
        card.id.to_string(),
        WorkerSessionKind::ClaudeCard,
        Some(AgentProvider::Claude),
    );
    init.session_id = Some("runtime-session".into());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(&mut tx, init).await.unwrap();
    tx.commit().await.unwrap();

    let session = resolve_claude_session_for_card(&repo, card.id.as_str())
        .await
        .unwrap();
    assert_eq!(session.as_deref(), Some("runtime-session"));
}

#[tokio::test]
async fn resolve_claude_session_for_card_falls_back_to_payload() {
    let repo = fresh_repo().await;
    let card = make_card(
        &repo,
        "claude",
        json!({"claude_session_id": "payload-session"}),
    )
    .await;

    let session = resolve_claude_session_for_card(&repo, card.id.as_str())
        .await
        .unwrap();
    assert_eq!(session.as_deref(), Some("payload-session"));
}
