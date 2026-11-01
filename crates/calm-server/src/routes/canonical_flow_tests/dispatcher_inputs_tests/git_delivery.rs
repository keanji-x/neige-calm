//! Canonical settlement acceptance must never reacquire the single writer lease.
use super::*;
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use calm_truth::db::RepoEventWrite as _;
use calm_types::git_candidate::{DeliverySettlement, DeliveryWakeReason};

#[tokio::test]
async fn git_settlement_canonical_acceptance_replay_and_writer_progress() {
    let mut flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let (task, event, id, retained_path, result) = settlement(&flow).await;
    // Hold the qualification while it owns the outer writer, then deterministically
    // poll a second writer into the wait queue before releasing it.
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("git-settlement-qualification", &task, pause.clone());
    let dispatch = flow.state.dispatcher.clone();
    let track = flow.track.clone();
    let incoming = event.clone();
    let accepting = tokio::spawn(async move {
        dispatch.catch_up_push(track.into(), incoming, id).await;
    });
    // Before the fix, the nested acquisition blocks before reaching this seam.
    let reached = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()).await;
    if reached.is_err() {
        accepting.abort();
        panic!("canonical settlement qualification blocked acquiring a nested writer");
    }

    {

        {
            let writer = async {
                let write_db = (flow.fixture.repo.db()).clone();
        write_db.writer().with_writer(|write_session_1| Box::pin(async move {
let mut tx = write_session_1.begin()
                    .await
                    .unwrap();
                sqlx::query("UPDATE tracks SET title='writer progressed' WHERE id=?1")
                    .bind(&flow.track)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();

        })).await.expect("independent write operation")
};
            tokio::pin!(writer);
            assert!(
                futures::poll!(writer.as_mut()).is_pending(),
                "the outer writer must still own its lease"
            );
            pause.release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(async { accepting.await.unwrap() }, writer.as_mut());
            })
            .await
            .expect("acceptance and the queued short writer must progress without acquisition timeout");
        }
        let before = flow.fixture.carrier(&flow.track).await.2;
        assert_eq!(before.header.push_watermark, id);
        let [
            PendingInput::System {
                observation,
                envelope_id,
                ..
            },
        ] = before.canonical_inputs.pending.as_slice()
        else {
            panic!(
                "exactly one settlement must be accepted: {:?}",
                before.canonical_inputs
            );
        };
        assert_eq!(*envelope_id, Some(id));
        assert!(
            matches!(observation.observation(), crate::harness::Observation::TaskGitDeliverySettled {
            key, attempt_id, result: actual, retained_path: Some(path),
        } if key == "deliver" && attempt_id == &task && actual == &result && path == retained_path.to_str().unwrap())
        );
        // Replay has no new prefix: only the already accepted pending System is requalified.
        tokio::time::timeout(
            Duration::from_secs(5),
            flow.state
                .dispatcher
                .catch_up_push(flow.track.clone().into(), event, id),
        )
        .await
        .expect("pending settlement requalification must reuse the outer writer");
        tokio::time::timeout(
            Duration::from_secs(5),
            flow.state.dispatcher.reconcile_tick_for_test(),
        )
        .await
        .expect("reconciliation must not wait for a nested writer timeout");
        assert_eq!(
            flow.fixture.carrier(&flow.track).await.2.canonical_inputs,
            before.canonical_inputs
        );
        flow.end(TURN).await;
        let call = flow
            .next()
            .await
            .expect("accepted settlement must reach the original Planner");
        assert!(call["params"]["input"].to_string().contains("deliver"));
        flow.accept(&call, "settlement-turn").await;
        flow.end("settlement-turn").await;
        flow.fixture
            .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
            .await;
        flow.close().await;


}
}

async fn settlement(flow: &Flow) -> (String, Event, i64, std::path::PathBuf, DeliverySettlement) {
    let task = crate::model::new_id();
    (flow.fixture.repo.db()).writer().execute(sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,created_at_ms,updated_at_ms) VALUES(?1,?2,'deliver','codex','Deliver','{}','done','delivery-worker',1,1)")
        .bind(&task).bind(&flow.track)).await.unwrap();
    let retained_path = flow.fixture.root.path().join("retained-worker");
    let write_db = (flow.fixture.repo.db()).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin().await.unwrap();
                crate::operation::workspace_lease::acquire_plain_workspace_lease_tx(
                    &mut tx,
                    "delivery-worker",
                    &flow.track,
                    "canonical-delivery",
                    &retained_path,
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
                let result = DeliverySettlement::Candidate {
                    candidate_id: "settled-delivery".into(),
                    commit_sha: "c".repeat(40),
                    base_sha: "b".repeat(40),
                    base_is_ancestor: true,
                };
                let event = Event::TaskGitDeliverySettled {
                    task_id: task.clone(),
                    idempotency_key: task.clone(),
                    track_id: flow.track.clone().into(),
                    card_id: "delivery-worker".into(),
                    delivery_id: "settled-delivery".into(),
                    ordinal: 1,
                    result: result.clone(),
                    wake_reason: DeliveryWakeReason::UngatedCandidate,
                };
                let id = flow
                    .fixture
                    .repo
                    .log_pure_event(
                        ActorId::KernelDispatcher,
                        EventScope::Track {
                            track: flow.track.clone().into(),
                            area: flow.area.clone().into(),
                        },
                        None,
                        &flow.state.events,
                        &flow.state.card_role_cache,
                        &flow.state.track_area_cache,
                        event.clone(),
                    )
                    .await
                    .unwrap();
                (task, event, id, retained_path, result)
            })
        })
        .await
        .expect("independent write operation")
}

#[tokio::test]
async fn git_settlement_canonical_qualification_and_watermark_share_outer_transaction() {
    let mut flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let (task, event, id, _, _) = settlement(&flow).await;
    let before = flow.fixture.carrier(&flow.track).await.2;
    let write_db = (flow.fixture.repo.db()).clone();
    write_db.writer().with_writer(|write_session| Box::pin(async move {
        let mut tx = write_session.begin()
            .await
            .unwrap();
        sqlx::query("UPDATE tasks SET worker_card_id='uncommitted-worker' WHERE id=?1")
            .bind(&task)
            .execute(&mut **tx)
            .await
            .unwrap();
        let carrier = tokio::time::timeout(
            Duration::from_secs(5),
            flow.state
                .dispatcher
                .accept_event_prefix_for_test(&mut tx, &flow.card, Some((&event, id))),
        )
        .await
        .expect("qualification must use the caller's transaction")
        .unwrap();
        assert_eq!(carrier.snapshot()["push_watermark"], id);
        assert!(
            matches!(carrier.checkpoint().pending.as_slice(),
                [PendingInput::System { observation, .. }]
                if matches!(observation.observation(), crate::harness::Observation::TaskGitDeliverySettled { attempt_id, retained_path: None, .. } if attempt_id == &task)
            ),
            "qualification must see the outer transaction's task/workspace facts"
        );
        tx.rollback().await.unwrap();
        let after = flow.fixture.carrier(&flow.track).await.2;
        assert_eq!(after.canonical_inputs, before.canonical_inputs);
        assert_eq!(after.header.push_watermark, before.header.push_watermark);
        flow.state.dispatcher.retire();
        flow.end(TURN).await;
        flow.fixture
            .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
            .await;
        flow.close().await;

    })).await.expect("independent write operation")
}
