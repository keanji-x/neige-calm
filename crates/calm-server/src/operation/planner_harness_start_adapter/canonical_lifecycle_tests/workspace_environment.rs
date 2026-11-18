//! Real project evaluation and native metadata must retain the original source fence.
use super::*;
use crate::shared_codex_appserver::native_owner_tests::{
    Peer, assert_no_request, connect, request, send,
};
use std::{path::PathBuf, time::Duration};

fn project_marker(fx: &Fixture) -> PathBuf {
    std::fs::write(
        fx._dir.path().join(".envrc"),
        "printf evaluated >> \"$PWD/environment-evaluated\"\nexport PROJECT_MODE=authority-test\n",
    )
    .unwrap();
    let allowed = std::process::Command::new("direnv")
        .arg("allow")
        .arg(fx._dir.path())
        .env_clear()
        .envs(crate::workspace_service::environment::directory_base_environment())
        .output()
        .unwrap();
    assert!(allowed.status.success(), "authorize the fixture project");
    fx._dir.path().join("environment-evaluated")
}

fn activate(
    fx: &Fixture,
    source: crate::auth::HumanInputSource,
) -> tokio::task::JoinHandle<Result<()>> {
    let route = RouteState::from_ref(&fx.state);
    let card = fx.card.clone();
    tokio::spawn(async move {
        CardStartFence::lock(
            &route.planner_recovery_locks,
            &route.repo,
            &route.operation_runtime,
            &card,
        )
        .await
        .wake_retained(
            route.planner_start_adapter.clone(),
            source,
            route.workspace_root.clone(),
            route.events.clone(),
        )
        .await
    })
}

async fn refused_without_rpc(
    fx: &Fixture,
    mut activation: tokio::task::JoinHandle<Result<()>>,
    peer: &mut Peer,
) {
    let unexpected = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut activation => {
                assert!(result.unwrap().is_err());
                None
            }
            call = request(peer) => {
                send(peer, json!({"id":call["id"],"error":{"code":-32000,"message":"unexpected revoked RPC"}})).await;
                assert!(activation.await.unwrap().is_err());
                Some(call["method"].clone())
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(unexpected, None, "revoked invocation must send no RPC");
    assert_no_request(peer).await;
    let row = fx
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
        .await
        .unwrap()
        .unwrap();
    assert!(row.spawn_op_id.is_none());
    assert!(row.thread_id.is_none());
    let frame: CanonicalHarnessSnapshot =
        serde_json::from_value(row.handle_state_json.unwrap()).unwrap();
    assert_eq!(frame.canonical_inputs.pending.len(), 1);
    assert!(frame.canonical_inputs.active.is_none());
    assert!(frame.canonical_inputs.attempts.is_empty());
}

#[tokio::test]
async fn metadata_sink_revocation_prevents_rpc_and_project_evaluation() {
    let fx = Fixture::new().await;
    let marker = project_marker(&fx);
    let (auth, token, source) = source().await;
    let (client, mut peer) = connect(&fx.state.shared_codex_appserver).await;
    let sink = client.lock_outbound_sink_for_test().await;
    let activation = activate(&fx, source);
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.pending_requests_for_test() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    auth.sessions.remove(&token).unwrap();
    drop(sink);
    refused_without_rpc(&fx, activation, &mut peer).await;
    assert!(
        !marker.exists(),
        "revoked metadata wait must not evaluate .envrc"
    );
}

#[tokio::test]
async fn metadata_reply_revocation_prevents_project_evaluation() {
    let fx = Fixture::new().await;
    let marker = project_marker(&fx);
    let (auth, token, source) = source().await;
    let (_client, mut peer) = connect(&fx.state.shared_codex_appserver).await;
    let activation = activate(&fx, source);
    let metadata = request(&mut peer).await;
    assert_eq!(metadata["method"], "config/read");
    assert_eq!(metadata["params"]["includeLayers"], false);
    assert!(!marker.exists(), "metadata precedes project evaluation");
    auth.sessions.remove(&token).unwrap();
    send(
        &mut peer,
        json!({"id":metadata["id"],"result":{"config":{}}}),
    )
    .await;
    refused_without_rpc(&fx, activation, &mut peer).await;
    assert!(!marker.exists());
}

#[tokio::test]
async fn local_environment_queue_revocation_prevents_project_evaluation() {
    let fx = Fixture::new().await;
    let marker = project_marker(&fx);
    let (auth, token, source) = source().await;
    let (_client, mut peer) = connect(&fx.state.shared_codex_appserver).await;
    let pause = crate::test_seams::PausePoint::default();
    let activation = activate(&fx, source);
    let metadata = request(&mut peer).await;
    assert_eq!(metadata["method"], "config/read");
    let selected_source = crate::workspace_service::environment::source_for_card(
        fx.repo.as_ref(),
        fx.card.as_str(),
        std::path::Path::new(metadata["params"]["cwd"].as_str().unwrap()),
    )
    .await
    .unwrap();
    // The source owner may return a root with a trailing separator. The seam
    // matches strings exactly, so register its actual selection before replying.
    crate::test_seams::install_pause_for_test(
        "workspace-environment-before-ready",
        selected_source.to_str().unwrap(),
        pause.clone(),
    );
    send(
        &mut peer,
        json!({"id":metadata["id"],"result":{"config":{}}}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    auth.sessions.remove(&token).unwrap();
    pause.release.notify_one();
    refused_without_rpc(&fx, activation, &mut peer).await;
    assert!(
        !marker.exists(),
        "admission must run after the local queue wait"
    );
}

#[tokio::test]
async fn project_evaluation_then_sink_revocation_prevents_thread_start() {
    let fx = Fixture::new().await;
    let marker = project_marker(&fx);
    let (auth, token, source) = source().await;
    let (client, mut peer) = connect(&fx.state.shared_codex_appserver).await;
    let activation = activate(&fx, source);
    let metadata = request(&mut peer).await;
    assert_eq!(metadata["method"], "config/read");
    let sink = client.lock_outbound_sink_for_test().await;
    send(
        &mut peer,
        json!({"id":metadata["id"],"result":{"config":{}}}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.exists() || client.pending_requests_for_test() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    auth.sessions.remove(&token).unwrap();
    drop(sink);
    refused_without_rpc(&fx, activation, &mut peer).await;
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "evaluated");
}
