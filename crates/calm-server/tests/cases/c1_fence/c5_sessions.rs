//! C5c/C5d.2 exercise the production close tick, with typed-terminal history and no renderer.
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
    session_with_id(b, track, path, provider, engine, &new_id()).await
}

async fn session_with_id(
    b: &Boot,
    track: &str,
    path: &Path,
    provider: Option<AgentProvider>,
    engine: bool,
    id: &str,
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
    let id = id.to_string();
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
        None => format!(
            "{}:{id}:{execution}",
            calm_truth::db::sqlite::workspace_execution::PLANNER_ALIAS_PREFIX
        ),
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
    // Retired history must still identify a typed terminal or Planner session.
    sqlx::query(
        "UPDATE workspace_executions SET settled_at_ms=1,settlement_json=?,\
        desired='retire',secrets_json=NULL WHERE execution_id=?",
    )
    .bind(
        serde_json::to_string(&workspace_runtime::execution::Settlement::Journal(
            workspace_runtime::execution::ExecutionState::Drained { status: Some(0) },
        ))
        .unwrap(),
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

async fn planner_stale_clean(provider: AgentProvider) {
    for state in ["starting", "running", "turn_pending"] {
        let (b, track, path) = fixture().await;
        calm_server::test_seams::boot_planner_upgrade_for_test(&b.state)
            .await
            .unwrap();
        let (id, execution) = session(&b, &track, &path, Some(provider.clone()), true).await;
        settle(&b, &execution).await;
        sqlx::query("UPDATE worker_sessions SET state=? WHERE id=?")
            .bind(state)
            .bind(&id)
            .execute(b.repo.pool())
            .await
            .unwrap();
        assert_eq!(clean(b.clone()).await.unwrap().0, 1, "{provider:?} {state}");
        assert!(!path.join("target/ignored").exists());
    }
}

#[tokio::test]
async fn c5d2_claude_stale_settled_cleans() {
    planner_stale_clean(AgentProvider::Claude).await;
}

#[tokio::test]
async fn c5d2_acp_stale_settled_defers() {
    // ACP discovery still uses a track cwd without execution occupancy. Settled turn
    // history therefore cannot exempt its active session proxy.
    for provider in [AgentProvider::OpenCode, AgentProvider::Grok] {
        for state in ["starting", "running", "turn_pending"] {
            let (b, track, path) = fixture().await;
            calm_server::test_seams::boot_planner_upgrade_for_test(&b.state)
                .await
                .unwrap();
            let (id, execution) = session(&b, &track, &path, Some(provider.clone()), true).await;
            settle(&b, &execution).await;
            sqlx::query("UPDATE worker_sessions SET state=? WHERE id=?")
                .bind(state)
                .bind(&id)
                .execute(b.repo.pool())
                .await
                .unwrap();
            let (count, history) = clean(b.clone()).await.unwrap();
            assert_eq!(count, 0, "{provider:?} {state}");
            assert!(history.is_empty());
            assert!(path.join("target/ignored").is_file());
            sqlx::query("UPDATE worker_sessions SET state='exited' WHERE id=?")
                .bind(id)
                .execute(b.repo.pool())
                .await
                .unwrap();
            assert_eq!(clean(b.clone()).await.unwrap().0, 1);
        }
    }
}

#[tokio::test]
async fn c5d2_planner_unsettled_defers() {
    for provider in [AgentProvider::Claude, AgentProvider::OpenCode] {
        let (b, track, path) = fixture().await;
        calm_server::test_seams::boot_planner_upgrade_for_test(&b.state)
            .await
            .unwrap();
        session(&b, &track, &path, Some(provider), true).await;
        let (count, history) = clean(b.clone()).await.unwrap();
        assert_eq!(count, 0);
        assert!(history.is_empty());
        assert!(path.join("target/ignored").is_file());
    }
}

#[tokio::test]
async fn c5d2_planner_unproven_upgrade_with_history_defers() {
    for provider in [AgentProvider::Claude, AgentProvider::OpenCode] {
        let (b, track, path) = fixture().await;
        // No boot proof: registered history does not establish pre-C4 absence.
        let (_, execution) = session(&b, &track, &path, Some(provider), true).await;
        settle(&b, &execution).await;
        let (count, history) = clean(b.clone()).await.unwrap();
        assert_eq!(count, 0);
        assert!(history.is_empty());
        assert!(path.join("target/ignored").is_file());
    }
}

#[tokio::test]
async fn c5d2_planner_failed_upgrade_sweep_with_history_defers() {
    for provider in [AgentProvider::Claude, AgentProvider::OpenCode] {
        let (b, track, path) = fixture().await;
        let (id, execution) = session(&b, &track, &path, Some(provider.clone()), true).await;
        settle(&b, &execution).await;
        let unproven = [id.clone()];
        let (claude, acp) = if provider == AgentProvider::Claude {
            (&unproven[..], &[][..])
        } else {
            (&[][..], &unproven[..])
        };
        calm_server::test_seams::planner_upgrade_unproven_for_test(&b.state, claude, acp);
        let (count, history) = clean(b.clone()).await.unwrap();
        assert_eq!(count, 0);
        assert!(history.is_empty());
        assert!(path.join("target/ignored").is_file());
        // A failed sweep retains unproven Claude proxies; ACP always retains its proxy.
        sqlx::query("UPDATE worker_sessions SET state='exited' WHERE id=?")
            .bind(id)
            .execute(b.repo.pool())
            .await
            .unwrap();
        let (_, execution) = session(&b, &track, &path, Some(provider.clone()), true).await;
        settle(&b, &execution).await;
        assert_eq!(
            clean(b.clone()).await.unwrap().0,
            if provider == AgentProvider::Claude {
                1
            } else {
                0
            }
        );
    }
}

#[tokio::test]
async fn c5d2_alias_session_boundary_defers() {
    for (id, other) in [
        ("s", "s1"),
        ("s", "s_x"),
        ("s_x", "sax"),
        ("s", "s:x"),
        ("s:x", "s:x:y"),
        ("s;x", "s;x:y"),
    ] {
        let (b, track, path) = fixture().await;
        calm_server::test_seams::boot_planner_upgrade_for_test(&b.state)
            .await
            .unwrap();
        let (_, execution) =
            session_with_id(&b, &track, &path, Some(AgentProvider::Claude), true, other).await;
        settle(&b, &execution).await;
        let (session, _) =
            session_with_id(&b, &track, &path, Some(AgentProvider::Claude), false, id).await;
        for provider in ["claude", "codex"] {
            sqlx::query("UPDATE worker_sessions SET provider=? WHERE id=?")
                .bind(provider)
                .bind(&session)
                .execute(b.repo.pool())
                .await
                .unwrap();
            let (count, history) = clean(b.clone()).await.unwrap();
            assert_eq!(count, 0, "{provider}: {id} must not match {other}");
            assert!(history.is_empty());
            assert!(path.join("target/ignored").is_file());
        }
    }
}

#[tokio::test]
async fn c5d2_no_history_planner_codex_and_legacy_defer() {
    for provider in [
        None,
        Some(AgentProvider::Claude),
        Some(AgentProvider::OpenCode),
        Some(AgentProvider::Codex),
    ] {
        let (b, track, path) = fixture().await;
        calm_server::test_seams::boot_planner_upgrade_for_test(&b.state)
            .await
            .unwrap();
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

#[tokio::test]
async fn c5d2_delimited_session_own_history_cleans() {
    for id in ["s:x", "s:x:y", "s;x", "s;x:y"] {
        let (b, track, path) = fixture().await;
        calm_server::test_seams::boot_planner_upgrade_for_test(&b.state)
            .await
            .unwrap();
        let (_, execution) =
            session_with_id(&b, &track, &path, Some(AgentProvider::Claude), true, id).await;
        settle(&b, &execution).await;
        assert_eq!(clean(b.clone()).await.unwrap().0, 1, "{id}");
        assert!(!path.join("target/ignored").exists());
    }
}

#[tokio::test]
async fn c5d2_undeclared_provider_with_drained_history_defers() {
    let (b, track, path) = fixture().await;
    calm_server::test_seams::boot_planner_upgrade_for_test(&b.state)
        .await
        .unwrap();
    let (_, execution) = session(&b, &track, &path, Some(AgentProvider::Codex), true).await;
    settle(&b, &execution).await;
    let (count, history) = clean(b.clone()).await.unwrap();
    assert_eq!(count, 0);
    assert!(history.is_empty());
    assert!(path.join("target/ignored").is_file());
}

#[tokio::test]
async fn c5d2_one_weak_settlement_among_drained_history_defers() {
    use workspace_runtime::execution::*;
    let interrupted =
        serde_json::to_string(&Settlement::Journal(ExecutionState::InterruptedQuiescent {
            reason: ExecutionInterruption::ExecutorInterrupted,
            proof: InterruptedQuiescenceProof {
                host_boot_id: "test-boot".into(),
                helper_process: NativeProcessIdentity::LinuxHelper(LinuxProcessIdentity {
                    pid: 12345,
                    start_time_ticks: 1,
                    nonce: CapabilityToken::fresh().unwrap(),
                }),
                mechanism: ScopeMechanismIdentity::LinuxHelper(LinuxHelperIdentity {
                    executable_device: 1,
                    executable_inode: 2,
                    protocol_version: 1,
                }),
                scope: InterruptedScope::LinuxOriginalGroupAndSessionEmpty,
            },
        }))
        .unwrap();
    let drained = serde_json::to_string(&Settlement::Journal(ExecutionState::Drained {
        status: Some(0),
    }))
    .unwrap();
    // Settlements are immutable: use independent histories for weak, unknown,
    // and all-Drained proof rather than upgrading an existing settlement.
    for (proof, expected) in [(interrupted, 0), ("{}".into(), 0), (drained, 1)] {
        let (b, track, path) = fixture().await;
        calm_server::test_seams::boot_planner_upgrade_for_test(&b.state)
            .await
            .unwrap();
        let (_, execution) = session(&b, &track, &path, Some(AgentProvider::Claude), true).await;
        settle(&b, &execution).await;
        // A second execution for the SAME session: one strong settlement cannot
        // hide a weaker historical proof, including retired history.
        let other = new_id();
        sqlx::query(
            "INSERT INTO workspace_executions \
             (execution_id,track_id,alias,intent_json,secrets_json,desired,settled_at_ms,settlement_json) \
             SELECT ?,track_id,replace(alias,execution_id,?),intent_json,NULL,'retire',1,? \
             FROM workspace_executions WHERE execution_id=?",
        )
            .bind(&other).bind(&other).bind(proof).bind(&execution)
            .execute(b.repo.pool()).await.unwrap();
        let (count, history) = clean(b.clone()).await.unwrap();
        assert_eq!(count, expected);
        if expected == 0 {
            assert!(history.is_empty());
        }
        assert_eq!(path.join("target/ignored").is_file(), expected == 0);
    }
}
