//! Ordinary transcript reads retain local-image redaction across every storage source.
use super::*;
use calm_server::db::sqlite::conversation::{
    PresentationSource, allocate_presentation_tx, import_item_tx,
};
use calm_server::model::NewCard;

#[tokio::test]
#[allow(deprecated)]
async fn owner_history_preserves_content_and_redacts_images_in_all_sources() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let fx = Fixture::with_state(with_repo(&fresh_state().await, repo.clone())).await;
    let (canonical, worker) = super::fixtures::canonical(&fx).await;
    let historical = fx
        .state
        .raw_repo()
        .card_create(NewCard {
            track_id: fx.track.clone().into(),
            kind: "codex".into(),
            sort: None,
            title: None,
            payload: json!({"harness_profile":"assistant","assistant_provider":"codex"}),
        })
        .await
        .unwrap()
        .id
        .to_string();
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    db.writer()
        .execute(sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1").bind(&historical))
        .await
        .unwrap();
    fx.state.write().role_cache().insert(
        historical.clone().into(),
        calm_server::model::CardRole::Assistant,
        fx.track.clone().into(),
    );
    for (card, source) in [
        (&historical, "historical_raw"),
        (&canonical, "native"),
        (&canonical, "imported_item"),
    ] {
        let params = json!({"item":{"type":"agentMessage","text":"Original speech", "content":[{"type":"localImage","path":"/private/image.png"}], "cwd":"/private/workspace", "diagnostic":"Original diagnostic"}});
        let segments =
            json!([{"presentation":"system","text":"Original system context","attachments":[]}]);
        let row = repo
            .fixture_transcript_insert(
                &worker,
                card,
                &fx.track,
                source,
                Some("turn"),
                Some(source),
                Some("agentMessage"),
                "item/completed",
                &params.to_string(),
                Some(&segments.to_string()),
            )
            .await
            .unwrap();
        if source != "historical_raw" {
            db.writer().with_writer(|session| Box::pin(async {
                let mut tx = session.begin_immediate().await.unwrap();
                if source == "native" {
                    allocate_presentation_tx(&mut tx, card, PresentationSource::NativeItem { row_id: row }).await.unwrap();
                } else {
                    let boundary: i64 = sqlx::query_scalar("SELECT MAX(sequence) FROM conversation_presentations WHERE card_id=?1 AND source_kind!='imported_item'").bind(card).fetch_one(&mut **tx).await.unwrap();
                    import_item_tx(&mut tx, card, row, boundary).await.unwrap().unwrap();
                }
                tx.commit().await.unwrap();
            })).await.unwrap();
        }
        for contract in if source == "historical_raw" {
            vec!["", "?contract=conversation_v1"]
        } else {
            vec!["?contract=conversation_v1"]
        } {
            let (status, page) = fx
                .request(
                    "GET",
                    &format!("/api/cards/{card}/harness/items{contract}"),
                    Some(&fx.cookie),
                    Value::Null,
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{page}");
            let rows: Vec<&Value> = if contract.is_empty() {
                page.as_array().unwrap().iter().collect()
            } else {
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|entry| entry["source"].get("item"))
                    .collect()
            };
            let item = rows.into_iter().find(|item| item["id"] == row).unwrap();
            let mut expected = params.clone();
            expected["item"]["content"][0]["path"] =
                calm_server::planner_attachments::REDACTED_LOCAL_IMAGE_PATH.into();
            let actual: Value = serde_json::from_str(item["params"].as_str().unwrap()).unwrap();
            assert_eq!(actual, expected, "{source} {contract}");
            assert_eq!(item["input_segments"], segments);
            assert!(!item.to_string().contains("/private/image.png"));
        }
    }
}
