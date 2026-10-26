//! Grok maintenance uses the canonical backend entry and the existing execution owner.
use super::*;
use crate::harness::backend::{MaintenanceObservation, MaintenanceTarget};
use crate::harness::native_admission::NativePromptAdmission;
use provider::control::CompactSubmission;

pub(super) async fn fixture() -> Fixture {
    Fixture::with_provider_version(AgentProvider::Grok, "reply", false, Some("1.0.70")).await
}
#[tokio::test]
async fn acp_grok_compact_retained_operation_never_sends_second_request() {
    super::compact::retained_operation(true).await;
}
#[tokio::test]
async fn acp_grok_compact_unknown_hung_timeout_no_resend() {
    interrupted(false).await;
}
#[tokio::test]
async fn acp_grok_compact_cancel_unconfirmed() {
    interrupted(true).await;
}
async fn interrupted(cancel: bool) {
    let fx = fixture().await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    std::fs::write(fx.dir.path().join("scenario"), "grok-compact-hung").unwrap();
    let target = MaintenanceTarget {
        card_id: fx.owner.card.clone(),
        track_id: fx.owner.track.clone(),
        worker_session_id: fx.owner.worker.clone(),
        thread_id: fx.owner.thread.clone(),
        operation_id: crate::model::new_id(),
    };
    let admission = NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) }));
    let submit = || CompactRequest {
        selection: TurnModelSelection::inherit(),
    };
    assert!(matches!(
        fx.backend
            .compact_canonical(
                target.clone(),
                submit(),
                &admission,
                &fx.backend.handoff_for_test().await
            )
            .await
            .unwrap(),
        CompactSubmission::Submitted { .. }
    ));
    let turn = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if fx
                .requests()
                .iter()
                .any(|f| f["method"] == "_x.ai/compact_conversation")
                && let Some(MaintenanceObservation::Turn {
                    turn_id,
                    settled: None,
                }) = fx.backend.observe_maintenance(&target).await.unwrap()
            {
                break turn_id;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        !fx.session.driver_finished_for_test(),
        "hung peer remains alive with no response"
    );
    let pid = fx
        .requests()
        .into_iter()
        .find(|f| f["method"] == "_x.ai/compact_conversation")
        .unwrap()["_pid"]
        .as_i64()
        .unwrap();
    assert!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok(),
        "hung peer process is alive"
    );
    if cancel {
        fx.session.turn_interrupt(&fx.owner.thread, &turn).await;
    } else {
        // Only the existing native response deadline advances; no hour-long wall wait.
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(3601)).await;
        tokio::time::resume();
    }
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if matches!(
                fx.backend.observe_maintenance(&target).await.unwrap(),
                Some(MaintenanceObservation::Turn {
                    settled: Some(false),
                    ..
                })
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    assert!(
        matches!(
            fx.backend
                .compact_canonical(
                    target.clone(),
                    submit(),
                    &admission,
                    &fx.backend.handoff_for_test().await
                )
                .await
                .unwrap(),
            CompactSubmission::Unconfirmed { .. }
        ),
        "timeout/cancellation must never resend the operation"
    );
    assert_eq!(
        fx.requests()
            .iter()
            .filter(|f| f["method"] == "_x.ai/compact_conversation")
            .count(),
        1
    );
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn acp_grok_compact_failed_join_retired() {
    super::compact::grok_failed_join().await;
}

#[tokio::test]
async fn acp_grok_compact_pre_send_live_completion_cannot_certify_empty_response() {
    send_window(true, true).await;
}
#[tokio::test]
async fn acp_grok_compact_fast_post_write_completion_is_preserved() {
    send_window(false, true).await;
}
#[tokio::test]
async fn acp_grok_compact_old_live_without_matching_response_never_completes() {
    send_window(true, false).await;
}
async fn send_window(pre_send: bool, response: bool) {
    let fx = fixture().await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    std::fs::write(
        fx.dir.path().join("scenario"),
        if pre_send {
            if response {
                "grok-compact-pre-send"
            } else {
                "grok-compact-pre-send-lost-response"
            }
        } else {
            "grok-compact-success"
        },
    )
    .unwrap();
    let target = MaintenanceTarget {
        card_id: fx.owner.card.clone(),
        track_id: fx.owner.track.clone(),
        worker_session_id: fx.owner.worker.clone(),
        thread_id: fx.owner.thread.clone(),
        operation_id: crate::model::new_id(),
    };
    let admission = NativePromptAdmission::new(
        || Ok(()),
        || {
            Box::pin(async {
                // Active maintenance is installed only after setup's final drain.
                if pre_send
                    && fx
                        .session
                        .active_turn_id_for_thread(&fx.owner.thread)
                        .is_some()
                {
                    assert!(
                        !fx.requests()
                            .iter()
                            .any(|f| f["method"] == "_x.ai/compact_conversation")
                    );
                    assert_eq!(
                        crate::acp_planner::test_seams::compact_incoming_len(&fx.owner.worker),
                        0,
                        "setup drain must finish before emitting the old live completion"
                    );
                    std::fs::write(fx.dir.path().join("grok-pre-send"), "!").unwrap();
                    tokio::time::timeout(Duration::from_secs(10), async {
                        while crate::acp_planner::test_seams::compact_incoming_len(&fx.owner.worker)
                            == 0
                        {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .unwrap();
                }
                Ok(())
            })
        },
    );
    assert!(matches!(
        fx.backend
            .compact_canonical(
                target.clone(),
                CompactRequest {
                    selection: TurnModelSelection::inherit(),
                },
                &admission,
                &fx.backend.handoff_for_test().await
            )
            .await
            .unwrap(),
        CompactSubmission::Submitted { .. }
    ));
    fx.wait_driver_finished().await;
    let observed = fx.backend.observe_maintenance(&target).await.unwrap();
    let Some(MaintenanceObservation::Turn { settled, .. }) = observed else {
        panic!("the original driver must retain this exact operation");
    };
    assert_eq!(settled, Some(!pre_send), "pre_send={pre_send}");
    assert_eq!(
        fx.requests()
            .iter()
            .filter(|f| f["method"] == "_x.ai/compact_conversation")
            .count(),
        1
    );
    fx.session.shutdown().await.unwrap();
}
