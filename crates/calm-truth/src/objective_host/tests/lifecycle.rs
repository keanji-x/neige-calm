use super::*;

pub(super) async fn job(repo: &SqlxRepo) -> StoredIntent {
    for (i, event) in scenario().into_iter().take(3).enumerate() {
        fixture_react(repo, event, &format!("setup-{i}")).await;
    }
    unsettled_intents(repo.db())
        .await
        .unwrap()
        .into_iter()
        .find(|row| matches!(row.intent, Intent::RunCheck { .. }))
        .unwrap()
}
pub(super) fn finished(job: &StoredIntent) -> ObjectiveInput {
    let Intent::RunCheck {
        objective,
        evaluation,
        check,
        revision,
        sample,
    } = &job.intent
    else {
        panic!("expected run_check")
    };
    ObjectiveInput {
        track_id: job.id.track_id.clone(),
        source: InputSource::IntentJob {
            intent: job.id.clone(),
        },
        event: Event::CheckFinished {
            objective: objective.clone(),
            evaluation: *evaluation,
            check: check.clone(),
            revision: revision.clone(),
            sample: *sample,
            output: SampleOutput::Command(CommandOutput { exit_code: 0 }),
        },
    }
}
pub(super) async fn dispatch(repo: &SqlxRepo, id: &IntentId) {
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    intent_mark_dispatched_tx(&mut tx, id).await.unwrap();
    tx.commit().await.unwrap();
}
pub(super) async fn stored_settlement(repo: &SqlxRepo, id: &IntentId) -> IntentSettlement {
    let json: String = sqlx::query_scalar("SELECT settlement_json FROM objective_intents WHERE track_id=? AND event_seq=? AND ordinal=?")
        .bind(id.track_id.as_str()).bind(sql_seq(id.seq).unwrap()).bind(i64::from(id.ordinal))
        .fetch_one(repo.pool()).await.unwrap();
    serde_json::from_str(&json).unwrap()
}
fn conflict<T: std::fmt::Debug>(result: Result<T>) {
    assert!(
        matches!(
            result,
            Err(TruthError::Core(calm_types::error::CoreError::Conflict(_)))
        ),
        "{result:?}"
    );
}

#[tokio::test]
async fn lifecycle_transitions_have_typed_refusals() {
    let repo = fixture().await;
    let job = job(&repo).await;
    let mut missing = job.id.clone();
    missing.ordinal = 999;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert!(matches!(
        intent_mark_dispatched_tx(&mut tx, &missing).await,
        Err(TruthError::Core(calm_types::error::CoreError::NotFound(_)))
    ));
    assert!(matches!(
        intent_settle_tx(&mut tx, &missing, ExecutorSettlement::Executed).await,
        Err(TruthError::Core(calm_types::error::CoreError::NotFound(_)))
    ));
    tx.rollback().await.unwrap();
    dispatch(&repo, &job.id).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    conflict(intent_mark_dispatched_tx(&mut tx, &job.id).await);
    intent_settle_tx(
        &mut tx,
        &job.id,
        ExecutorSettlement::Failed {
            reason: "could not run".into(),
        },
    )
    .await
    .unwrap();
    conflict(intent_mark_dispatched_tx(&mut tx, &job.id).await);
    conflict(intent_settle_tx(&mut tx, &job.id, ExecutorSettlement::Executed).await);
    tx.commit().await.unwrap();
    assert_eq!(
        stored_settlement(&repo, &job.id).await,
        IntentSettlement::Failed {
            reason: "could not run".into()
        }
    );
    assert!(
        !unsettled_intents(repo.db())
            .await
            .unwrap()
            .iter()
            .any(|r| r.id == job.id)
    );
}

#[tokio::test]
async fn executor_settlements_are_durable_from_pending_or_dispatched() {
    for dispatched in [false, true] {
        for settlement in [
            ExecutorSettlement::Executed,
            ExecutorSettlement::Delivered,
            ExecutorSettlement::Superseded {
                reason: "session replaced".into(),
            },
            ExecutorSettlement::Failed {
                reason: "executor failure".into(),
            },
        ] {
            let repo = fixture().await;
            let job = job(&repo).await;
            if dispatched {
                dispatch(&repo, &job.id).await;
            }
            let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
            intent_settle_tx(&mut tx, &job.id, settlement.clone())
                .await
                .unwrap();
            tx.commit().await.unwrap();
            assert_eq!(
                serde_json::to_value(stored_settlement(&repo, &job.id).await).unwrap(),
                serde_json::to_value(settlement).unwrap()
            );
        }
    }
}

#[test]
fn executors_cannot_forge_reported() {
    let reported = serde_json::to_value(IntentSettlement::Reported {
        result_seq: LogSeq(4),
    })
    .unwrap();
    assert!(serde_json::from_value::<ExecutorSettlement>(reported).is_err());
}

#[tokio::test]
async fn reported_settlement_and_identical_retry() {
    let repo = fixture().await;
    let job = job(&repo).await;
    dispatch(&repo, &job.id).await;
    let input = finished(&job);
    let result = react(repo.db(), input.clone()).await.unwrap();
    assert!(matches!(
        result.reaction(),
        Reaction::Appended { seq: LogSeq(4), .. }
    ));
    assert_eq!(
        stored_settlement(&repo, &job.id).await,
        IntentSettlement::Reported {
            result_seq: LogSeq(4)
        }
    );
    assert!(
        !unsettled_intents(repo.db())
            .await
            .unwrap()
            .iter()
            .any(|r| r.id == job.id)
    );
    let before = counts(&repo).await;
    assert!(matches!(
        react(repo.db(), input.clone()).await.unwrap().reaction(),
        Reaction::Duplicate { seq: LogSeq(4) }
    ));
    let mut changed = input;
    if let Event::CheckFinished { output, .. } = &mut changed.event {
        *output = SampleOutput::Command(CommandOutput { exit_code: 1 });
    }
    conflict(react(repo.db(), changed).await);
    assert_eq!(counts(&repo).await, before);
    assert_eq!(
        replay_log(repo.db(), &"t".into()).await.unwrap().0,
        load_state(repo.db(), &"t".into()).await.unwrap().1
    );
}

#[tokio::test]
async fn check_finished_for_other_sample_is_stale() {
    let repo = fixture().await;
    let job = job(&repo).await;
    dispatch(&repo, &job.id).await;
    let mut input = finished(&job);
    if let Event::CheckFinished { sample, .. } = &mut input.event {
        *sample += 1;
    }
    let before = counts(&repo).await;
    conflict(react(repo.db(), input).await);
    assert_eq!(counts(&repo).await, before);
}

#[tokio::test]
async fn job_tuple_and_state_fences() {
    let repo = fixture().await;
    let job = job(&repo).await;
    let before = counts(&repo).await;
    conflict(react(repo.db(), finished(&job)).await); // pending
    dispatch(&repo, &job.id).await;
    for field in [
        "objective",
        "evaluation",
        "check",
        "revision",
        "track",
        "missing",
        "oversized",
        "notify",
    ] {
        let mut input = finished(&job);
        let Event::CheckFinished {
            objective,
            evaluation,
            check,
            revision,
            ..
        } = &mut input.event
        else {
            unreachable!()
        };
        match field {
            "objective" => *objective = ObjectiveKey::new("other"),
            "evaluation" => *evaluation = EvaluationId::new(9),
            "check" => *check = CheckId::new("other"),
            "revision" => *revision = Revision::new("other"),
            "track" => input.track_id = "other".into(),
            "missing" => {
                let InputSource::IntentJob { intent } = &mut input.source else {
                    unreachable!()
                };
                intent.ordinal = 999;
            }
            "oversized" => {
                let InputSource::IntentJob { intent } = &mut input.source else {
                    unreachable!()
                };
                intent.seq = LogSeq(u64::MAX);
            }
            "notify" => {
                let InputSource::IntentJob { intent } = &mut input.source else {
                    unreachable!()
                };
                intent.seq = LogSeq(1);
                intent.ordinal = 0;
                dispatch(&repo, intent).await;
            }
            _ => unreachable!(),
        }
        conflict(react(repo.db(), input).await);
    }
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    intent_settle_tx(&mut tx, &job.id, ExecutorSettlement::Executed)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    conflict(react(repo.db(), finished(&job)).await);
    assert_eq!(counts(&repo).await, before);
}

#[tokio::test]
async fn intent_job_authorization_matrix_is_closed() {
    let repo = fixture().await;
    let job = job(&repo).await;
    dispatch(&repo, &job.id).await;
    let mut events = scenario();
    events.push(Event::RevisionDelivered {
        revision: Revision::new("r1"),
    });
    for event in events {
        // Exhaustive decision: new engine variants must be considered here as well as in authorize.
        let allowed = match &event {
            Event::CheckFinished { .. } => true,
            Event::DefinitionChanged { .. }
            | Event::ApprovalGranted { .. }
            | Event::Requested { .. }
            | Event::RevisionDelivered { .. }
            | Event::Settled { .. }
            | Event::Closed { .. }
            | Event::Usage { .. }
            | Event::HostIdle { .. } => false,
        };
        let input = ObjectiveInput {
            event,
            ..finished(&job)
        };
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let result = react_tx(&mut tx, &input, 0).await;
        if allowed {
            assert!(result.is_ok(), "{result:?}");
        } else {
            assert!(
                matches!(result, Err(TruthError::Forbidden(_))),
                "{result:?}"
            );
        }
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn reported_settle_failure_rolls_back_append_state_and_intents() {
    let repo = fixture().await;
    let job = job(&repo).await;
    dispatch(&repo, &job.id).await;
    let before = counts(&repo).await;
    let state = load_state(repo.db(), &"t".into()).await.unwrap();
    sqlx::query(
        "CREATE TRIGGER refuse_report BEFORE UPDATE ON objective_intents
        WHEN NEW.status='settled' BEGIN SELECT RAISE(ABORT,'test failure'); END",
    )
    .execute(repo.pool())
    .await
    .unwrap();
    assert!(react(repo.db(), finished(&job)).await.is_err());
    assert_eq!(counts(&repo).await, before);
    assert_eq!(load_state(repo.db(), &"t".into()).await.unwrap(), state);
    assert!(
        unsettled_intents(repo.db())
            .await
            .unwrap()
            .iter()
            .any(|r| r.id == job.id)
    );
}

#[tokio::test]
async fn redefined_evaluation_is_logged_as_stale_and_reported() {
    let repo = fixture().await;
    let job = job(&repo).await;
    dispatch(&repo, &job.id).await;
    let mut definition = definition();
    definition.constraints.push("new standard".into());
    react(
        repo.db(),
        planner(
            Event::DefinitionChanged {
                objective: definition,
            },
            "redefine",
        ),
    )
    .await
    .unwrap();
    let reaction = react(repo.db(), finished(&job))
        .await
        .unwrap()
        .into_reaction();
    let Reaction::Appended { seq, intents } = reaction else {
        panic!("expected append")
    };
    assert!(intents.iter().any(|row| matches!(
        &row.intent,
        Intent::Notify {
            notice: objective::engine::Notice::EventIgnored {
                reason: objective::engine::IgnoreReason::StaleResult,
                ..
            }
        }
    )));
    assert_eq!(
        stored_settlement(&repo, &job.id).await,
        IntentSettlement::Reported { result_seq: seq }
    );
}
