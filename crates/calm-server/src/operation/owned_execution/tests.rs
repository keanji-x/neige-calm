use calm_truth::db::sqlite::{SqlxRepo, begin_immediate_tx, gate_execution as sql};
use serde_json::{Value, json};

use crate::operation::{OperationKey, OperationRepo, SqlxOperationRepo, TxOutput, new_id};

#[tokio::test]
async fn c3_keyed_record_cas_never_touches_another_key() {
    let db = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let repo = SqlxOperationRepo::new(db.db().clone());
    let id = repo
        .insert_operation(
            "keyed-owner",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: "hash".into(),
            },
            json!({}),
        )
        .await
        .unwrap();
    let mut output = TxOutput::new("unknown", None, json!({}));
    output.data = json!({"other":"preserved"});
    sqlx::query("UPDATE operations SET phase='spawn_started',tx_output_json=?2 WHERE id=?1")
        .bind(&id)
        .bind(serde_json::to_string(&output).unwrap())
        .execute(db.pool())
        .await
        .unwrap();
    let gate = json!({"version":1,"execution_id":"gate","release":"Held","decision":"Observing"});
    let terminal =
        json!({"version":1,"execution_id":"terminal","release":"Held","decision":"Observing"});
    let updated = json!({"version":1,"execution_id":"gate","release":{"Released":{"authorized_at_ms":42}},"decision":"Observing"});
    let mut tx = begin_immediate_tx(db.db()).await.unwrap();
    sql::insert(&mut tx, &id, "gate_execution", &gate.to_string())
        .await
        .unwrap();
    sql::insert(&mut tx, &id, "terminal_launch", &terminal.to_string())
        .await
        .unwrap();
    assert!(
        sql::insert(&mut tx, &id, "gate_execution", &terminal.to_string())
            .await
            .is_err()
    );
    // A snapshot from another key cannot authorize a write or satisfy a completion fence.
    assert!(
        sql::compare_exchange(
            &mut tx,
            &id,
            "terminal_launch",
            &gate.to_string(),
            &updated.to_string()
        )
        .await
        .is_err()
    );
    assert!(
        sql::require_record(&mut tx, &id, "terminal_launch", &gate.to_string())
            .await
            .is_err()
    );
    sql::compare_exchange(
        &mut tx,
        &id,
        "gate_execution",
        &gate.to_string(),
        &updated.to_string(),
    )
    .await
    .unwrap();
    assert!(
        sql::compare_exchange(
            &mut tx,
            &id,
            "gate_execution",
            &gate.to_string(),
            &terminal.to_string()
        )
        .await
        .is_err()
    );
    assert!(
        sql::require_record(&mut tx, &id, "gate_execution", &gate.to_string())
            .await
            .is_err()
    );
    sql::require_record(&mut tx, &id, "gate_execution", &updated.to_string())
        .await
        .unwrap();
    sql::require_record(&mut tx, &id, "terminal_launch", &terminal.to_string())
        .await
        .unwrap();
    let reordered = r#"{"version":1,"release":{"Released":{"authorized_at_ms":42}},"decision":"Observing","execution_id":"gate"}"#;
    sql::require_record(&mut tx, &id, "gate_execution", reordered)
        .await
        .unwrap();
    let mut extra = updated.clone();
    extra["unexpected"] = json!(true);
    assert!(
        sql::require_record(&mut tx, &id, "gate_execution", &extra.to_string())
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
    let value: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?1")
        .bind(&id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&value).unwrap();
    assert_eq!(
        value["data"],
        json!({"other":"preserved","gate_execution":updated,"terminal_launch":terminal})
    );
}

// Reuse the real supervisor fixture; it contains transport setup, not owner policy.
use crate::operation::forge_action_adapter::tests::executor;

async fn pipe_admission(config: super::pipe::PipeConfig, gate_facade: bool) {
    use crate::operation::{OperationCompletionBus, SpawnCtx};
    use crate::{
        db::RouteRepo, event::EventBus, state::DaemonClient,
        terminal_renderer::TerminalRendererRegistry,
    };
    use std::sync::Arc;
    use workspace_runtime::execution::ExecutionSpecification;
    let db = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo = Arc::new(SqlxOperationRepo::new(db.db().clone()));
    let id = repo
        .insert_operation(
            "pipe-test",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: "hash".into(),
            },
            json!({}),
        )
        .await
        .unwrap();
    let mut output = TxOutput::new("unknown", None, json!({}));
    output.data = json!({"preserved": true});
    sqlx::query("UPDATE operations SET phase='spawn_started',tx_output_json=?2 WHERE id=?1")
        .bind(&id)
        .bind(serde_json::to_string(&output).unwrap())
        .execute(db.pool())
        .await
        .unwrap();
    let op = repo.claim_drive_operation(&id).await.unwrap().unwrap();
    let execution = executor::Executor::new(db.db().clone()).await;
    assert!(execution.lifecycle.engine().unwrap().available());
    let route: Arc<dyn RouteRepo> = db.clone();
    let secret = serde_json::from_reader(
        std::fs::File::open(execution._root.path().join("key.json")).unwrap(),
    )
    .unwrap();
    let engine = std::sync::Arc::new(workspace_runtime::execution::Engine::new(
        crate::execution_store::SqliteExecutionStore::new(db.db().clone()).with_projection(
            config.consumer.into(),
            Arc::new(crate::operation::gate_execution::Projection),
        ),
        workspace_runtime::execution::session_executor::SessionExecutor::new(
            execution._socket.path().join("control.sock"),
            secret,
        ),
    ));
    assert!(engine.recover().await.error.is_none());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !engine.available() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let lifecycle = crate::execution_quiesce::ExecutionLifecycle::default();
    lifecycle.bind(engine);
    let ctx = SpawnCtx::new(
        route.clone(),
        repo.clone(),
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new_with_repo(route),
        EventBus::new(),
        OperationCompletionBus::new(),
    )
    .with_execution(lifecycle);
    let cwd = tempfile::tempdir().unwrap();
    let paths = crate::operation::gate_lifecycle::GatePaths::new(cwd.path(), "test");
    let admitted = if gate_facade {
        crate::operation::gate_execution::admit(
            &op,
            &ctx,
            cwd.path().to_str().unwrap(),
            "pipe-track",
            crate::operation::gate_process::GateFiles {
                script: &paths.script,
                log: &paths.log,
                exit: &paths.exit,
                step: &paths.step,
            },
            crate::operation::gate_process::WrapperSteps::Logged(&[]),
            "pipe-marker",
        )
        .await
        .unwrap()
    } else {
        super::pipe::admit(
            config,
            &op,
            &ctx,
            cwd.path().canonicalize().unwrap(),
            "pipe-track",
            ExecutionSpecification {
                ambient_environment: calm_session::control::execution::AmbientEnvironment::None,
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "read -r go".into()],
                environment: vec![],
                io_mode: calm_session::control::IoMode::Pipe,
                replay_bytes: 8192,
            },
        )
        .await
        .unwrap()
    };
    let current = repo.get_operation(&id).await.unwrap().unwrap();
    let data = &current.tx_output.as_ref().unwrap().data;
    assert_eq!(data["preserved"], true);
    assert_eq!(
        data[config.record_key],
        serde_json::to_value(&admitted).unwrap()
    );
    assert_eq!(super::pipe::record(config, &current).unwrap(), admitted);
    let intent: String =
        sqlx::query_scalar("SELECT intent_json FROM workspace_executions WHERE execution_id=?1")
            .bind(admitted.execution_id.as_str())
            .fetch_one(db.pool())
            .await
            .unwrap();
    let intent: Value = serde_json::from_str(&intent).unwrap();
    assert_eq!(intent["consumer"], config.consumer, "{intent}");
    assert_eq!(intent["alias"], format!("{}:{id}", config.alias_prefix));
    if gate_facade {
        assert_eq!(config.consumer, "gate");
        assert_eq!(config.alias_prefix, "gate");
        assert_eq!(config.record_key, "gate_execution");
        assert_eq!(intent["program"]["program"], "/bin/sh");
        assert_eq!(intent["program"]["args"], json!([paths.script]));
        assert_eq!(intent["program"]["io_mode"], "Pipe");
        assert_eq!(intent["program"]["replay_bytes"], 8192);
        assert_eq!(
            std::fs::read_to_string(&paths.script).unwrap(),
            crate::operation::gate_process::render_gate_wrapper(&[])
        );
    } else {
        assert!(data.get("gate_execution").is_none());
    }
    // Persist Stop while the release pipe is held; never leave a test child running.
    super::pipe::stop(config, &ctx, &current, super::StopReason::Timeout)
        .await
        .unwrap();
}

#[tokio::test]
async fn w_pipe_non_gate_config_admits_own_identity_and_record() {
    pipe_admission(
        super::pipe::PipeConfig {
            consumer: "pipe-test",
            alias_prefix: "pipe-test",
            record_key: "terminal_launch",
        },
        false,
    )
    .await;
}

#[tokio::test]
async fn w_pipe_gate_config_preserves_literal_contract() {
    pipe_admission(crate::operation::gate_execution::CONFIG, true).await;
}
