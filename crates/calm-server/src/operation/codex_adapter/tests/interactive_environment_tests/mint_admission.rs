//! The actual first-mint callers must retain their operation/task owner across waits.
use super::*;
use crate::agent_backend_pool::codex::codex_route_tests::pool_with_weights;
use crate::shared_codex_appserver::native_owner_tests::{Peer, connect_with_reader, request, send};
use std::{path::PathBuf, time::Duration};

#[derive(Clone, Copy)]
enum Boundary {
    Metadata,
    Resource,
    Live,
}

fn project(source: &Path) -> PathBuf {
    std::fs::write(
        source.join(".envrc"),
        "printf evaluated >> \"$PWD/mint-project-evaluated\"\n",
    )
    .unwrap();
    let allowed = Command::new("direnv")
        .arg("allow")
        .arg(source)
        .env_clear()
        .envs(crate::workspace_service::environment::directory_base_environment())
        .output()
        .unwrap();
    assert!(
        allowed.status.success(),
        "authorize only the synthetic project"
    );
    source.join("mint-project-evaluated")
}

fn pause(source: &Path, boundary: Boundary) -> Option<crate::test_seams::PausePoint> {
    if !matches!(boundary, Boundary::Resource) {
        return None;
    }
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "workspace-environment-before-ready",
        source.to_str().unwrap(),
        pause.clone(),
    );
    Some(pause)
}

async fn reached(peer: &mut Peer, pause: &Option<crate::test_seams::PausePoint>) -> Option<Value> {
    let metadata = request(peer).await;
    assert_eq!(metadata["method"], "config/read");
    if let Some(pause) = pause {
        send(peer, json!({"id":metadata["id"],"result":{"config":{}}})).await;
        tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
            .await
            .unwrap();
        None
    } else {
        Some(metadata)
    }
}

async fn release(
    peer: &mut Peer,
    pause: Option<crate::test_seams::PausePoint>,
    metadata: Option<Value>,
) {
    if let Some(pause) = pause {
        pause.release.notify_one();
    }
    if let Some(metadata) = metadata {
        send(peer, json!({"id":metadata["id"],"result":{"config":{}}})).await;
    }
}

async fn settle<T: Send + 'static>(
    mut effect: tokio::task::JoinHandle<T>,
    peer: &mut Peer,
    live: bool,
) -> (T, Vec<String>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut methods = Vec::new();
        loop {
            tokio::select! {
                result = &mut effect => break (result.unwrap(), methods),
                call = request(peer) => {
                    let method = call["method"].as_str().unwrap().to_owned();
                    methods.push(method.clone());
                    if !live {
                        send(peer, json!({"id":call["id"],"error":{"code":-32000,"message":"unexpected revoked mint"}})).await;
                        continue;
                    }
                    let result = match method.as_str() {
                        "thread/start" => json!({"thread":{"id":"owned-mint-thread"}}),
                        "turn/start" => json!({"turn":{"id":"owned-mint-turn"}}),
                        other => panic!("unexpected native method {other}"),
                    };
                    send(peer, json!({"id":call["id"],"result":result})).await;
                    if method == "turn/start" {
                        send(peer, json!({"method":"turn/started","params":{"threadId":"owned-mint-thread","turn":{"id":"owned-mint-turn"}}})).await;
                    }
                }
            }
        }
    }).await.expect("the original first-mint attempt must finish")
}

async fn interactive(boundary: Boundary) {
    let mut f = idle_card().await;
    let (_pool_root, pool) = pool_with_weights(f._workspace.repo.clone(), &[1]).await;
    let shared = pool.default_only().clone();
    let (_client, mut peer, forwarder) = connect_with_reader(&shared).await;
    f.shared = shared.clone();
    f.adapter.shared_codex_appserver = shared;
    let source = crate::workspace_service::environment::source_for_card(
        f._workspace.repo.as_ref(),
        &f.output.output_string("card_id", "test").unwrap(),
        Path::new(&f.output.output_string("cwd", "test").unwrap()),
    )
    .await
    .unwrap();
    let marker = project(&source);
    let pause = pause(&source, boundary);
    let operations = f.operations.clone();
    let original = f.op.clone();
    let session = f.output.target_id.clone().expect("prepared worker session");
    let before = f
        ._workspace
        .repo
        .session_projection_by_id(&session)
        .await
        .unwrap();
    let repo = f._workspace.repo.clone();
    let effect = tokio::spawn(async move {
        let result = f
            .adapter
            .app_server_interact(&mut f.output, &f.op, &f.ctx)
            .await;
        (f, result)
    });
    let metadata = reached(&mut peer, &pause).await;
    let revoked = !matches!(boundary, Boundary::Live);
    if revoked {
        let replacement = operations
            .claim_operation_for_recovery(&original.id)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(replacement.lease_owner, original.lease_owner);
    }
    release(&mut peer, pause, metadata).await;
    let ((f, result), methods) = settle(effect, &mut peer, !revoked).await;
    if revoked {
        assert!(result.is_err());
        assert!(
            !marker.exists(),
            "a lost operation claim must prevent project evaluation"
        );
        assert!(
            methods.is_empty(),
            "a lost claim emitted native effects: {methods:?}"
        );
        assert_eq!(
            repo.session_projection_by_id(&session).await.unwrap(),
            before
        );
        let persisted = operations
            .get_operation(&original.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.phase, original.phase);
        assert_eq!(
            serde_json::to_value(persisted.tx_output).unwrap(),
            serde_json::to_value(original.tx_output).unwrap()
        );
    } else {
        result.unwrap();
        assert!(marker.exists());
        assert_eq!(methods, ["thread/start"]);
        assert_eq!(
            repo.session_projection_by_id(&session)
                .await
                .unwrap()
                .unwrap()
                .thread_id
                .as_deref(),
            Some("owned-mint-thread")
        );
        assert_eq!(f.pending.pending_count().await, 0);
    }
    forwarder.abort();
}

async fn worker(boundary: Boundary) {
    let mut h = worker_lease_harness().await;
    let (_pool_root, pool) = pool_with_weights(h.repo.clone(), &[1]).await;
    let shared = pool.default_only().clone();
    let (_client, mut peer, forwarder) = connect_with_reader(&shared).await;
    h.adapter.codex_backends = pool;
    let mcp = calm_test_sockets::socket_dir("mint-admission");
    h.adapter.mcp_server = Some(McpServer::new_for_test(crate::mcp_server::McpShimConfig {
        shim_bin: mcp.path().join("shim"),
        socket_path: mcp.path().join("mcp.sock"),
    }));
    let (operations, original, mut output) =
        prepare_worker_app_server_interact(&h, "mint-admission").await;
    let ctx = SpawnCtx::new(
        h.repo.clone(),
        operations.clone(),
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new_with_repo(h.repo.clone()),
        h.events.clone(),
        OperationCompletionBus::new(),
    );
    h.adapter
        .app_server_interact(&mut output, &original, &ctx)
        .await
        .unwrap();
    operations
        .set_phase(&original, Phase::SpawnStarted, None)
        .await
        .unwrap()
        .applied()
        .unwrap();
    let op = operations
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|op| op.id == original.id)
        .unwrap();
    let output = op.tx_output.as_ref().unwrap().clone();
    let card = output.output_string("card_id", "test").unwrap();
    let session = h
        .repo
        .session_projection_active_for_card(&card)
        .await
        .unwrap()
        .expect("the prepared card owns its original worker session")
        .id;
    let task = op.idempotency_key.clone().unwrap();
    let running_task = task.clone();
    let running_card = card.clone();
    crate::db::write_in_tx_typed(h.repo.as_ref(), move |tx| {
        Box::pin(async move {
            assert_eq!(
                crate::db::sqlite::task_mark_running_tx(
                    tx,
                    &running_task,
                    Some(&running_card),
                    now_ms(),
                    now_ms() + 60_000
                )
                .await?,
                1
            );
            Ok(())
        })
    })
    .await
    .unwrap();
    let before = h
        .repo
        .session_projection_by_id(&session)
        .await
        .unwrap()
        .unwrap();
    let source = crate::workspace_service::environment::source_for_card(
        h.repo.as_ref(),
        &card,
        Path::new(&output.output_string("cwd", "test").unwrap()),
    )
    .await
    .unwrap();
    let marker = project(&source);
    let pause = pause(&source, boundary);
    let repo = h.repo.clone();
    let effect = tokio::spawn(async move {
        let result = h.adapter.spawn_side_effect(&output, &op, &ctx).await;
        (h, result)
    });
    let metadata = reached(&mut peer, &pause).await;
    let revoked = !matches!(boundary, Boundary::Live);
    if revoked {
        let canceled_task = task.clone();
        crate::db::write_in_tx_typed(repo.as_ref(), move |tx| {
            Box::pin(async move {
                assert_eq!(
                    crate::db::sqlite::task_cancel_running_tx(
                        tx,
                        &canceled_task,
                        &card,
                        "fixture original owner canceled",
                        now_ms()
                    )
                    .await?,
                    1
                );
                Ok(())
            })
        })
        .await
        .unwrap();
    }
    release(&mut peer, pause, metadata).await;
    let ((_h, result), methods) = settle(effect, &mut peer, !revoked).await;
    let after = repo
        .session_projection_by_id(&session)
        .await
        .unwrap()
        .unwrap();
    if revoked {
        assert!(result.is_err());
        assert!(
            !marker.exists(),
            "a canceled original task must prevent project evaluation"
        );
        assert!(
            methods.is_empty(),
            "a canceled task emitted native effects: {methods:?}"
        );
        assert_eq!(after.thread_id, before.thread_id);
        assert_eq!(after.active_turn_id, before.active_turn_id);
        assert_eq!(after.status, before.status);
        assert_eq!(
            repo.task_get(&task).await.unwrap().unwrap().status,
            crate::model::TaskStatus::Canceled
        );
    } else {
        result.unwrap();
        assert!(marker.exists());
        assert_eq!(methods, ["thread/start", "turn/start"]);
        assert_eq!(after.thread_id.as_deref(), Some("owned-mint-thread"));
    }
    forwarder.abort();
}

#[tokio::test]
async fn interactive_claim_replaced_during_mint_metadata_refuses_project_and_native_start() {
    interactive(Boundary::Metadata).await;
}
#[tokio::test]
async fn interactive_claim_replaced_before_mint_resource_ready_refuses_project_and_native_start() {
    interactive(Boundary::Resource).await;
}
#[tokio::test]
async fn interactive_live_operation_owner_mints_one_native_thread() {
    interactive(Boundary::Live).await;
}
#[tokio::test]
async fn worker_task_canceled_during_mint_metadata_refuses_project_and_native_start() {
    worker(Boundary::Metadata).await;
}
#[tokio::test]
async fn worker_task_canceled_before_mint_resource_ready_refuses_project_and_native_start() {
    worker(Boundary::Resource).await;
}
#[tokio::test]
async fn worker_live_task_owner_mints_native_thread_and_first_turn() {
    worker(Boundary::Live).await;
}
