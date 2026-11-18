use super::*;
use crate::db::RepoOutOfDomain;

#[tokio::test]
async fn worker_model_defaults_claude_publication_captures_pair_and_clear() {
    let h = claude_worker_harness().await;
    let mut published = Vec::new();
    for (key, pair) in [
        ("a", Some(json!({"model":"model-a","effort":"high"}))),
        ("b", Some(json!({"model":"model-b","effort":"low"}))),
        ("c", None),
    ] {
        if let Some(pair) = &pair {
            h.repo
                .settings_upsert("worker_model_defaults_claude", &pair.to_string())
                .await
                .unwrap();
        } else {
            h.repo
                .settings_delete("worker_model_defaults_claude")
                .await
                .unwrap();
        }
        let (output, _, _) = prepare_claude_worker(&h, key).await;
        let card_id = output.output_string("card_id", "test").unwrap();
        let expected = json!({"provider":"claude", "selection": match pair {
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
    for (card, expected) in published {
        assert_eq!(
            h.repo.card_get(&card).await.unwrap().unwrap().payload["worker_model_selection"],
            expected
        );
    }
}
