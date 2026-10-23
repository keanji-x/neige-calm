use super::*;

async fn asked(repo: &SqlxRepo, track: &str, actor: &str, action: serde_json::Value) -> i64 {
    sqlx::query_scalar("INSERT INTO events(kind,scope_track,scope_area,actor,payload,at) VALUES('ask.requested',?,'a',?,?,0) RETURNING id")
        .bind(track)
        .bind(serde_json::json!({"kind":"AiPlanner","id":actor}).to_string())
        .bind(serde_json::json!({"track_id":track,"questions":[{"title":"批准？","options":["批准","不批准"]}],"delivery":"wake","action":action}).to_string())
        .fetch_one(repo.pool()).await.unwrap()
}
fn action() -> serde_json::Value {
    serde_json::json!({"kind":"objective_approval","objective":"build","standard_hash":standard_hash(&definition())})
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
    let mut wrong = action();
    wrong["standard_hash"] = serde_json::json!("different");
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
            serde_json::json!({"kind":"objective_approval","objective":"other","standard_hash":standard_hash(&definition())}),
        ),
        ("other", "foreign", action()),
        ("t", "w", action()),
        ("t", "foreign", action()),
        ("t", "p", serde_json::json!({"kind":"grant_admin"})),
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
    sqlx::query("INSERT INTO events(kind,scope_track,scope_area,actor,payload,at) VALUES('ask.answered','t','a','{\"kind\":\"User\"}',?,0)")
        .bind(serde_json::json!({"ask_id":id,"track_id":"t","answers":[{"option":0}]}).to_string()).execute(repo.pool()).await.unwrap();
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
