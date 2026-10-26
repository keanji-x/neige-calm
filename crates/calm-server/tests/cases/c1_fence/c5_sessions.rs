//! C5c exercises the production close tick, with typed-terminal history and no renderer.
use super::*;
use calm_server::db::RepoOutOfDomain;
use calm_server::model::{NewCard, NewTerminal, new_id, now_ms};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};

async fn session(
    b: &Boot,
    track: &str,
    path: &Path,
    provider: Option<AgentProvider>,
    engine: bool,
) -> (String, String) {
    let card = b
        .repo
        .card_create(NewCard {
            track_id: track.to_string().into(),
            title: None,
            kind: if provider.is_some() {
                "agent"
            } else {
                "terminal"
            }
            .into(),
            sort: None,
            payload: json!({"terminal_presentation":{"source":"unrecorded","in_workbench":false}}),
        })
        .await
        .unwrap();
    let terminal = if provider.is_none() {
        Some(
            b.repo
                .terminal_create(NewTerminal {
                    card_id: card.id.clone(),
                    program: "/bin/true".into(),
                    cwd: path.to_string_lossy().into_owned(),
                    env: json!({}),
                    theme: calm_server::routes::theme::RequestTheme::default_dark(),
                })
                .await
                .unwrap(),
        )
    } else {
        None
    };
    let id = new_id();
    let mut tx = b.repo.pool().begin().await.unwrap();
    calm_server::db::sqlite::session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: id.clone(),
            card_id: card.id.to_string(),
            kind: if provider.is_some() {
                WorkerSessionKind::SharedPlanner
            } else {
                WorkerSessionKind::Terminal
            },
            agent_provider: provider,
            status: WorkerSessionState::Running,
            terminal_run_id: terminal.as_ref().map(|t| t.id.clone()),
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
    let execution = new_id();
    let alias = match terminal {
        Some(t) => calm_truth::db::sqlite::workspace_execution::terminal_alias(&t.id),
        None => format!("planner:{id}:{execution}"),
    };
    if engine {
        sqlx::query(
            "INSERT INTO workspace_executions \
        (execution_id,track_id,alias,intent_json,secrets_json,desired) VALUES (?,?,?,?,?,'run')",
        )
        .bind(&execution)
        .bind(track)
        .bind(alias)
        .bind(json!({"source":{"root":path}}).to_string())
        .bind("{}")
        .execute(b.repo.pool())
        .await
        .unwrap();
    }
    (id, execution)
}

async fn settle(b: &Boot, execution: &str) {
    // Retired history must still identify a typed terminal.
    sqlx::query(
        "UPDATE workspace_executions SET settled_at_ms=1,settlement_json='{}',\
        desired='retire',secrets_json=NULL WHERE execution_id=?",
    )
    .bind(execution)
    .execute(b.repo.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn c5_session_stale_running_settled_execution_allows_clean() {
    let (b, track, path) = fixture().await;
    let (_, execution) = session(&b, &track, &path, None, true).await;
    settle(&b, &execution).await;
    let (count, _) = clean(b.clone()).await.unwrap();
    assert_eq!(count, 1);
    assert!(!path.join("target/ignored").exists());
}

#[tokio::test]
async fn c5_session_no_silent_miss_exited_unsettled_without_renderer_defers() {
    for state in ["exited", "idle"] {
        let (b, track, path) = fixture().await;
        let (id, execution) = session(&b, &track, &path, None, true).await;
        sqlx::query("UPDATE worker_sessions SET state=? WHERE id=?")
            .bind(state)
            .bind(&id)
            .execute(b.repo.pool())
            .await
            .unwrap();
        let (count, history) = clean(b.clone()).await.unwrap();
        assert_eq!(count, 0);
        assert!(history.is_empty());
        assert!(path.join("target/ignored").is_file());
        settle(&b, &execution).await;
        assert_eq!(clean(b.clone()).await.unwrap().0, 1);
    }
}

#[tokio::test]
async fn c5_session_planner_stale_proxy_with_settled_execution_still_defers() {
    for provider in [AgentProvider::Claude, AgentProvider::OpenCode] {
        let (b, track, path) = fixture().await;
        let (id, execution) = session(&b, &track, &path, Some(provider), true).await;
        settle(&b, &execution).await;
        for state in ["starting", "running", "turn_pending"] {
            sqlx::query("UPDATE worker_sessions SET state=? WHERE id=?")
                .bind(state)
                .bind(&id)
                .execute(b.repo.pool())
                .await
                .unwrap();
            let (count, history) = clean(b.clone()).await.unwrap();
            assert_eq!(count, 0);
            assert!(history.is_empty());
            assert!(path.join("target/ignored").is_file());
        }
    }
}

#[tokio::test]
async fn c5_session_legacy_without_execution_still_defers() {
    for provider in [
        None,
        Some(AgentProvider::Claude),
        Some(AgentProvider::OpenCode),
        Some(AgentProvider::Codex),
    ] {
        let (b, track, path) = fixture().await;
        // Another carrier's settled history must never classify this Legacy row.
        let (_, execution) = session(&b, &track, &path, None, true).await;
        settle(&b, &execution).await;
        let (id, _) = session(&b, &track, &path, provider, false).await;
        for state in ["starting", "running", "turn_pending"] {
            sqlx::query("UPDATE worker_sessions SET state=? WHERE id=?")
                .bind(state)
                .bind(&id)
                .execute(b.repo.pool())
                .await
                .unwrap();
            let (count, history) = clean(b.clone()).await.unwrap();
            assert_eq!(count, 0);
            assert!(history.is_empty());
            assert!(path.join("target/ignored").is_file());
        }
        sqlx::query("UPDATE worker_sessions SET state='exited' WHERE id=?")
            .bind(&id)
            .execute(b.repo.pool())
            .await
            .unwrap();
        assert_eq!(clean(b.clone()).await.unwrap().0, 1);
    }
}
