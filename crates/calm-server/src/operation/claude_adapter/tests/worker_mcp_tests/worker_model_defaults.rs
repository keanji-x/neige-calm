//! Exercise the published operation's real launch boundary, including frozen replay output.
use super::*;
use crate::db::{RepoOutOfDomain, RepoRead};
use crate::operation::Phase;
use crate::worker_model_defaults::tests::Warnings;
use calm_types::worker_model::WorkerModelProvider;

struct Launch {
    h: ClaudeWorkerHarness,
    adapter: ClaudeWorkerAdapter,
    output: TxOutput,
    op: Operation,
    spawned: Spawned,
    _mcp_dir: tempfile::TempDir,
}

async fn launch(explicit: bool) -> Launch {
    let h = claude_worker_harness().await;
    if explicit {
        h.repo
            .settings_upsert(
                "worker_model_defaults_claude",
                r#"{"model":"model-a","effort":"high"}"#,
            )
            .await
            .unwrap();
    }
    h.repo.db().writer().execute(sqlx::query(
        "INSERT INTO tasks (id,track_id,key,kind,goal,context_json,depends_on_json,status,created_at_ms,updated_at_ms) \
         VALUES (?1,?2,'launch','claude','test','null','[]','dispatched',1,1)"
    ).bind(format!("{}:launch", h.track_id)).bind(&h.track_id)).await.unwrap();
    let output = prepare_claude_worker_as_scheduled(&h, "launch").await;
    let op_repo = SqlxOperationRepo::new(h.repo.db().clone());
    let op = op_repo.claim_drive_batch(1).await.unwrap().remove(0);
    assert_eq!(op.phase, Phase::TxCommitted);
    op_repo
        .set_phase_and_tx_output(&op, Phase::SpawnStarted, &output)
        .await
        .unwrap()
        .expect("owned spawn checkpoint");
    let op = op_repo.claim_drive_batch(1).await.unwrap().remove(0);
    assert_eq!(op.phase, Phase::SpawnStarted);
    assert_eq!(
        op.tx_output.as_ref().unwrap().data["terminal_launch"]["state"],
        "not_requested"
    );
    let mcp_dir = tempfile::tempdir().unwrap();
    let (spawned, hook) = capturing_hook();
    let adapter = ClaudeWorkerAdapter::new_with_spawn_hook(
        h.repo.clone(),
        Arc::new(CodexClient::new_stub()),
        Some(test_mcp_server(mcp_dir.path())),
        CardRoleCache::new(),
        TrackAreaCache::new(),
        h.workspace.path().to_path_buf(),
        hook,
    );
    Launch {
        h,
        adapter,
        output,
        op,
        spawned,
        _mcp_dir: mcp_dir,
    }
}

async fn set_snapshot(l: &Launch, snapshot: Option<Value>) {
    // Only a temporary DB is touched: SQL represents corruption/absence that client writes refuse.
    let card = l.output.data["card_id"].as_str().unwrap();
    let mut payload = l.h.repo.card_get(card).await.unwrap().unwrap().payload;
    match snapshot {
        Some(value) => {
            payload["worker_model_selection"] = value;
        }
        None => {
            payload
                .as_object_mut()
                .unwrap()
                .remove("worker_model_selection");
        }
    }
    l.h.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE cards SET payload=? WHERE id=?")
                .bind(payload.to_string())
                .bind(card),
        )
        .await
        .unwrap();
}

async fn assert_no_launch_effects(l: &Launch) {
    let card_id = l.output.output_string("card_id", "test").unwrap();
    assert!(l.spawned.lock().await.is_empty());
    let settings = Path::new(l.output.data["settings_path"].as_str().unwrap());
    assert!(
        !settings.parent().unwrap().exists(),
        "no settings or MCP file may be created"
    );
    let tokens: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM card_mcp_tokens WHERE card_id=?")
        .bind(&card_id)
        .fetch_one(l.h.repo.pool())
        .await
        .unwrap();
    assert_eq!(tokens, 0);
    let session =
        l.h.repo
            .session_projection_active_for_card(&card_id)
            .await
            .unwrap()
            .unwrap();
    let hash: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
            .bind(&session.id)
            .fetch_one(l.h.repo.pool())
            .await
            .unwrap();
    assert!(hash.is_none());
}

async fn refuses_snapshot(snapshot: Value, replay: bool) {
    let l = launch(true).await;
    assert_no_launch_effects(&l).await;
    set_snapshot(&l, Some(snapshot)).await;
    // The replay uses the frozen output reloaded from the real operation row, not publication.
    let output = if replay {
        l.op.tx_output.as_ref().unwrap()
    } else {
        &l.output
    };
    let result = l
        .adapter
        .spawn_side_effect(output, &l.op, &spawn_ctx(&l.h))
        .await;
    assert!(
        matches!(result, Err(CalmError::Conflict(_))),
        "{:?}",
        result.err()
    );
    assert_no_launch_effects(&l).await;
    release(&l.h, l.output.data["card_id"].as_str().unwrap()).await;
}

#[tokio::test]
async fn worker_model_defaults_claude_first_launch_refuses_null_snapshot() {
    refuses_snapshot(Value::Null, false).await;
}

#[tokio::test]
async fn worker_model_defaults_claude_replay_refuses_null_snapshot() {
    refuses_snapshot(Value::Null, true).await;
}

#[tokio::test]
async fn worker_model_defaults_claude_first_launch_refuses_malformed_snapshot() {
    refuses_snapshot(
        json!({"provider":"claude","selection":{"kind":"explicit","value":{"model":"a"}}}),
        false,
    )
    .await;
}

#[tokio::test]
async fn worker_model_defaults_claude_replay_refuses_malformed_snapshot() {
    refuses_snapshot(
        json!({"provider":"claude","selection":{"kind":"explicit","value":{"model":"a"}}}),
        true,
    )
    .await;
}

#[tokio::test]
async fn worker_model_defaults_claude_first_launch_refuses_provider_mismatch() {
    refuses_snapshot(
        json!({"provider":"codex","selection":{"kind":"inherit"}}),
        false,
    )
    .await;
}

#[tokio::test]
async fn worker_model_defaults_claude_replay_refuses_provider_mismatch() {
    refuses_snapshot(
        json!({"provider":"codex","selection":{"kind":"inherit"}}),
        true,
    )
    .await;
}

#[tokio::test]
async fn worker_model_defaults_claude_explicit_and_inherit_keep_frozen_command_without_warning() {
    for explicit in [true, false] {
        let l = launch(explicit).await;
        l.h.repo
            .settings_upsert(
                "worker_model_defaults_claude",
                r#"{"model":"model-b","effort":"low"}"#,
            )
            .await
            .unwrap();
        let warnings = Warnings::default();
        warnings
            .run(l.adapter.spawn_side_effect(
                l.op.tx_output.as_ref().unwrap(),
                &l.op,
                &spawn_ctx(&l.h),
            ))
            .await
            .unwrap();
        let spawned = l.spawned.lock().await;
        assert_eq!(spawned.len(), 1);
        assert_eq!(
            spawned[0].0,
            l.output.data["command_line"].as_str().unwrap()
        );
        let argv = shell_argv(&spawned[0].0);
        if explicit {
            assert!(argv.contains(&"--model=model-a".into()), "{argv:?}");
            assert!(argv.contains(&"--effort=high".into()), "{argv:?}");
        } else {
            assert!(
                !argv
                    .iter()
                    .any(|arg| arg.starts_with("--model=") || arg.starts_with("--effort=")),
                "{argv:?}"
            );
        }
        let card = l.output.data["card_id"].as_str().unwrap();
        warnings.assert_no_legacy(card);
        release(&l.h, card).await;
    }
}

#[tokio::test]
async fn worker_model_defaults_claude_legacy_unstarted_operation_warns_without_new_defaults() {
    let mut l = launch(false).await;
    set_snapshot(&l, None).await;
    // Base produced no snapshot in either persisted carrier. Preserve its original command.
    l.output.result["payload"]
        .as_object_mut()
        .unwrap()
        .remove("worker_model_selection");
    let repo = SqlxOperationRepo::new(l.h.repo.db().clone());
    repo.set_phase_and_tx_output(&l.op, Phase::SpawnStarted, &l.output)
        .await
        .unwrap()
        .unwrap();
    l.op = repo.claim_drive_batch(1).await.unwrap().remove(0);
    l.h.repo
        .settings_upsert(
            "worker_model_defaults_claude",
            r#"{"model":"model-b","effort":"low"}"#,
        )
        .await
        .unwrap();
    let card = l.output.data["card_id"].as_str().unwrap();
    for _ in 0..2 {
        let warnings = Warnings::default();
        warnings
            .run(l.adapter.spawn_side_effect(
                l.op.tx_output.as_ref().unwrap(),
                &l.op,
                &spawn_ctx(&l.h),
            ))
            .await
            .unwrap();
        warnings.assert_legacy(card, WorkerModelProvider::Claude);
    }
    let spawned = l.spawned.lock().await;
    assert_eq!(spawned.len(), 2);
    for (command, _) in spawned.iter() {
        assert_eq!(command, l.output.data["command_line"].as_str().unwrap());
        let argv = shell_argv(command);
        assert!(
            !argv
                .iter()
                .any(|arg| arg.starts_with("--model=") || arg.starts_with("--effort=")),
            "{argv:?}"
        );
    }
    assert!(
        l.h.repo
            .card_get(card)
            .await
            .unwrap()
            .unwrap()
            .payload
            .get("worker_model_selection")
            .is_none()
    );
    release(&l.h, card).await;
}

#[tokio::test]
async fn worker_model_defaults_claude_bad_snapshot_does_not_change_live_or_exited_noop() {
    for live in [true, false] {
        let l = launch(false).await;
        set_snapshot(&l, Some(Value::Null)).await;
        let terminal = l.output.data["terminal_id"].as_str().unwrap();
        if live {
            l.h.repo
                .terminal_set_pid(terminal, Some(4242))
                .await
                .unwrap();
        } else {
            l.h.repo
                .terminal_set_exit(terminal, Some(0), false)
                .await
                .unwrap();
        }
        let warnings = Warnings::default();
        let outcome = warnings
            .run(l.adapter.spawn_side_effect(
                l.op.tx_output.as_ref().unwrap(),
                &l.op,
                &spawn_ctx(&l.h),
            ))
            .await
            .unwrap();
        assert!(matches!(outcome, SpawnOutcome::Ready(SpawnHandle::NoOp)));
        assert_no_launch_effects(&l).await;
        let card = l.output.data["card_id"].as_str().unwrap();
        warnings.assert_no_legacy(card);
        release(&l.h, card).await;
    }
}

#[tokio::test]
async fn worker_model_defaults_claude_legacy_restart_warns_without_new_defaults() {
    let r = prepared_worker_restart_snapshot(PreviousEnd::Exited, false, true).await;
    let warnings = Warnings::default();
    warnings
        .run(
            r.restart
                .spawn_side_effect(&r.restart_output, &r.restart_op, &spawn_ctx(&r.harness)),
        )
        .await
        .unwrap();
    warnings.assert_legacy(&r.card_id, WorkerModelProvider::Claude);
    let spawned = r.spawned.lock().await;
    assert_eq!(spawned.len(), 1);
    let argv = shell_argv(&spawned[0].0);
    assert!(argv.iter().any(|arg| arg.starts_with("--resume=")));
    assert!(
        !argv
            .iter()
            .any(|arg| arg.starts_with("--model=") || arg.starts_with("--effort=")),
        "{argv:?}"
    );
    assert!(
        r.harness
            .repo
            .card_get(&r.card_id)
            .await
            .unwrap()
            .unwrap()
            .payload
            .get("worker_model_selection")
            .is_none()
    );
    release(&r.harness, &r.card_id).await;
}

async fn restart_refuses_snapshot(snapshot: Value) {
    let r = prepared_worker_restart(PreviousEnd::Exited).await;
    r.harness.repo.db().writer().execute(sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.worker_model_selection',json(?)) WHERE id=?"
    ).bind(snapshot.to_string()).bind(&r.card_id)).await.unwrap();
    let settings = Path::new(&r.settings_path);
    assert!(!settings.parent().unwrap().exists());
    let terminal = r.restart_output.data["terminal_id"].as_str().unwrap();
    let before = r
        .harness
        .repo
        .terminal_get(terminal)
        .await
        .unwrap()
        .unwrap();
    // The predecessor is already exited: its handshake is refused before spawn.
    // Compare persisted credentials so this checks rotation without reviving it.
    let token_query = "SELECT c.hashed_token, s.mcp_token_hash FROM card_mcp_tokens c \
        JOIN worker_sessions s ON s.card_id=c.card_id WHERE c.card_id=? AND s.id=?";
    let before_tokens: (String, Option<String>) = sqlx::query_as(token_query)
        .bind(&r.card_id)
        .bind(&r.restart_session)
        .fetch_one(r.harness.repo.pool())
        .await
        .unwrap();
    let result = r
        .restart
        .spawn_side_effect(&r.restart_output, &r.restart_op, &spawn_ctx(&r.harness))
        .await;
    assert!(
        matches!(result, Err(CalmError::Conflict(_))),
        "{:?}",
        result.err()
    );
    assert!(r.spawned.lock().await.is_empty());
    assert!(!settings.parent().unwrap().exists());
    let after_tokens: (String, Option<String>) = sqlx::query_as(token_query)
        .bind(&r.card_id)
        .bind(&r.restart_session)
        .fetch_one(r.harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(after_tokens, before_tokens, "no token rotation on refusal");
    let after = r
        .harness
        .repo
        .terminal_get(terminal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.exit_code, before.exit_code);
    assert_eq!(after.signal_killed, before.signal_killed);
    release(&r.harness, &r.card_id).await;
}

#[tokio::test]
async fn worker_model_defaults_claude_restart_replay_refuses_null_snapshot() {
    restart_refuses_snapshot(Value::Null).await;
}

#[tokio::test]
async fn worker_model_defaults_claude_restart_replay_refuses_malformed_snapshot() {
    restart_refuses_snapshot(
        json!({"provider":"claude","selection":{"kind":"explicit","value":{"model":"a"}}}),
    )
    .await;
}

#[tokio::test]
async fn worker_model_defaults_claude_restart_replay_refuses_provider_mismatch() {
    restart_refuses_snapshot(json!({"provider":"codex","selection":{"kind":"inherit"}})).await;
}
