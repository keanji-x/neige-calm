use super::*;
use crate::db::{RepoOutOfDomain, RepoRead};

#[tokio::test]
async fn worker_model_defaults_publication_freezes_pair_and_clear_only_affects_new_workers() {
    let h = worker_lease_harness_with_disk(true).await;
    let mut published = Vec::new();
    for (key, pair) in [
        ("a", Some(json!({"model":"model-a","effort":"high"}))),
        ("b", Some(json!({"model":"model-b","effort":"low"}))),
        ("c", None),
    ] {
        // Queue before the setting write: only publication decides the selection.
        let op = claim_worker_operation(&h, key, key).await;
        if let Some(pair) = &pair {
            h.repo
                .settings_upsert("worker_model_defaults_codex", &pair.to_string())
                .await
                .unwrap();
        } else {
            h.repo
                .settings_delete("worker_model_defaults_codex")
                .await
                .unwrap();
        }
        let output =
            crate::operation::tests::prepare_claimed_adapter(h.repo.clone(), &h.adapter, &op)
                .await
                .unwrap();
        let card_id = output.output_string("card_id", "test").unwrap();
        let expected = json!({"provider":"codex", "selection": match pair {
            Some(value) => json!({"kind":"explicit","value":value}),
            None => json!({"kind":"inherit"})
        }});
        assert_eq!(output.result["payload"]["worker_model_selection"], expected);
        published.push((card_id.clone(), expected));
        release_workspace_lease_for_card_repo(
            h.repo.as_ref(),
            &h.events,
            &card_id,
            ReleaseDelivery::CommitWorkerMessage(
                CommitMessage::parse("test: release worker defaults fixture").unwrap(),
            ),
        )
        .await
        .unwrap();
        crate::worker_model_defaults::tests::retire_publication_fixture(
            h.repo.as_ref(),
            format!("{}:{key}", h.track_id),
            card_id,
        )
        .await;
    }
    let reopened = crate::db::sqlite::SqlxRepo::open(&format!(
        "sqlite:{}",
        h.repo_root.path().join(".git/test.sqlite").display()
    ))
    .await
    .unwrap();
    for (card, expected) in published {
        assert_eq!(
            reopened.card_get(&card).await.unwrap().unwrap().payload["worker_model_selection"],
            expected
        );
    }
}

#[tokio::test]
async fn worker_model_defaults_corrupt_setting_rolls_back_publication() {
    let h = worker_lease_harness().await;
    h.repo
        .settings_upsert("worker_model_defaults_codex", "broken")
        .await
        .unwrap();
    assert!(try_prepare_worker_and_op(&h, "bad", "bad").await.is_err());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions")
        .fetch_one(h.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}
