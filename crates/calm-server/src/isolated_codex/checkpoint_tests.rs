//! Real checkpoint callbacks retain private facts even without any live domain row.
use super::*;
use crate::dedicated_codex::{
    Checkpoint, Controller, ControllerConfig, DedicatedIdentity, DedicatedRequest, HomeSeed,
    NativeMcp, RequestPhase, SessionRecord,
};
use crate::operation::{OperationKey, OperationRepo, SqlxOperationRepo, TxOutput};
use serde_json::json;
use std::{sync::Arc, time::Duration};

struct Fixture {
    _root: tempfile::TempDir,
    controller: Controller,
    runtime: calm_worker_runtime::RuntimeConfig,
    session: SessionRecord,
    checkpoint: journal::OperationCheckpoint,
    repo: Arc<crate::db::sqlite::SqlxRepo>,
    _socket: std::os::unix::net::UnixListener,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("isolated-checkpoint-")
            .tempdir()
            .unwrap();
        // Capability stub for the fake provider only; actual outer namespace isolation is real.
        let sandbox_bwrap = root.path().join("sandbox-bwrap");
        std::fs::write(
            &sandbox_bwrap,
            "#!/bin/sh\nprintf '%s\\n' '--argv0 --perms --ro-bind --unshare-user --unshare-net'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sandbox_bwrap, std::fs::Permissions::from_mode(0o700)).unwrap();

        let repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
                .await
                .unwrap(),
        );
        let operations = SqlxOperationRepo::new(repo.pool().clone());
        let id = operations
            .insert_operation(
                OPERATION_KIND,
                OperationKey {
                    operation_key: crate::model::new_id(),
                    idempotency_key: Some("gone-task".into()),
                    payload_hash: "fixture".into(),
                },
                serde_json::to_value(WorkerPayload {
                    version: WorkerVersion::V1,
                    actor: crate::ids::ActorId::KernelDispatcher,
                    track_id: "gone-track".into(),
                    task_id: "gone-task".into(),
                    idempotency_key: "gone-task".into(),
                })
                .unwrap(),
            )
            .await
            .unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(workspace.join(".codex")).unwrap();
        let config_path = root.path().join("config.toml");
        let auth = root.path().join("auth.json");
        std::fs::write(&config_path, "model='fixture'\n").unwrap();
        std::fs::write(&auth, r#"{"tokens":{"access_token":"FAKE"}}"#).unwrap();
        let socket = root.path().join("mcp.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let runtime = calm_worker_runtime::RuntimeConfig {
            state_root: root.path().join("runtime"),
            helper: std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("calm-worker-boundary"),
            bwrap: "/usr/bin/bwrap".into(),
            timeout: Duration::from_secs(5),
        };
        let controller = Controller::new(ControllerConfig {
            private_root: root.path().join("private"),
            runtime: runtime.clone(),
            codex_binary: "/usr/bin/true".into(),
            code_mode_host_binary: std::env::current_exe().unwrap(),
            mcp_shim: "/usr/bin/true".into(),
            sandbox_bwrap,
            provider_environment: Default::default(),
            connect_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_secs(3),
        })
        .unwrap();
        let request = DedicatedRequest {
            identity: DedicatedIdentity {
                run_id: id.clone(),
                attempt_id: "gone-task".into(),
                card_id: "gone-card".into(),
                session_id: "gone-session".into(),
            },
            workspace,
            developer_instructions: "Previously prepared execution; task has been deleted.".into(),
        };
        let endpoint = controller
            .prepare(
                request.clone(),
                &HomeSeed::read(&config_path, &auth).unwrap(),
                &NativeMcp {
                    socket,
                    card_token: "FAKE-NATIVE".into(),
                    plugin_tools: Vec::new(),
                },
            )
            .await
            .unwrap();
        let session = SessionRecord::prepared(endpoint);
        let record = record::RunRecord {
            version: record::RecordVersion::V1,
            request,
            track_id: "gone-track".into(),
            native_token: "FAKE-NATIVE".into(),
            admission: record::Admission::Closed,
            provider: record::ProviderRecord::Prepared(Box::new(session.clone())),
        };
        let mut output = TxOutput::new("card", Some("gone-card".into()), json!({}));
        output.data = json!({"isolated_execution":record});
        sqlx::query("UPDATE operations SET phase='spawn_started',lease_owner='owner',target_type='card',target_id='gone-card',tx_output_json=?1 WHERE id=?2")
            .bind(serde_json::to_string(&output).unwrap()).bind(&id).execute(repo.pool()).await.unwrap();
        let operation = operations.get_operation(&id).await.unwrap().unwrap();
        let checkpoint = journal::OperationCheckpoint {
            repo: repo.clone(),
            operation,
            events: crate::event::EventBus::new(),
            task_timeout_ms: 30000,
        };
        Self {
            _root: root,
            controller,
            runtime,
            session,
            checkpoint,
            repo,
            _socket: listener,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = calm_worker_runtime::Runtime::new(self.runtime.clone())
            .and_then(|r| r.stop(&self.session.endpoint.boundary, Duration::from_secs(3)));
    }
}

#[tokio::test]
async fn isolated_checkpoint_stop_without_domain_events_commits_quiescence() {
    let f = Fixture::new().await;
    let mut session = f.session.clone();
    let state = f
        .controller
        .stop(&mut session, &f.checkpoint, Duration::from_secs(3))
        .await
        .unwrap();
    assert!(matches!(
        state,
        calm_worker_runtime::BoundaryState::Quiesced(_)
    ));
    let id = f.checkpoint.operation.id.clone();
    let stored = crate::db::write_in_tx_typed(f.repo.as_ref(), move |tx| {
        Box::pin(async move { journal::load_tx(tx, &id).await })
    })
    .await
    .unwrap();
    assert_eq!(stored.session().unwrap(), &session);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "private cleanup must not fabricate a business event"
    );
}

#[tokio::test]
async fn isolated_checkpoint_late_ack_without_live_session_keeps_closed_admission() {
    let mut f = Fixture::new().await;
    f.session.phase = RequestPhase::ProviderStarting;
    sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.isolated_execution.provider.record',json(?1)) WHERE id=?2")
        .bind(serde_json::to_string(&f.session).unwrap()).bind(&f.checkpoint.operation.id).execute(f.repo.pool()).await.unwrap();
    let mut reply = f.session.clone();
    reply.phase = RequestPhase::Connected;
    f.checkpoint.save(&f.session, &reply).await.unwrap();
    let id = f.checkpoint.operation.id.clone();
    let stored = crate::db::write_in_tx_typed(f.repo.as_ref(), move |tx| {
        Box::pin(async move { journal::load_tx(tx, &id).await })
    })
    .await
    .unwrap();
    assert_eq!(stored.session().unwrap(), &reply);
    assert_eq!(stored.admission, record::Admission::Closed);
    assert!(
        f.checkpoint.save(&f.session, &reply).await.is_err(),
        "stale ACK still fails exact CAS"
    );
}

#[tokio::test]
async fn isolated_missing_task_failure_observation_is_a_noop() {
    let f = Fixture::new().await;
    let adapter = adapter::IsolatedCodexAdapter::new(
        None,
        f.repo.clone(),
        None,
        crate::state::WriteContext::new(Default::default(), Default::default()),
    );
    let ctx = crate::operation::SpawnCtx::new(
        f.repo.clone(),
        Arc::new(SqlxOperationRepo::new(f.repo.pool().clone())),
        Arc::new(crate::state::DaemonClient::new_stub()),
        crate::terminal_renderer::TerminalRendererRegistry::new_with_repo(f.repo.clone()),
        f.checkpoint.events.clone(),
        crate::operation::OperationCompletionBus::new(),
    );
    observe::fail(
        &adapter,
        &f.checkpoint.operation,
        &ctx,
        "late turn completion",
    )
    .await
    .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn isolated_checkpoint_stale_owner_cannot_stop_or_save() {
    let f = Fixture::new().await;
    let mut session = f.session.clone();
    sqlx::query("UPDATE operations SET lease_owner='replacement' WHERE id=?1")
        .bind(&f.checkpoint.operation.id)
        .execute(f.repo.pool())
        .await
        .unwrap();
    let stopped = f
        .controller
        .stop(&mut session, &f.checkpoint, Duration::from_secs(3))
        .await;
    assert!(stopped.is_err());
    assert!(matches!(
        calm_worker_runtime::Runtime::new(f.runtime.clone())
            .unwrap()
            .probe(&f.session.endpoint.boundary)
            .unwrap(),
        calm_worker_runtime::BoundaryState::Prepared
    ));
    let id = f.checkpoint.operation.id.clone();
    let record = crate::db::write_in_tx_typed(f.repo.as_ref(), move |tx| {
        Box::pin(async move { journal::load_tx(tx, &id).await })
    })
    .await
    .unwrap();
    assert_eq!(record.session().unwrap(), &f.session);
}

#[tokio::test]
async fn isolated_native_socket_replacement_refuses_restart_but_allows_owned_stop() {
    let f = Fixture::new().await;
    let socket = f.session.endpoint.home.mcp_source_socket.clone();
    std::fs::remove_file(&socket).unwrap();
    let _replacement = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    assert!(
        f.controller
            .connect(f.session.clone(), &f.checkpoint)
            .await
            .is_err()
    );
    let mut record = f.session.clone();
    let stopped = f
        .controller
        .stop(&mut record, &f.checkpoint, Duration::from_secs(3))
        .await
        .unwrap();
    assert!(matches!(
        stopped,
        calm_worker_runtime::BoundaryState::Quiesced(_)
    ));
    assert_eq!(
        record.phase,
        RequestPhase::Prepared,
        "restart must not start a provider or turn"
    );
}
