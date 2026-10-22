use super::*;

#[tokio::test]
async fn objective_turn_usage_accumulates_and_replay_is_duplicate() {
    let repo = fixture().await;
    for (turn, growth) in [("one", 10000), ("two", 5000)] {
        assert!(matches!(
            record_turn_usage(
                repo.db(),
                &"t".into(),
                "p".into(),
                turn.into(),
                TurnUsage {
                    context_growth: growth,
                    secs: 7
                }
            )
            .await
            .unwrap(),
            TurnUsageResult::Recorded(Reaction::Appended { .. })
        ));
    }
    assert!(matches!(
        record_turn_usage(
            repo.db(),
            &"t".into(),
            "p".into(),
            "one".into(),
            TurnUsage {
                context_growth: 10000,
                secs: 7
            }
        )
        .await
        .unwrap(),
        TurnUsageResult::Recorded(Reaction::Duplicate { .. })
    ));
    let (_, state) = read_state(repo.db().read_pool(), &"t".into())
        .await
        .unwrap();
    assert_eq!(
        state.usage,
        Usage {
            context_tokens: 15000,
            turns: 2,
            elapsed_secs: 14
        }
    );
    assert_eq!(counts(&repo).await, (2, 1, 0));
    // Version mismatch is replayed without writing the old snapshot back.
    sqlx::query("UPDATE objective_states SET state_version=999")
        .execute(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        read_state(repo.db().read_pool(), &"t".into())
            .await
            .unwrap()
            .1,
        state
    );
    let version: i64 = sqlx::query_scalar("SELECT state_version FROM objective_states")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(version, 999);
}

#[tokio::test]
async fn objective_turn_usage_without_capability_skips_every_row() {
    let repo = fixture_with_capability("sqlite::memory:", false).await;
    assert_eq!(
        record_turn_usage(
            repo.db(),
            &"t".into(),
            "p".into(),
            "one".into(),
            TurnUsage {
                context_growth: 10000,
                secs: 7
            }
        )
        .await
        .unwrap(),
        TurnUsageResult::Skipped
    );
    assert_eq!(counts(&repo).await, (0, 0, 0));
}

#[tokio::test]
async fn objective_turn_usage_retains_selected_planner_authorization() {
    let repo = fixture().await;
    for session in ["a", "w", "foreign", "missing"] {
        assert!(
            record_turn_usage(
                repo.db(),
                &"t".into(),
                session.into(),
                "one".into(),
                TurnUsage {
                    context_growth: 1,
                    secs: 1
                }
            )
            .await
            .is_err()
        );
    }
    assert_eq!(counts(&repo).await, (0, 0, 0));
}

#[tokio::test]
async fn objective_read_state_replays_sources_without_writing() {
    let repo = fixture().await;
    assert_eq!(
        read_state(repo.db().read_pool(), &"t".into())
            .await
            .unwrap(),
        (LogSeq(0), State::default())
    );
    for (index, event) in scenario().into_iter().enumerate() {
        fixture_react(&repo, event, &index.to_string()).await;
    }
    let expected = load_state(repo.db(), &"t".into()).await.unwrap();
    let before = counts(&repo).await;
    sqlx::query("UPDATE objective_states SET state_version=999")
        .execute(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        read_state(repo.db().read_pool(), &"t".into())
            .await
            .unwrap(),
        expected
    );
    assert_eq!(counts(&repo).await, before);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT state_version FROM objective_states")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        999
    );
}
