use super::*;
use calm_server::operation::terminal_execution_create::{CONSUMER, KIND};
use calm_server::operation::{OperationKey, OperationRepo, SqlxOperationRepo};

pub(super) async fn create(f: &Fixture, cwd: &str, program: &str) -> (StatusCode, String) {
    f.request(
        "POST",
        &format!("/api/tracks/{}/terminal-cards", f.track),
        Some(json!({
            "cwd": cwd, "program": program, "theme": RequestTheme::default_dark(),
        })),
    )
    .await
}
pub(super) async fn terminal(f: &Fixture, body: &str) -> Terminal {
    let card: Card = serde_json::from_str(body).unwrap();
    f.repo
        .terminal_get_by_card(card.id.as_str())
        .await
        .unwrap()
        .unwrap()
}
pub(super) async fn execution_id(f: &Fixture, term: &Terminal) -> ExecutionId {
    let id: String =
        sqlx::query_scalar("SELECT execution_id FROM workspace_executions WHERE alias = ?")
            .bind(sql::terminal_alias(&term.id))
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    id.try_into().unwrap()
}
#[tokio::test]
async fn typed_terminal_create_admits_and_drives() {
    let mut f = Fixture::new().await;
    wait(|| async { f.engine.available() }).await;
    f.state = f.state.clone().with_terminal_execution(true);
    let (status, body) = create(&f, "", "printf typed-ready; exec sleep 120").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let term = terminal(&f, &body).await;
    assert_eq!(term.cwd, f.root.to_str().unwrap());
    assert!(term.pid.is_none());
    let id = execution_id(&f, &term).await;
    wait(|| async {
        matches!(f.engine.output(&id, None).await,
            Ok(ExecutionReply::Observation { state: ExecutionState::Spawned, ref output, .. }) if output == b"typed-ready")
    }).await;
    wait(|| async { session(&f, &term).await.0 == "running" }).await;
    let (state, provider): (String, String) =
        sqlx::query_as("SELECT state, provider FROM worker_sessions WHERE terminal_run_id = ?")
            .bind(&term.id)
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    assert_eq!((state.as_str(), provider.as_str()), ("running", "terminal"));
    // Config affects new terminals only; an admitted one remains drivable after it is disabled.
    f.state = f.state.clone().with_terminal_execution(false);
    f.stop(&id).await;
    let term = f.repo.terminal_get(&term.id).await.unwrap().unwrap();
    assert!(term.signal_killed);
}

#[tokio::test]
async fn typed_create_unavailable_is_error_without_rows() {
    for absent in [true, false] {
        let mut f = Fixture::with_engine(false).await;
        if !absent {
            let engine = Arc::new(Engine::new(
                f.store.clone(),
                session_executor::SessionExecutor::new(
                    f._socket.path().join("absent.sock"),
                    CapabilityToken::fresh().unwrap(),
                ),
            ));
            assert!(!engine.available());
            f.state.operation_runtime.execution().bind(engine.clone());
            f.state.plugin.execution.bind(engine.clone());
            f.state.execution = Some(engine);
        }
        f.state = f.state.clone().with_terminal_execution(true);
        let (status, body) = create(&f, "", "true").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"],
            "execution_unavailable"
        );
        for table in [
            "cards",
            "worker_sessions",
            "terminals",
            "workspace_executions",
            "operations",
        ] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(f.repo.pool())
                .await
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
    }
}
#[tokio::test]
async fn config_off_creates_legacy_terminal() {
    use clap::Parser;
    assert!(
        !calm_server::config::Config::try_parse_from(["calm-server"])
            .unwrap()
            .terminal_execution
    );
    let f = Fixture::with_engine(false).await;
    let (status, body) = create(&f, "", "printf legacy-ready; exec sleep 120").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let term = terminal(&f, &body).await;
    assert!(
        !sql::alias_has_execution(f.repo.pool(), &sql::terminal_alias(&term.id))
            .await
            .unwrap()
    );
    let (status, body) = f
        .request("DELETE", &format!("/api/cards/{}", term.card_id), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}
#[tokio::test]
async fn prepare_refused_leaves_exited_deletable_card() {
    let mut f = Fixture::new().await;
    wait(|| async { f.engine.available() }).await;
    f.state = f.state.clone().with_terminal_execution(true);
    let missing = f.root.join("deleted");
    std::fs::create_dir(&missing).unwrap();
    std::fs::remove_dir(&missing).unwrap();
    let (status, body) = create(&f, missing.to_str().unwrap(), "true").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let term = terminal(&f, &body).await;
    let id = execution_id(&f, &term).await;
    f.engine.wait_settled(&id).await.unwrap();
    assert!(
        f.repo
            .terminal_get(&term.id)
            .await
            .unwrap()
            .unwrap()
            .signal_killed
    );
    let state: String =
        sqlx::query_scalar("SELECT state FROM worker_sessions WHERE terminal_run_id = ?")
            .bind(&term.id)
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    assert_eq!(state, "exited");
    // Refused Prepare is revoked without creating a child: the executor already exposes
    // Retired(Revoked). The lifecycle owner publishes exit and removes its own entry.
    assert!(matches!(
        f.engine.output(&id, None).await.unwrap(),
        ExecutionReply::Retired(tombstone)
            if tombstone.outcome == workspace_runtime::execution::TombstoneOutcome::Revoked
    ));
    wait(|| async { f.state.terminal_renderer.get(&term.id).is_none() }).await;

    let (status, body) = f
        .request("DELETE", &format!("/api/cards/{}", term.card_id), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    f.retired(&id).await;
}

// Install actual domain rows without driving an executor, to exercise the Store port's projection.
pub(super) async fn projection(f: &Fixture) -> (Terminal, ExecutionId) {
    let op_repo = SqlxOperationRepo::new(f.repo.db().clone());
    let op = op_repo
        .insert_operation(
            KIND,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: "projection".into(),
            },
            json!({}),
        )
        .await
        .unwrap();
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let (_, term) = sqlite::card_with_terminal_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        f.track.clone().into(),
        None,
        None,
        "true".into(),
        f.root.to_str().unwrap().into(),
        json!({}),
        CardRole::Worker,
        true,
        f.repo.card_role_cache(),
        RequestTheme::default_dark(),
        false,
    )
    .await
    .unwrap();
    let mut intent = exec::test_support::intent(f.root.clone());
    intent.metadata.owner = OperationRef(op);
    intent.metadata.consumer = CONSUMER.into();
    intent.metadata.alias = ProcessAlias(sql::terminal_alias(&term.id));
    intent.metadata.track = exec::TrackId(f.track.clone());
    f.store.insert(&mut tx, &intent).await.unwrap();
    tx.commit().await.unwrap();
    (term, intent.metadata.id)
}
async fn session(f: &Fixture, term: &Terminal) -> (String, i64, Option<i64>) {
    sqlx::query_as("SELECT state, updated_at_ms, completed_at_ms FROM worker_sessions WHERE terminal_run_id = ?")
        .bind(&term.id).fetch_one(f.repo.pool()).await.unwrap()
}
#[tokio::test]
async fn projection_spawned_sets_running_once() {
    let f = Fixture::new().await;
    let (term, id) = projection(&f).await;
    f.store.spawned(&id).await.unwrap();
    assert_eq!(session(&f, &term).await.0, "running");
    sqlx::query("UPDATE worker_sessions SET updated_at_ms = 1 WHERE terminal_run_id = ?")
        .bind(&term.id)
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    f.store.spawned(&id).await.unwrap();
    assert_eq!(
        session(&f, &term).await.1,
        1,
        "replay must not rewrite a running session"
    );
}
#[tokio::test]
async fn projection_settle_writes_exit_or_signal_killed() {
    let f = Fixture::new().await;
    for (state, exit) in [
        (ExecutionState::Drained { status: Some(7) }, Some(7)),
        (ExecutionState::Drained { status: None }, None),
        (
            ExecutionState::NoChild {
                reason: "test".into(),
            },
            None,
        ),
        (ExecutionState::Revoked, None),
    ] {
        for active in ["starting", "running", "idle", "turn_pending"] {
            let (term, id) = projection(&f).await;
            sqlx::query("UPDATE worker_sessions SET state = ? WHERE terminal_run_id = ?")
                .bind(active)
                .bind(&term.id)
                .execute(f.repo.db().writer_pool())
                .await
                .unwrap();
            f.store
                .settle(&id, &Settlement::Journal(state.clone()))
                .await
                .unwrap();
            let projected = f.repo.terminal_get(&term.id).await.unwrap().unwrap();
            assert_eq!(projected.exit_code, exit);
            assert_eq!(projected.signal_killed, exit.is_none());
            assert_eq!(session(&f, &term).await.0, "exited");
            assert!(session(&f, &term).await.2.is_some());
            assert!(f.store.load(&id).await.unwrap().unwrap().is_settled());
        }
    }
}
#[tokio::test]
async fn settled_worker_exited_not_overwritten_by_settle() {
    let f = Fixture::new().await;
    let (term, id) = projection(&f).await;
    sqlx::query("UPDATE worker_sessions SET state = 'exited', updated_at_ms = 1, completed_at_ms = 1 WHERE terminal_run_id = ?")
        .bind(&term.id).execute(f.repo.db().writer_pool()).await.unwrap();
    f.store.spawned(&id).await.unwrap();
    f.store
        .settle(&id, &Settlement::Journal(ExecutionState::Revoked))
        .await
        .unwrap();
    assert_eq!(session(&f, &term).await, ("exited".into(), 1, Some(1)));
    assert!(
        f.repo
            .terminal_get(&term.id)
            .await
            .unwrap()
            .unwrap()
            .signal_killed
    );
}

#[tokio::test]
async fn projection_failure_rolls_back_execution_settlement() {
    let f = Fixture::new().await;
    let (term, id) = projection(&f).await;
    sqlx::query(
        "CREATE TRIGGER reject_terminal_projection BEFORE UPDATE ON terminals \
        BEGIN SELECT RAISE(ABORT, 'projection refused'); END",
    )
    .execute(f.repo.db().writer_pool())
    .await
    .unwrap();
    assert!(
        f.store
            .settle(&id, &Settlement::Journal(ExecutionState::Revoked))
            .await
            .is_err()
    );
    assert!(!f.store.load(&id).await.unwrap().unwrap().is_settled());
    assert_eq!(session(&f, &term).await.0, "starting");
    assert!(
        !f.repo
            .terminal_get(&term.id)
            .await
            .unwrap()
            .unwrap()
            .signal_killed
    );
}

async fn keyed(f: &Fixture, key: &str, program: &str) -> (StatusCode, String) {
    f.request_keyed(
        "POST",
        &format!("/api/tracks/{}/terminal-cards", f.track),
        Some(json!({"program": program, "theme": RequestTheme::default_dark()})),
        Some(key),
    )
    .await
}
async fn counts(f: &Fixture) -> (i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT COUNT(*) FROM cards), (SELECT COUNT(*) FROM operations), (SELECT COUNT(*) FROM workspace_executions)")
        .fetch_one(f.repo.pool()).await.unwrap()
}

#[tokio::test]
async fn typed_keyed_replay_drives_without_second_admit() {
    use calm_server::operation::terminal_adapter::{
        TerminalCreateOperationPayload, TerminalCreateRequestPayload,
        normalize_terminal_create_request,
    };
    use calm_server::routes::idempotency_key::stable_payload_hash;
    let mut f = Fixture::new().await;
    wait(|| async { f.engine.available() }).await;
    f.state = f.state.clone().with_terminal_execution(true);
    let marker = f.root.join("launches");
    let program = format!("echo launch >> '{}'; exec sleep 120", marker.display());
    let request = normalize_terminal_create_request(TerminalCreateRequestPayload {
        track_id: f.track.clone(),
        title: None,
        sort: None,
        program: program.clone(),
        cwd: String::new(),
        env: serde_json::Value::Null,
        theme: RequestTheme::default_dark(),
    });
    let hash = stable_payload_hash(&json!({"actor": "user", "request": &request})).unwrap();
    let payload = serde_json::to_value(TerminalCreateOperationPayload {
        actor: calm_server::ids::ActorId::User,
        worker_session_id: Some(new_id()),
        planner_hooks: false,
        request,
    })
    .unwrap();
    // Commit through the production tx-only entry point, deliberately losing the caller's drive.
    let result = f
        .state
        .operation_runtime
        .commit_keyed(
            KIND,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some("lost-drive".into()),
                payload_hash: hash,
            },
            payload,
        )
        .await
        .unwrap();
    assert!(!marker.exists());
    assert_eq!(counts(&f).await, (1, 1, 1));
    let op = SqlxOperationRepo::new(f.repo.db().clone())
        .get_operation(&result.op_id)
        .await
        .unwrap()
        .unwrap();
    let durable_id = op.tx_output.unwrap().data["execution_id"]
        .as_str()
        .unwrap()
        .to_owned();
    for _ in 0..2 {
        let (status, body) = keyed(&f, "lost-drive", &program).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let term = terminal(&f, &body).await;
        assert_eq!(execution_id(&f, &term).await.as_str(), durable_id);
    }
    wait(|| async { marker.exists() }).await;
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "launch\n");
    assert_eq!(counts(&f).await, (1, 1, 1));
    f.stop(&durable_id.try_into().unwrap()).await;
}

#[tokio::test]
async fn typed_replay_while_unavailable_returns_card() {
    let mut f = Fixture::new().await;
    wait(|| async { f.engine.available() }).await;
    f.state = f.state.clone().with_terminal_execution(true);
    let (status, body) = keyed(&f, "outage", "exec sleep 120").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let term = terminal(&f, &body).await;
    let id = execution_id(&f, &term).await;
    wait(|| async {
        matches!(
            f.engine.output(&id, None).await,
            Ok(ExecutionReply::Observation {
                state: ExecutionState::Spawned,
                ..
            })
        )
    })
    .await;
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let ticket = f.engine.request(&mut tx, &id, Desired::Stop).await.unwrap();
    tx.commit().await.unwrap();
    f.engine.drive(ticket.committed());
    f.engine.wait_settled(&id).await.unwrap();
    // Remove the real command endpoint and force a failed command; the adapter then drops
    // its existing Watch connection and reports unavailable while reconnecting.
    std::fs::remove_file(f._socket.path().join("control.sock")).unwrap();
    assert!(f.engine.output(&id, None).await.is_err());
    wait(|| async { !f.engine.available() }).await;
    let (status, replay) = keyed(&f, "outage", "exec sleep 120").await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(terminal(&f, &replay).await.card_id, term.card_id);
    assert_eq!(counts(&f).await, (1, 1, 1));
    // The adapter's generic replay entry point also skips new-admission availability checks.
    let op = f
        .state
        .operation_runtime
        .find_by_kind_and_idempotency(KIND, "outage")
        .await
        .unwrap()
        .unwrap();
    f.state
        .operation_runtime
        .commit_keyed(
            KIND,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some("outage".into()),
                payload_hash: op.payload_hash,
            },
            op.payload,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn keyed_create_across_config_toggle_returns_same_card() {
    for typed in [true, false] {
        let mut f = Fixture::new().await;
        wait(|| async { f.engine.available() }).await;
        f.state = f.state.clone().with_terminal_execution(typed);
        let (status, body) = keyed(&f, "toggle", "exec sleep 120").await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let term = terminal(&f, &body).await;
        f.state = f.state.clone().with_terminal_execution(!typed);
        let (status, replay) = keyed(&f, "toggle", "exec sleep 120").await;
        assert_eq!(status, StatusCode::CREATED, "{replay}");
        assert_eq!(terminal(&f, &replay).await.card_id, term.card_id);
        let (status, body) = keyed(&f, "toggle", "different").await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let (status, body) = f
            .request_keyed(
                "POST",
                "/api/tracks/missing-track/terminal-cards",
                Some(json!({"program": "exec sleep 120", "theme": RequestTheme::default_dark()})),
                Some("toggle"),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "key conflict precedes new-request validation: {body}"
        );
        assert_eq!(counts(&f).await, (1, 1, i64::from(typed)));
        if typed {
            f.stop(&execution_id(&f, &term).await).await;
        } else {
            f.request("DELETE", &format!("/api/cards/{}", term.card_id), None)
                .await;
        }
    }
}

#[tokio::test]
async fn unregistered_projection_fails_and_rolls_back() {
    let f = Fixture::new().await;
    let (term, id) = projection(&f).await;
    let store = SqliteExecutionStore::new(f.repo.db().clone());
    let before = session(&f, &term).await;
    let spawned = store.spawned(&id).await;
    let settled = store
        .settle(&id, &Settlement::Journal(ExecutionState::Revoked))
        .await;
    for result in [spawned, settled] {
        assert_eq!(
            result.unwrap_err().0,
            format!("missing execution projection for consumer: {CONSUMER}")
        );
    }
    assert!(!store.load(&id).await.unwrap().unwrap().is_settled());
    assert_eq!(session(&f, &term).await, before);
    let after = f.repo.terminal_get(&term.id).await.unwrap().unwrap();
    assert_eq!(after.exit_code, term.exit_code);
    assert_eq!(after.signal_killed, term.signal_killed);
}

#[tokio::test]
async fn projection_survives_owner_operation_reclaim() {
    let f = Fixture::new().await;
    let (term, id) = projection(&f).await;
    let owner: String = sqlx::query_scalar(
        "SELECT json_extract(intent_json, '$.owner') FROM workspace_executions WHERE execution_id = ?",
    ).bind(id.as_str()).fetch_one(f.repo.pool()).await.unwrap();
    let deleted = sqlx::query("DELETE FROM operations WHERE id = ? AND idempotency_key IS NULL")
        .bind(&owner)
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    assert_eq!(deleted.rows_affected(), 1);
    assert!(f.repo.terminal_get(&term.id).await.unwrap().is_some());
    assert_eq!(session(&f, &term).await.0, "starting");
    f.store.spawned(&id).await.unwrap();
    assert_eq!(session(&f, &term).await.0, "running");
    f.store
        .settle(
            &id,
            &Settlement::Journal(ExecutionState::Drained { status: Some(7) }),
        )
        .await
        .unwrap();
    assert!(f.store.load(&id).await.unwrap().unwrap().is_settled());
    let after = f.repo.terminal_get(&term.id).await.unwrap().unwrap();
    assert_eq!(after.exit_code, Some(7));
    assert!(!after.signal_killed);
    assert_eq!(session(&f, &term).await.0, "exited");
    assert!(session(&f, &term).await.2.is_some());
}
