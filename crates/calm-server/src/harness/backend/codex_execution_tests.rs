//! Actual dispatch and current-native observation through the shared RPC core.
use super::dispatch_fixture::Owner;
use super::*;
use crate::shared_codex_appserver::native_owner_tests::{
    Peer, connect, notify_thread, request, send,
};
use calm_types::conversation::SubmissionDispatchProof;
use serde_json::json;

struct Fixture {
    _dir: tempfile::TempDir,
    owner: Owner,
    backend: PlannerBackend,
    daemon: Arc<SharedCodexAppServer>,
}

#[tokio::test]
async fn codex_original_native_end_keeps_its_first_terminal_fact() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    fx.accepted(&mut peer, "immutable-original").await;
    notify_thread(
        &fx.daemon,
        &mut peer,
        &fx.owner.thread,
        "turn/completed",
        json!({"id":"immutable-original","status":"interrupted"}),
    )
    .await;
    notify_thread(
        &fx.daemon,
        &mut peer,
        &fx.owner.thread,
        "turn/completed",
        json!({"id":"immutable-original","status":"completed"}),
    )
    .await;
    let observed = fx
        .backend
        .qualify_execution_end(&fx.target("immutable-original"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        observed.terminal().unwrap()["status"],
        "interrupted",
        "a conflicting duplicate cannot rewrite the actual original END"
    );
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let owner = super::dispatch_fixture::seed(dir.path(), AgentProvider::Codex).await;
        let daemon = SharedCodexAppServer::new_stub(owner.repo.clone());
        let backend =
            PlannerBackend::codex(daemon.clone(), &owner.card, &owner.track, &owner.worker)
                .unwrap();
        Self {
            _dir: dir,
            owner,
            backend,
            daemon,
        }
    }

    fn start(&self) -> tokio::task::JoinHandle<InputAttemptReport> {
        let backend = self.backend.clone();
        let thread = self.owner.thread.clone();
        tokio::spawn(async move {
            let proof = super::dispatch_tests::proof();
            backend
                .dispatch_input(
                    None,
                    &super::dispatch_tests::logical_turn_id(),
                    proof.attempt(),
                    &thread,
                    &proof,
                    vec![],
                    &TurnModelSelection::inherit(),
                    PlannerPermissionMode::Never,
                    &crate::harness::native_admission::NativePromptAdmission::new(
                        || Ok(()),
                        || Box::pin(async { Ok(()) }),
                    ),
                )
                .await
        })
    }

    fn target(&self, turn: &str) -> ExecutionEndTarget {
        ExecutionEndTarget {
            card_id: self.owner.card.clone(),
            track_id: self.owner.track.clone(),
            worker_session_id: self.owner.worker.clone(),
            logical_turn_id: super::dispatch_tests::logical_turn_id(),
            proof: super::dispatch_tests::proof(),
            scope: ExecutionEndScope::NativeTurn {
                thread_id: self.owner.thread.clone(),
                native_turn_id: turn.into(),
            },
        }
    }

    async fn notify(&self, peer: &mut Peer, method: &str, turn: &str) {
        notify_thread(
            &self.daemon,
            peer,
            &self.owner.thread,
            method,
            json!({"id":turn,"status":if method == "turn/completed" {"completed"} else {"inProgress"}}),
        )
        .await;
    }

    async fn accepted(&self, peer: &mut Peer, turn: &str) -> InputAttemptReport {
        let task = self.start();
        let req = request(peer).await;
        assert_eq!(req["method"], "turn/start");
        assert_eq!(req["params"]["threadId"], self.owner.thread);
        self.notify(peer, "turn/started", turn).await;
        send(peer, json!({"id":req["id"],"result":{"turn":{"id":turn}}})).await;
        let report = task.await.unwrap();
        assert!(matches!(report.delivery(), InputDelivery::Accepted(_)));
        report
    }
}

#[tokio::test]
async fn codex_fast_end_is_bound_to_the_actual_row_and_original_initial() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let task = fx.start();
    let req = request(&mut peer).await;
    fx.notify(&mut peer, "turn/started", "fast").await;
    fx.notify(&mut peer, "turn/completed", "fast").await;
    send(
        &mut peer,
        json!({"id":req["id"],"result":{"turn":{"id":"fast"}}}),
    )
    .await;
    let report = task.await.unwrap();
    assert!(matches!(report.delivery(), InputDelivery::Accepted(_)));
    let target = fx.target("fast");
    let observation = fx
        .backend
        .qualify_execution_end(&target)
        .await
        .unwrap()
        .expect("actual matched END");
    assert!(observation.matches(&target));
    for field in [
        "card", "track", "runtime", "logical", "proof", "thread", "native",
    ] {
        let mut foreign = target.clone();
        match field {
            "card" => foreign.card_id = "other-card".into(),
            "track" => foreign.track_id = "other-track".into(),
            "runtime" => foreign.worker_session_id = "other-runtime".into(),
            "logical" => {
                foreign.logical_turn_id = calm_types::conversation::LogicalTurnId::parse(
                    "77777777777777777777777777777777",
                )
                .unwrap()
            }
            "proof" => {
                let mut proof = serde_json::to_value(&foreign.proof).unwrap();
                proof["attempt"]["attempt_id"] = json!("66666666666666666666666666666666");
                foreign.proof = serde_json::from_value(proof).unwrap();
            }
            "thread" => {
                foreign.scope = ExecutionEndScope::NativeTurn {
                    thread_id: "other-thread".into(),
                    native_turn_id: "fast".into(),
                }
            }
            "native" => {
                foreign.scope = ExecutionEndScope::NativeTurn {
                    thread_id: fx.owner.thread.clone(),
                    native_turn_id: "other-turn".into(),
                }
            }
            _ => unreachable!(),
        }
        assert!(
            fx.backend
                .qualify_execution_end(&foreign)
                .await
                .unwrap()
                .is_none(),
            "{field}"
        );
    }
    let other = Fixture::new().await;
    let foreign_backend = PlannerBackend::codex(
        fx.daemon.clone(),
        &other.owner.card,
        &other.owner.track,
        &other.owner.worker,
    )
    .unwrap();
    assert!(
        foreign_backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
    // An already obtained END is a fact. Replacement only blocks new qualification.
    let (_new_client, _new_peer) = connect(&fx.daemon).await;
    assert!(
        fx.backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
    assert!(observation.matches(&target));
    assert!(matches!(report.delivery(), InputDelivery::Accepted(_)));
}

#[tokio::test]
async fn codex_budget_notice_inflight_blocks_observation_but_never_replaces_initial() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    fx.accepted(&mut peer, "initial").await;
    let target = fx.target("initial");
    let mut proof = serde_json::to_value(&target.proof).unwrap();
    proof["attempt"]["submission_id"] = json!("88888888888888888888888888888888");
    proof["attempt"]["attempt_id"] = json!("99999999999999999999999999999999");
    proof["render_plan"]["submission_id"] = json!("88888888888888888888888888888888");
    let proof: SubmissionDispatchProof = serde_json::from_value(proof).unwrap();
    let backend = fx.backend.clone();
    let thread = fx.owner.thread.clone();
    let scope = target.clone();
    let notice = crate::planner_turn_budget::BudgetControlNotice::for_turn(
        10.try_into().unwrap(),
        &thread,
        "initial",
        "original-start",
    )
    .unwrap();
    let task = tokio::spawn(async move {
        backend
            .send_budget_notice(
                &notice,
                BudgetControlScope::Canonical(Box::new(scope)),
                &crate::harness::native_admission::NativePromptAdmission::new(
                    || Ok(()),
                    || Box::pin(async { Ok(()) }),
                ),
            )
            .await
    });
    let req = request(&mut peer).await;
    assert_eq!(req["method"], "turn/steer");
    fx.notify(&mut peer, "turn/completed", "initial").await;
    assert!(
        fx.backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
    send(
        &mut peer,
        json!({"id":req["id"],"result":{"turnId":"initial"}}),
    )
    .await;
    assert!(matches!(
        task.await.unwrap().delivery(),
        ControlDelivery::Accepted { native_turn_id } if native_turn_id == "initial"
    ));
    assert!(
        fx.backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_some()
    );
    let different_input = ExecutionEndTarget { proof, ..target };
    assert!(
        fx.backend
            .qualify_execution_end(&different_input)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn codex_refused_candidate_stays_interruptible_without_acceptance_or_end() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let task = fx.start();
    let req = request(&mut peer).await;
    fx.notify(&mut peer, "turn/started", "candidate").await;
    send(
        &mut peer,
        json!({"id":req["id"],"error":{"code":-32600,"message":"refused"}}),
    )
    .await;
    let report = task.await.unwrap();
    assert!(matches!(report.delivery(), InputDelivery::Unknown(_)));
    assert_eq!(
        fx.daemon
            .active_turn_id_for_thread(&fx.owner.thread)
            .as_deref(),
        Some("candidate")
    );
    assert!(
        fx.backend
            .qualify_execution_end(&fx.target("candidate"))
            .await
            .unwrap()
            .is_none()
    );
    let daemon = fx.daemon.clone();
    let thread = fx.owner.thread.clone();
    let stop = tokio::spawn(async move { daemon.interrupt_active_outcome(&thread).await });
    let req = request(&mut peer).await;
    assert_eq!(req["method"], "turn/interrupt");
    assert_eq!(req["params"]["turnId"], "candidate");
    send(&mut peer, json!({"id":req["id"],"result":{}})).await;
    assert!(matches!(
        stop.await.unwrap(),
        provider::control::NativeInterruptOutcome::NativeAck
    ));
    assert!(
        fx.backend
            .qualify_execution_end(&fx.target("candidate"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(report.delivery(), InputDelivery::Unknown(_)));
}

#[tokio::test]
async fn old_source_accepted_cleanup_error_stays_scoped_to_the_original_connection() {
    let fx = Fixture::new().await;
    let (_old, mut old_peer) = connect(&fx.daemon).await;
    let task = fx.start();
    let req = request(&mut old_peer).await;
    let (_new, mut peer) = connect(&fx.daemon).await;
    fx.notify(&mut peer, "turn/started", "current").await;
    fx.daemon.thread_seals().seal_for_deletion(&fx.owner.thread);
    send(
        &mut old_peer,
        json!({"id":req["id"],"result":{"turn":{"id":"old"}}}),
    )
    .await;
    let cleanup = request(&mut old_peer).await;
    assert_eq!(cleanup["method"], "turn/interrupt");
    assert_eq!(cleanup["params"]["turnId"], "old");
    send(
        &mut old_peer,
        json!({"id":cleanup["id"],"error":{"code":-32600,"message":"cleanup failed"}}),
    )
    .await;
    let report = task.await.unwrap();
    assert!(
        matches!(report.delivery(),InputDelivery::Accepted(receipt) if receipt.native_turn_id()=="old")
    );
    assert!(report.outcome().is_err());
    assert_eq!(
        fx.daemon
            .active_turn_id_for_thread(&fx.owner.thread)
            .as_deref(),
        Some("current")
    );
}

#[tokio::test]
async fn old_source_end_cannot_qualify_the_current_execution() {
    let fx = Fixture::new().await;
    let (_old, mut old_peer) = connect(&fx.daemon).await;
    let (_current, mut peer) = connect(&fx.daemon).await;
    fx.accepted(&mut peer, "current").await;
    let target = fx.target("current");
    fx.notify(&mut old_peer, "turn/completed", "current").await;
    assert!(
        fx.backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fx.daemon
            .active_turn_id_for_thread(&fx.owner.thread)
            .as_deref(),
        Some("current")
    );
    fx.notify(&mut peer, "turn/completed", "current").await;
    assert!(
        fx.backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn rebuilt_native_slots_cannot_recover_association_from_a_qualification_target() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    fx.accepted(&mut peer, "ended").await;
    fx.notify(&mut peer, "turn/completed", "ended").await;
    let target = fx.target("ended");
    let observed = fx
        .backend
        .qualify_execution_end(&target)
        .await
        .unwrap()
        .unwrap();
    crate::shared_codex_appserver::native_owner_tests::rebuild(&fx.daemon).await;
    fx.notify(&mut peer, "turn/started", "ended").await;
    fx.notify(&mut peer, "turn/completed", "ended").await;
    assert!(
        fx.backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
    assert!(observed.matches(&target));
}

#[tokio::test]
async fn cold_persisted_native_thread_is_not_proven_idle_by_an_empty_process_cache() {
    let fx = Fixture::new().await;
    assert!(matches!(
        fx.daemon.interrupt_active_outcome(&fx.owner.thread).await,
        provider::control::NativeInterruptOutcome::Unconfirmed { .. }
    ));
}

#[tokio::test]
async fn reader_loss_rejects_future_end_before_a_blocked_forwarder_can_retire() {
    use crate::shared_codex_appserver::native_owner_tests::{
        attribution_blocked, connect_with_reader, hold_attribution, reader_finished,
    };
    let fx = Fixture::new().await;
    let (client, mut peer, forwarder) = connect_with_reader(&fx.daemon).await;
    let report = fx.accepted(&mut peer, "ended").await;
    fx.notify(&mut peer, "turn/completed", "ended").await;
    let target = fx.target("ended");
    let observed = fx
        .backend
        .qualify_execution_end(&target)
        .await
        .unwrap()
        .unwrap();
    let hold = hold_attribution(&fx.daemon).await;
    send(
        &mut peer,
        json!({"method":"thread/started","params":{"thread":{"id":"attribution-held"}}}),
    )
    .await;
    attribution_blocked(&fx.daemon).await;
    drop(peer);
    reader_finished(&client).await;
    assert!(fx.daemon.is_running());
    assert!(observed.matches(&target));
    assert!(matches!(report.delivery(), InputDelivery::Accepted(_)));
    assert!(
        fx.backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
    drop(hold);
    forwarder.await.unwrap();
}

#[tokio::test]
async fn forwarder_abort_retires_the_actual_source_without_revoking_observed_facts() {
    use crate::shared_codex_appserver::native_owner_tests::connect_with_reader;
    let fx = Fixture::new().await;
    let (client, mut peer, forwarder) = connect_with_reader(&fx.daemon).await;
    let report = fx.accepted(&mut peer, "ended").await;
    fx.notify(&mut peer, "turn/completed", "ended").await;
    let target = fx.target("ended");
    let observed = fx
        .backend
        .qualify_execution_end(&target)
        .await
        .unwrap()
        .unwrap();
    forwarder.abort();
    assert!(forwarder.await.unwrap_err().is_cancelled());
    assert!(!client.reader_finished());
    assert!(client.notification_stream_closed());
    assert!(observed.matches(&target));
    assert!(matches!(report.delivery(), InputDelivery::Accepted(_)));
    assert!(
        fx.backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        fx.daemon.interrupt_active_outcome(&fx.owner.thread).await,
        provider::control::NativeInterruptOutcome::Unconfirmed { .. }
    ));
}

#[tokio::test]
async fn retired_reader_eof_cannot_retire_the_new_source_or_its_end() {
    use crate::shared_codex_appserver::native_owner_tests::connect_with_reader;
    let fx = Fixture::new().await;
    let (_old, old_peer, old_forwarder) = connect_with_reader(&fx.daemon).await;
    let (_current, mut peer) = connect(&fx.daemon).await;
    fx.accepted(&mut peer, "current").await;
    fx.notify(&mut peer, "turn/completed", "current").await;
    drop(old_peer);
    old_forwarder.await.unwrap();
    assert!(
        fx.backend
            .qualify_execution_end(&fx.target("current"))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn reader_eof_drops_queued_receipt_fanout_after_actual_attribution_wait() {
    use crate::shared_codex_appserver::native_owner_tests::{
        attribution_blocked, connect_with_reader, hold_attribution, reader_finished,
    };
    let fx = Fixture::new().await;
    let (client, mut peer, forwarder) = connect_with_reader(&fx.daemon).await;
    let mut seen = fx.daemon.subscribe_notifications();
    let hold = hold_attribution(&fx.daemon).await;
    send(
        &mut peer,
        json!({"method":"thread/started","params":{"thread":{"id":"attribution-held"}}}),
    )
    .await;
    send(&mut peer,json!({"method":"item/completed","params":{"threadId":fx.owner.thread,"item":{"type":"userMessage","clientId":"11111111111111111111111111111111"}}})).await;
    attribution_blocked(&fx.daemon).await;
    drop(peer);
    reader_finished(&client).await;
    drop(hold);
    forwarder.await.unwrap();
    while let Ok(event) = seen.try_recv() {
        assert!(
            !matches!(event, crate::codex_appserver::Notification::Item { .. }),
            "dead reader's queued receipt escaped source qualification"
        );
    }
}

impl Fixture {
    async fn interrupt_ack(&self, peer: &mut Peer, turn: &str) {
        // Exercise the same native transport used by the owner-bound managed interrupt.
        // These tests also interrupt a start candidate before its acknowledgement;
        // canonical durable control deliberately serializes behind that start.
        let daemon = self.daemon.clone();
        let thread = self.owner.thread.clone();
        let turn = turn.to_owned();
        let task = tokio::spawn(async move { daemon.interrupt_outcome(&thread, &turn).await });
        let req = request(peer).await;
        assert_eq!(req["method"], "turn/interrupt");
        send(peer, json!({"id":req["id"],"result":{}})).await;
        assert!(matches!(
            task.await.unwrap(),
            provider::control::NativeInterruptOutcome::NativeAck
        ));
    }

    async fn assert_original_end(&self, turn: &str) {
        let target = self.target(turn);
        let observed = self.backend.qualify_execution_end(&target).await.unwrap();
        assert!(
            observed.is_some(),
            "actual same-source END lost the Original Initial association"
        );
        assert!(observed.unwrap().matches(&target));
    }
}

#[tokio::test]
async fn interrupt_ack_preserves_original_association_for_later_native_end() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let report = fx.accepted(&mut peer, "interrupted").await;
    fx.interrupt_ack(&mut peer, "interrupted").await;
    assert!(
        fx.backend
            .qualify_execution_end(&fx.target("interrupted"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(fx.daemon.active_turn_id_for_thread(&fx.owner.thread), None);
    fx.notify(&mut peer, "turn/completed", "interrupted").await;
    fx.assert_original_end("interrupted").await;
    assert!(matches!(report.delivery(), InputDelivery::Accepted(_)));
}

async fn interrupted_candidate_retains_initial(end_before_start_ack: bool) {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let task = fx.start();
    let req = request(&mut peer).await;
    fx.notify(&mut peer, "turn/started", "candidate").await;
    fx.interrupt_ack(&mut peer, "candidate").await;
    assert!(
        fx.backend
            .qualify_execution_end(&fx.target("candidate"))
            .await
            .unwrap()
            .is_none()
    );
    if end_before_start_ack {
        fx.notify(&mut peer, "turn/completed", "candidate").await;
    }
    send(
        &mut peer,
        json!({"id":req["id"],"result":{"turn":{"id":"candidate"}}}),
    )
    .await;
    let report = task.await.unwrap();
    assert!(matches!(report.delivery(), InputDelivery::Accepted(_)));
    if !end_before_start_ack {
        assert!(
            fx.backend
                .qualify_execution_end(&fx.target("candidate"))
                .await
                .unwrap()
                .is_none()
        );
        fx.notify(&mut peer, "turn/completed", "candidate").await;
    }
    fx.assert_original_end("candidate").await;
}

#[tokio::test]
async fn interrupted_pending_candidate_keeps_original_through_start_ack_then_end() {
    interrupted_candidate_retains_initial(false).await;
}

#[tokio::test]
async fn interrupted_pending_candidate_keeps_original_through_end_then_start_ack() {
    interrupted_candidate_retains_initial(true).await;
}

#[tokio::test]
async fn compatibility_abort_keeps_original_for_a_subsequent_formal_end() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    fx.accepted(&mut peer, "aborted").await;
    fx.notify(&mut peer, "turn/aborted", "aborted").await;
    assert!(
        fx.backend
            .qualify_execution_end(&fx.target("aborted"))
            .await
            .unwrap()
            .is_none()
    );
    fx.notify(&mut peer, "turn/completed", "aborted").await;
    fx.assert_original_end("aborted").await;
}

#[tokio::test]
async fn sealed_pending_cleanup_preserves_original_ack_and_already_observed_end() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let task = fx.start();
    let start = request(&mut peer).await;
    fx.daemon.thread_seals().seal_for_deletion(&fx.owner.thread);
    fx.notify(&mut peer, "turn/started", "sealed").await;
    let cleanup = request(&mut peer).await;
    assert_eq!(cleanup["method"], "turn/interrupt");
    send(&mut peer, json!({"id":cleanup["id"],"result":{}})).await;
    // Observe the actual cleanup's control projection, without substituting a
    // task flag or a fabricated terminal for its real peer acknowledgement.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while fx
            .daemon
            .active_turn_id_for_thread(&fx.owner.thread)
            .is_some()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fx.notify(&mut peer, "turn/completed", "sealed").await;
    assert!(
        fx.backend
            .qualify_execution_end(&fx.target("sealed"))
            .await
            .unwrap()
            .is_none()
    );
    send(
        &mut peer,
        json!({"id":start["id"],"result":{"turn":{"id":"sealed"}}}),
    )
    .await;
    // The accepted-response path independently runs its existing sealed cleanup.
    let accepted_cleanup = request(&mut peer).await;
    assert_eq!(accepted_cleanup["method"], "turn/interrupt");
    send(&mut peer, json!({"id":accepted_cleanup["id"],"result":{}})).await;
    let report = task.await.unwrap();
    assert!(matches!(report.delivery(), InputDelivery::Accepted(_)));
    assert!(report.outcome().is_err());
    fx.assert_original_end("sealed").await;
}

#[tokio::test]
async fn h2_remote_start_error_holds_input_but_cleans_unstarted_native_slot() {
    for code in [-32600, -32602, -32099] {
        let fx = Fixture::new().await;
        let (_client, mut peer) = connect(&fx.daemon).await;
        let task = fx.start();
        let call = request(&mut peer).await;
        send(&mut peer, json!({"id":call["id"],"error":{"code":code,"message":"opaque","data":{"detail":true}}})).await;
        let report = task.await.unwrap();
        assert!(matches!(report.delivery(), InputDelivery::Unknown(_)));
        assert!(report.outcome().is_err());
        assert_eq!(fx.daemon.active_turn_id_for_thread(&fx.owner.thread), None);
        // Holding application input must not occupy a factually unstarted native slot.
        fx.accepted(&mut peer, "corrected").await;
    }
}

mod h2_delivery;
