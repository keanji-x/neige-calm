use super::*;

#[tokio::test]
async fn replay_equality() {
    let repo = fixture().await;
    let mut state = State::default();
    let mut expected = vec![];
    for (i, event) in scenario().into_iter().enumerate() {
        let (next, intents) = objective::react(&state, &event);
        state = next;
        for (ordinal, intent) in intents.into_iter().enumerate() {
            expected.push((
                IntentId {
                    track_id: "t".into(),
                    seq: LogSeq(i as u64 + 1),
                    ordinal: ordinal as u32,
                },
                intent,
            ));
        }
        fixture_react(&repo, event, &i.to_string()).await;
    }
    let (seq, snapshot) = load_state(repo.db(), &"t".into()).await.unwrap();
    assert_eq!(seq, LogSeq(9));
    assert_eq!(snapshot, state);
    assert_eq!(
        state.compactions, 1,
        "scenario must exercise a durable Execute"
    );
    assert!(
        expected
            .iter()
            .any(|(_, intent)| matches!(intent, Intent::RunCheck { .. }))
    );
    let target: String =
        sqlx::query_scalar("SELECT target_session_id FROM objective_intents WHERE kind='execute'")
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(target, "p");
    assert_eq!(
        replay_log(repo.db(), &"t".into()).await.unwrap(),
        (state, expected.clone())
    );
    let stored: Vec<(i64, i64, String)> = sqlx::query_as(
        "SELECT event_seq,ordinal,intent_json FROM objective_intents ORDER BY event_seq,ordinal",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap();
    let actual: Vec<_> = stored
        .into_iter()
        .map(|(seq, ordinal, json)| {
            (
                IntentId {
                    track_id: "t".into(),
                    seq: LogSeq(seq as u64),
                    ordinal: ordinal as u32,
                },
                serde_json::from_str::<Intent>(&json).unwrap(),
            )
        })
        .collect();
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn version_bump_rebuild_keeps_intents() {
    let repo = fixture().await;
    for (i, event) in scenario().into_iter().enumerate() {
        fixture_react(&repo, event, &i.to_string()).await;
    }
    let before = load_state(repo.db(), &"t".into()).await.unwrap();
    let n = counts(&repo).await;
    sqlx::query("UPDATE objective_states SET state_version=0,state_json='{}'")
        .execute(repo.pool())
        .await
        .unwrap();
    assert_eq!(load_state(repo.db(), &"t".into()).await.unwrap(), before);
    assert_eq!(counts(&repo).await, n);
    react(repo.db(), harness(Event::HostIdle { queue_len: 1 }, "new"))
        .await
        .unwrap();
    assert_eq!(counts(&repo).await, (n.0 + 1, n.1, n.2));
    let current: i64 = sqlx::query_scalar("SELECT state_version FROM objective_states")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(current, i64::from(objective::engine::STATE_VERSION));
}

#[tokio::test]
async fn snapshot_seq_tracks_log_tip() {
    let repo = fixture().await;
    fixture_input_react(
        &repo,
        planner(
            Event::DefinitionChanged {
                objective: definition(),
            },
            "first",
        ),
    )
    .await
    .unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert!(
        snapshot::store_tx(
            &mut tx,
            &"t".into(),
            LogSeq(0),
            LogSeq(2),
            &State::default()
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert!(
        snapshot::store_tx(
            &mut tx,
            &"t".into(),
            LogSeq(99),
            LogSeq(1),
            &State::default()
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        load_state(repo.db(), &"t".into()).await.unwrap().0,
        LogSeq(1)
    );
}

#[tokio::test]
async fn corrupt_log_row_fails_closed() {
    let repo = fixture().await;
    fixture_input_react(
        &repo,
        planner(
            Event::DefinitionChanged {
                objective: definition(),
            },
            "first",
        ),
    )
    .await
    .unwrap();
    sqlx::query("DROP TRIGGER objective_events_no_update")
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE objective_events SET payload_json=json_set(payload_json,'$.objective',json('{\"key\":\"build\"}'))",
    )
    .execute(repo.pool())
    .await
    .unwrap();
    assert!(replay_log(repo.db(), &"t".into()).await.is_err());
    sqlx::query("UPDATE objective_states SET state_version=0")
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(load_state(repo.db(), &"t".into()).await.is_err());
    assert!(
        react(repo.db(), harness(Event::HostIdle { queue_len: 0 }, "new"))
            .await
            .is_err()
    );
    assert_eq!(counts(&repo).await, (1, 1, 1));
}

#[tokio::test]
async fn snapshot_corruption_and_unknown_event_version_fail_closed() {
    let repo = fixture().await;
    fixture_input_react(
        &repo,
        planner(
            Event::DefinitionChanged {
                objective: definition(),
            },
            "first",
        ),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE objective_states SET state_json='{}'")
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(load_state(repo.db(), &"t".into()).await.is_err());
    sqlx::query("DROP TRIGGER objective_events_no_update")
        .execute(repo.pool())
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    sqlx::query("PRAGMA ignore_check_constraints=ON")
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("UPDATE objective_events SET schema_version=999")
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("PRAGMA ignore_check_constraints=OFF")
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(replay_log(repo.db(), &"t".into()).await.is_err());
}

#[tokio::test]
async fn track_delete_cascades_both_paths() {
    for area in [false, true] {
        let repo = fixture().await;
        fixture_input_react(
            &repo,
            planner(
                Event::DefinitionChanged {
                    objective: definition(),
                },
                "first",
            ),
        )
        .await
        .unwrap();
        assert_eq!(counts(&repo).await, (1, 1, 1));
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        if area {
            area_delete_tx(&mut tx, "a").await.unwrap();
        } else {
            track_delete_tx(&mut tx, "t", repo.track_area_cache())
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        assert_eq!(counts(&repo).await, (0, 0, 0));
    }
}
