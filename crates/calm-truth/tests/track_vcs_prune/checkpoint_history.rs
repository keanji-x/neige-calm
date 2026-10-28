use super::*;
use calm_types::harness::snapshot::CanonicalHarnessSnapshot;

#[tokio::test]
async fn current_owner_checkpoint_prunes_only_before_oldest_endpoint() {
    let fixture = fixture_with_commits(7).await;
    let mut frame = CanonicalHarnessSnapshot::dormant(0);
    frame.header.last_seen_head = Some(fixture.commits[3].hash.clone());
    frame.header.issued_turn_head = Some(fixture.commits[1].hash.clone());
    let snapshot = serde_json::to_string(&frame).unwrap();
    insert_active_session(
        fixture.pool(),
        &fixture.track_id,
        "current",
        Some(&snapshot),
    )
    .await;
    let before = track_vcs::diff(
        fixture.pool(),
        &fixture.commits[1].hash,
        &fixture.commits[6].hash,
        None,
    )
    .await
    .unwrap();
    assert_eq!(prune_once(fixture.db(), &fixture.track_id, 1).await, 1);
    for (index, commit) in fixture.commits.iter().enumerate() {
        assert_eq!(
            commit_exists(fixture.pool(), &commit.hash).await,
            index >= 1
        );
    }
    assert!(
        track_vcs::sweep_unreferenced_objects_once(fixture.db())
            .await
            .unwrap()
            > 0
    );
    for commit in &fixture.commits[1..] {
        assert!(object_exists(fixture.pool(), &commit.tree_hash).await);
        assert!(object_exists(fixture.pool(), &commit.blob_hash).await);
    }
    assert_eq!(
        track_vcs::diff(
            fixture.pool(),
            &fixture.commits[1].hash,
            &fixture.commits[6].hash,
            None
        )
        .await
        .unwrap(),
        before
    );
}

fn released_frame(
    version: u32,
    last_seen: Option<&str>,
    issued: Option<&str>,
) -> serde_json::Value {
    let mut frame = CanonicalHarnessSnapshot::dormant(0);
    frame.header.last_seen_head = last_seen.map(str::to_owned);
    frame.header.issued_turn_head = issued.map(str::to_owned);
    let mut value = serde_json::to_value(frame).unwrap();
    value["schema_version"] = json!(version);
    if version < 4 {
        value["canonical_inputs"]
            .as_object_mut()
            .unwrap()
            .remove("receipts");
    }
    value
}

#[tokio::test]
async fn released_owner_checkpoints_preserve_either_endpoint_and_keep_n() {
    for version in [2, 3, 4, 5] {
        for (last_seen, issued, floor) in [
            (Some(1), Some(3), 1),
            (Some(3), Some(1), 1),
            (None, Some(2), 2),
            (Some(2), None, 2),
            (None, None, 4),
        ] {
            let fixture = fixture_with_commits(6).await;
            let value = released_frame(
                version,
                last_seen.map(|i| fixture.commits[i].hash.as_str()),
                issued.map(|i| fixture.commits[i].hash.as_str()),
            );
            // The declared owner conversion, never a second codec in this fixture.
            CanonicalHarnessSnapshot::from_released(value.clone()).unwrap();
            insert_active_session(
                fixture.pool(),
                &fixture.track_id,
                "released",
                Some(&value.to_string()),
            )
            .await;
            assert_eq!(
                prune_once(fixture.db(), &fixture.track_id, 2).await,
                floor as u64,
                "V{version}"
            );
            for (i, commit) in fixture.commits.iter().enumerate() {
                assert_eq!(
                    commit_exists(fixture.pool(), &commit.hash).await,
                    i >= floor,
                    "V{version}: {i}"
                );
            }
        }
    }
}

#[tokio::test]
async fn invalid_owner_checkpoints_and_missing_endpoints_skip_prune() {
    for version in [2, 3, 4, 5] {
        let valid = released_frame(version, None, None);
        let mut cases = Vec::new();
        for field in ["canonical_inputs", "turn_budget", "push_watermark"] {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(field);
            cases.push(missing);
        }
        for (field, value) in [
            ("schema_version", json!(99)),
            ("mode", json!("worker")),
            ("last_seen_head", json!(123)),
            ("issued_turn_head", json!([])),
            ("last_seen_head", json!("absent-commit")),
            ("issued_turn_head", json!("absent-commit")),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            cases.push(invalid);
        }
        for (index, value) in cases.into_iter().enumerate() {
            let fixture = fixture_with_commits(5).await;
            insert_active_session(
                fixture.pool(),
                &fixture.track_id,
                "invalid",
                Some(&value.to_string()),
            )
            .await;
            assert_eq!(
                prune_once(fixture.db(), &fixture.track_id, 1).await,
                0,
                "V{version} case {index}"
            );
            assert_eq!(commit_count(fixture.pool(), &fixture.track_id).await, 5);
        }
    }
}

#[tokio::test]
async fn current_checkpoint_prune_dry_run_rolls_back_before_reclamation() {
    let fixture = fixture_with_commits(6).await;
    let value = released_frame(5, None, None);
    insert_active_session(
        fixture.pool(),
        &fixture.track_id,
        "dry-run",
        Some(&value.to_string()),
    )
    .await;
    let objects = object_count(fixture.pool()).await;
    let mut tx = begin_immediate_tx(fixture.db()).await.unwrap();
    assert_eq!(
        track_vcs::prune_track_history_tx(&mut tx, &fixture.track_id, 2)
            .await
            .unwrap(),
        4
    );
    tx.rollback().await.unwrap();
    assert_eq!(commit_count(fixture.pool(), &fixture.track_id).await, 6);
    assert_eq!(object_count(fixture.pool()).await, objects);
}
