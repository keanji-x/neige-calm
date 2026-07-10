use super::session_projection::runtime_get_projectable_for_card_from_pool;
use super::*;
use crate::model::{CardRole, NewCard, NewCove, NewWave, RequestTheme, new_id};
use crate::session_projection_repo::{
    AgentProvider, RuntimeId, Tx as WorkerSessionProjectionTx, WorkerSessionInit,
    WorkerSessionKind, WorkerSessionProjection,
};
use serde_json::json;
use sqlx::SqlitePool;

#[derive(Clone)]
pub(super) struct RuntimeReadCase {
    pub(super) label: &'static str,
    pub(super) card_kind: &'static str,
    pub(super) kind: WorkerSessionKind,
    pub(super) agent_provider: Option<AgentProvider>,
    pub(super) status: WorkerSessionState,
}

pub(super) struct KeyedRuntimeSeed {
    pub(super) label: &'static str,
    pub(super) card_kind: &'static str,
    pub(super) kind: WorkerSessionKind,
    pub(super) agent_provider: Option<AgentProvider>,
    pub(super) thread_id: Option<&'static str>,
    pub(super) session_id: Option<&'static str>,
    pub(super) now_ms: i64,
}

pub(super) fn runtime_read_cases() -> Vec<RuntimeReadCase> {
    vec![
        RuntimeReadCase {
            label: "terminal",
            card_kind: "terminal",
            kind: WorkerSessionKind::Terminal,
            agent_provider: None,
            status: WorkerSessionState::Starting,
        },
        RuntimeReadCase {
            label: "codex-card",
            card_kind: "codex",
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
        },
        RuntimeReadCase {
            label: "claude-card",
            card_kind: "claude",
            kind: WorkerSessionKind::ClaudeCard,
            agent_provider: Some(AgentProvider::Claude),
            status: WorkerSessionState::Idle,
        },
        RuntimeReadCase {
            label: "shared-spec",
            card_kind: "codex",
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::TurnPending,
        },
    ]
}

pub(super) async fn fresh_repo() -> SqlxRepo {
    SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite repo")
}

pub(super) async fn create_card_in_tx(
    repo: &SqlxRepo,
    tx: &mut WorkerSessionProjectionTx<'_>,
    label: &str,
    card_kind: &str,
) -> String {
    let cove = cove_create_tx(
        tx,
        NewCove {
            name: format!("read flip {label}"),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .expect("create cove");
    let wave = wave_create_tx(
        tx,
        NewWave {
            cove_id: cove.id,
            title: format!("read flip {label}"),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .expect("create wave");
    let card_id = format!("card-read-flip-{label}");
    let card = card_create_with_id_tx(
        tx,
        card_id,
        NewCard {
            wave_id: wave.id,
            kind: card_kind.into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "case": label}),
        },
        CardRole::Worker,
        true,
        repo.card_role_cache(),
    )
    .await
    .expect("create card");
    card.id.to_string()
}

pub(super) async fn seed_runtime(
    repo: &SqlxRepo,
    case: RuntimeReadCase,
    now_ms: i64,
) -> WorkerSessionProjection {
    let mut tx = repo.pool().begin().await.expect("begin seed tx");
    let card_id = create_card_in_tx(repo, &mut tx, case.label, case.card_kind).await;
    let runtime = session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: format!("rt-read-flip-{}", case.label),
            card_id,
            kind: case.kind,
            agent_provider: case.agent_provider,
            status: case.status,
            terminal_run_id: None,
            thread_id: Some(format!("thread-{}", case.label)),
            session_id: Some(format!("agent-session-{}", case.label)),
            active_turn_id: Some(format!("turn-{}", case.label)),
            handle_state_json: Some(json!({"case": case.label})),
            spawn_op_id: None,
            now_ms,
        },
    )
    .await
    .expect("start runtime");
    tx.commit().await.expect("commit seed tx");
    runtime
}

pub(super) async fn seed_runtime_with_keys(
    repo: &SqlxRepo,
    seed: KeyedRuntimeSeed,
) -> WorkerSessionProjection {
    let mut tx = repo.pool().begin().await.expect("begin keyed seed tx");
    let card_id = create_card_in_tx(repo, &mut tx, seed.label, seed.card_kind).await;
    let runtime = session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: format!("rt-read-flip-{}", seed.label),
            card_id,
            kind: seed.kind,
            agent_provider: seed.agent_provider,
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: seed.thread_id.map(str::to_string),
            session_id: seed.session_id.map(str::to_string),
            active_turn_id: Some(format!("turn-{}", seed.label)),
            handle_state_json: Some(json!({"case": seed.label})),
            spawn_op_id: None,
            now_ms: seed.now_ms,
        },
    )
    .await
    .expect("start keyed runtime");
    tx.commit().await.expect("commit keyed seed tx");
    runtime
}

pub(super) async fn seed_terminal_runtime(
    repo: &SqlxRepo,
    label: &'static str,
) -> (WorkerSessionProjection, String) {
    let mut tx = repo.pool().begin().await.expect("begin terminal seed tx");
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: format!("read flip {label}"),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .expect("create terminal cove");
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            cove_id: cove.id,
            title: format!("read flip {label}"),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .expect("create terminal wave");
    let runtime_id = format!("rt-read-flip-{label}");
    let (_card, terminal) = card_with_terminal_create_tx(
        &mut tx,
        format!("card-read-flip-{label}"),
        &runtime_id,
        None,
        wave.id,
        None,
        "bash".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Worker,
        true,
        repo.card_role_cache(),
        RequestTheme::default_dark(),
    )
    .await
    .expect("create terminal card");
    let runtime = session_projection_by_id_tx(&mut tx, &runtime_id)
        .await
        .expect("read seeded terminal runtime")
        .expect("seeded terminal runtime exists");
    tx.commit().await.expect("commit terminal seed tx");
    (runtime, terminal.id)
}

pub(super) async fn seed_codex_terminal_card(
    repo: &SqlxRepo,
    label: &'static str,
) -> (String, String, RuntimeId) {
    let mut tx = repo
        .pool()
        .begin()
        .await
        .expect("begin codex terminal seed tx");
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: format!("read flip {label}"),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .expect("create codex terminal cove");
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            cove_id: cove.id,
            title: format!("read flip {label}"),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .expect("create codex terminal wave");
    let runtime_id = format!("rt-read-flip-{label}-initial");
    let (card, terminal, _mcp_token) = card_with_codex_create_tx(
        &mut tx,
        format!("card-read-flip-{label}"),
        &runtime_id,
        None,
        wave.id,
        None,
        "/tmp".into(),
        json!({}),
        None,
        None,
        None,
        CardRole::Spec,
        false,
        repo.card_role_cache(),
        RequestTheme::default_dark(),
    )
    .await
    .expect("create codex terminal card");
    tx.commit().await.expect("commit codex terminal seed tx");
    (card.id.to_string(), terminal.id, runtime_id)
}

pub(super) async fn age_terminal_past_grace(repo: &SqlxRepo, terminal_id: &str) {
    let res = sqlx::query("UPDATE terminals SET created_at = 1 WHERE id = ?1")
        .bind(terminal_id)
        .execute(repo.pool())
        .await
        .expect("age terminal past grace");
    assert_eq!(res.rows_affected(), 1);
}

pub(super) struct ProjectableHistory {
    pub(super) card_id: String,
    pub(super) superseded: WorkerSessionProjection,
    pub(super) exited: WorkerSessionProjection,
    pub(super) active: Option<WorkerSessionProjection>,
}

pub(super) fn projectable_runtime_init(
    card_id: &str,
    label: &str,
    slot: &str,
    status: WorkerSessionState,
    now_ms: i64,
) -> WorkerSessionInit {
    WorkerSessionInit {
        id: format!("rt-projectable-{label}-{slot}"),
        card_id: card_id.to_string(),
        kind: WorkerSessionKind::CodexCard,
        agent_provider: Some(AgentProvider::Codex),
        status,
        terminal_run_id: None,
        thread_id: Some(format!("thread-{label}-{slot}")),
        session_id: Some(format!("agent-session-{label}-{slot}")),
        active_turn_id: Some(format!("turn-{label}-{slot}")),
        handle_state_json: Some(json!({"label": label, "slot": slot})),
        spawn_op_id: None,
        now_ms,
    }
}

pub(super) fn deferred_projectable_placeholder_init(
    card_id: &str,
    placeholder_id: &str,
    now_ms: i64,
) -> WorkerSessionInit {
    WorkerSessionInit {
        id: placeholder_id.to_string(),
        card_id: card_id.to_string(),
        kind: WorkerSessionKind::SharedSpec,
        agent_provider: Some(AgentProvider::Codex),
        status: WorkerSessionState::Starting,
        terminal_run_id: None,
        thread_id: None,
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        spawn_op_id: None,
        now_ms,
    }
}

pub(super) async fn seed_projectable_history(
    repo: &SqlxRepo,
    label: &'static str,
    include_active: bool,
) -> ProjectableHistory {
    let mut tx = repo.pool().begin().await.expect("begin projectable tx");
    let card_id = create_card_in_tx(repo, &mut tx, label, "codex").await;
    let older = session_start_runtime_tx(
        &mut tx,
        projectable_runtime_init(
            &card_id,
            label,
            "older",
            WorkerSessionState::Running,
            10_000,
        ),
    )
    .await
    .expect("start older runtime");
    let exited = session_supersede_and_start_tx(
        &mut tx,
        &older.id,
        projectable_runtime_init(
            &card_id,
            label,
            "exited",
            WorkerSessionState::Exited,
            20_000,
        ),
    )
    .await
    .expect("supersede older runtime with exited runtime");
    let superseded = session_projection_by_id_tx(&mut tx, &older.id)
        .await
        .expect("read superseded runtime")
        .expect("superseded runtime row");
    let active = if include_active {
        Some(
            session_start_runtime_tx(
                &mut tx,
                projectable_runtime_init(
                    &card_id,
                    label,
                    "active",
                    WorkerSessionState::Running,
                    30_000,
                ),
            )
            .await
            .expect("start active runtime"),
        )
    } else {
        None
    };
    tx.commit().await.expect("commit projectable tx");

    ProjectableHistory {
        card_id,
        superseded,
        exited,
        active,
    }
}

pub(super) async fn seed_deferred_projectable_placeholder(
    repo: &SqlxRepo,
    label: &'static str,
) -> (String, String) {
    let placeholder_id = format!("rt-projectable-placeholder-{label}-{}", new_id());
    let mut tx = repo.pool().begin().await.expect("begin placeholder tx");
    let card_id = create_card_in_tx(repo, &mut tx, label, "codex").await;
    session_prepare_deferred_spec_tx(
        &mut tx,
        &deferred_projectable_placeholder_init(&card_id, &placeholder_id, 40_000),
    )
    .await
    .expect("prepare deferred projectable placeholder");
    tx.commit().await.expect("commit placeholder tx");
    (card_id, placeholder_id)
}

pub(super) fn assert_ws_backed_projection(
    expected: &WorkerSessionProjection,
    actual: &WorkerSessionProjection,
) {
    assert_eq!(actual, expected);
    if matches!(&expected.kind, WorkerSessionKind::Terminal) {
        assert!(actual.agent_provider.is_none());
    }
}

pub(super) async fn worker_session_card_id(pool: &SqlitePool, id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT card_id FROM worker_sessions WHERE id = ?1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("worker session card_id")
}

pub(super) async fn assert_projectable_card_picks_active_winner(
    repo: &SqlxRepo,
    history: &ProjectableHistory,
    expected_winner_id: &str,
) {
    let read = runtime_get_projectable_for_card_from_pool(repo.pool(), &history.card_id)
        .await
        .expect("worker-session projectable read")
        .expect("projectable runtime from worker_sessions");
    assert_eq!(read.id, expected_winner_id);
    assert_ne!(read.id, history.superseded.id);
    assert_ne!(read.status, WorkerSessionState::Superseded);
}
