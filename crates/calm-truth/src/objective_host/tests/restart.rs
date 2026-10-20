use super::lifecycle::{dispatch, finished, job, stored_settlement};
use super::*;

#[tokio::test]
async fn restarts_preserve_state_ordered_intents_and_dispatched_results() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("restart.db").display()
    );
    let repo = fixture_at(&url).await;
    let job = job(&repo).await;
    dispatch(&repo, &job.id).await;
    react(repo.db(), harness(scenario()[5].clone(), "usage"))
        .await
        .unwrap();
    // A second track pins the leading sort key independently of event_seq.
    let mut input = planner(
        Event::DefinitionChanged {
            objective: definition(),
        },
        "foreign",
    );
    input.track_id = "other".into();
    input.source = InputSource::Planner {
        principal: Principal::Agent {
            session_id: "foreign".into(),
            track_id: "other".into(),
            area_id: "a".into(),
        },
        request_id: PlannerRequestId::new("foreign"),
    };
    react(repo.db(), input).await.unwrap();
    let before = load_state(repo.db(), &"t".into()).await.unwrap();
    assert_eq!(before.1.usage.turns, 2);
    let rows = unsettled_intents(repo.db()).await.unwrap();
    assert_eq!(rows.first().unwrap().id.track_id.as_str(), "other");
    assert!(rows.windows(2).all(
        |w| (w[0].id.track_id.as_str(), w[0].id.seq.0, w[0].id.ordinal)
            < (w[1].id.track_id.as_str(), w[1].id.seq.0, w[1].id.ordinal)
    ));
    repo.db().close().await;
    let repo = SqlxRepo::open(&url).await.unwrap();
    assert_eq!(load_state(repo.db(), &"t".into()).await.unwrap(), before);
    assert_eq!(unsettled_intents(repo.db()).await.unwrap(), rows);
    let Reaction::Appended { seq, .. } = react(repo.db(), finished(&job))
        .await
        .unwrap()
        .into_reaction()
    else {
        panic!("expected append")
    };
    assert_eq!(
        stored_settlement(&repo, &job.id).await,
        IntentSettlement::Reported { result_seq: seq }
    );
    react(repo.db(), planner(scenario()[4].clone(), "settle-0"))
        .await
        .unwrap();
    let pending = load_state(repo.db(), &"t".into()).await.unwrap();
    assert!(pending.1.pending_compact);
    repo.db().close().await;
    let repo = SqlxRepo::open(&url).await.unwrap();
    assert_eq!(load_state(repo.db(), &"t".into()).await.unwrap(), pending);
    compact(&repo, "idle-0", "compact:0").await;
    let mut request = scenario()[2].clone();
    if let Event::Requested { revision, .. } = &mut request {
        *revision = Revision::new("r1");
    }
    react(repo.db(), planner(request, "request-1"))
        .await
        .unwrap();
    let next_job = unsettled_intents(repo.db()).await.unwrap().into_iter()
        .find(|row| matches!(row.intent, Intent::RunCheck { evaluation, .. } if evaluation == EvaluationId::new(1))).unwrap();
    dispatch(&repo, &next_job.id).await;
    react(repo.db(), finished(&next_job)).await.unwrap();
    let mut settle = scenario()[4].clone();
    if let Event::Settled { evaluation, .. } = &mut settle {
        *evaluation = EvaluationId::new(1);
    }
    react(repo.db(), planner(settle, "settle-1")).await.unwrap();
    let pending = load_state(repo.db(), &"t".into()).await.unwrap();
    assert!(pending.1.pending_compact);
    assert_eq!(pending.1.compactions, 1);
    repo.db().close().await;
    let repo = SqlxRepo::open(&url).await.unwrap();
    assert_eq!(load_state(repo.db(), &"t".into()).await.unwrap(), pending);
    compact(&repo, "idle-1", "compact:1").await;
    assert_eq!(
        load_state(repo.db(), &"t".into())
            .await
            .unwrap()
            .1
            .compactions,
        2
    );
    sqlx::query("UPDATE objective_states SET state_json='{}' WHERE track_id='t'")
        .execute(repo.pool())
        .await
        .unwrap();
    repo.db().close().await;
    let repo = SqlxRepo::open(&url).await.unwrap();
    assert!(matches!(
        load_state(repo.db(), &"t".into()).await,
        Err(TruthError::Internal(_))
    ));
    repo.db().close().await;
}

async fn compact(repo: &SqlxRepo, turn: &str, expected_key: &str) {
    let Reaction::Appended { intents, .. } =
        react(repo.db(), harness(Event::HostIdle { queue_len: 0 }, turn))
            .await
            .unwrap()
            .into_reaction()
    else {
        panic!("expected append")
    };
    let execute = intents
        .iter()
        .find(|row| matches!(row.intent, Intent::Execute { .. }))
        .unwrap();
    let Intent::Execute { call } = &execute.intent else {
        unreachable!()
    };
    assert_eq!(call.idempotency_key(), expected_key);
    assert_eq!(
        execute.effect,
        StoredEffect::Execute {
            target_session_id: "p".into()
        }
    );
}
