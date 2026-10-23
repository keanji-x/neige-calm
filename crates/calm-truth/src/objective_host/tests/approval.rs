use super::*;

use calm_types::event::{
    AskAction, AskAnswer, AskDelivery, AskQuestion, Event as TrackEvent, EventScope,
};
use calm_types::ids::ActorId;

async fn asked(repo: &SqlxRepo, track: &str, actor: &str, action: AskAction) -> i64 {
    // Replay the exact provenance under test, including invalid role/track bindings that
    // the live role gate would reject before objective approval can inspect them.
    repo.event_append_fixture(
        ActorId::AiPlanner(actor.into()),
        EventScope::Track {
            track: track.into(),
            area: "a".into(),
        },
        None,
        &TrackEvent::AskRequested {
            track_id: track.into(),
            questions: vec![AskQuestion {
                title: "批准？".into(),
                options: vec!["批准".into(), "不批准".into()],
            }],
            delivery: AskDelivery::Wake,
            action: Some(action),
            source_item_id: None,
        },
    )
    .await
    .unwrap()
}
fn action() -> AskAction {
    AskAction::ObjectiveApproval {
        objective: "build".into(),
        standard_hash: standard_hash(&definition()).as_str().into(),
    }
}
fn input(id: i64) -> ObjectiveInput {
    ObjectiveInput {
        track_id: "t".into(),
        event: scenario()[1].clone(),
        source: serde_json::from_value(serde_json::json!({"source":"ask_answered","ask_id":id}))
            .unwrap(),
    }
}

#[tokio::test]
async fn objective_approval_accepts_duplicate_and_replays() {
    let repo = fixture().await;
    fixture_react(&repo, scenario()[0].clone(), "define").await;
    let id = asked(&repo, "t", "p", action()).await;
    assert!(matches!(
        react(repo.db(), input(id)).await.unwrap().reaction(),
        Reaction::Appended { .. }
    ));
    assert!(matches!(
        react(repo.db(), input(id)).await.unwrap().reaction(),
        Reaction::Duplicate { .. }
    ));
    let (_, state) = load_state(repo.db(), &"t".into()).await.unwrap();
    assert!(state.objectives[&definition().key].is_approved());
    assert_eq!(replay_log(repo.db(), &"t".into()).await.unwrap().0, state);
    assert_eq!(read_state(repo.pool(), &"t".into()).await.unwrap().1, state);
}

#[tokio::test]
async fn objective_approval_hash_mismatch_is_forbidden() {
    let repo = fixture().await;
    let wrong = AskAction::ObjectiveApproval {
        objective: "build".into(),
        standard_hash: "different".into(),
    };
    let id = asked(&repo, "t", "p", wrong).await;
    assert!(matches!(
        react(repo.db(), input(id)).await,
        Err(TruthError::Forbidden(_))
    ));
    assert_eq!(counts(&repo).await, (0, 0, 0));
}

#[tokio::test]
async fn objective_approval_rejects_unbound_sources() {
    let repo = fixture().await;
    for (track, actor, action) in [
        (
            "t",
            "p",
            AskAction::ObjectiveApproval {
                objective: "other".into(),
                standard_hash: standard_hash(&definition()).as_str().into(),
            },
        ),
        ("other", "foreign", action()),
        ("t", "w", action()),
        ("t", "foreign", action()),
        ("t", "p", AskAction::GrantAdmin),
    ] {
        let id = asked(&repo, track, actor, action).await;
        assert!(matches!(
            react(repo.db(), input(id)).await,
            Err(TruthError::Forbidden(_))
        ));
    }
    for id in [i64::MAX, 0, -1] {
        assert!(matches!(
            react(repo.db(), input(id)).await,
            Err(TruthError::Forbidden(_))
        ));
    }
    let id = asked(&repo, "t", "p", action()).await;
    repo.event_append_fixture(
        ActorId::User,
        EventScope::Track {
            track: "t".into(),
            area: "a".into(),
        },
        None,
        &TrackEvent::AskAnswered {
            ask_id: id,
            track_id: "t".into(),
            answers: vec![AskAnswer::Option(0)],
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        react(repo.db(), input(id)).await,
        Err(TruthError::Forbidden(_))
    ));
    let mut unbound = input(id);
    unbound.source = InputSource::IntentJob {
        intent: IntentId {
            track_id: "t".into(),
            seq: LogSeq(1),
            ordinal: 0,
        },
    };
    assert!(matches!(
        react(repo.db(), unbound).await,
        Err(TruthError::Forbidden(_))
    ));
    assert_eq!(counts(&repo).await, (0, 0, 0));
}
