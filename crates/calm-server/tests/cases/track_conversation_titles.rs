//! Fresh reads exercise server title metadata, without the history REST endpoint.
use super::*;
use calm_truth::db::RepoRead;

#[tokio::test]
async fn conversation_title_planner_fresh_detail() {
    let b = boot().await;
    let track = b.create_track("unrelated Track name").await;
    let card: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(&track)
            .fetch_one(b.repo.db().read_pool())
            .await
            .unwrap();
    b.repo.fixture_transcript_insert("title-runtime", &card, &track, "title-thread", None, None,
        Some("userMessage"), "item/completed",
        &json!({"item":{"content":[{"type":"text","text":"Original planner title\nsecond line"}]}}).to_string(), None).await.unwrap();
    let (status, body) = b
        .request("GET", &format!("/api/tracks/{track}"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let planner = body["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == card)
        .unwrap();
    assert_eq!(planner["title"], "Original planner title");
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn conversation_title_assistant_fresh_list() {
    let b = boot().await;
    let track = b.create_track("unrelated Track name").await;
    let (status, created) = b
        .create_conversation(
            &track,
            "title-assistant",
            "Original assistant title\nsecond line",
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let (status, body) = b.list_conversations(&track).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body.as_array().unwrap()[0]["title"],
        "Original assistant title"
    );
    b.shutdown_harnesses().await;
}

async fn planner(b: &Boot, track: &str) -> String {
    sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
        .bind(track)
        .fetch_one(b.repo.db().read_pool())
        .await
        .unwrap()
}

async fn input(b: &Boot, track: &str, card: &str, segments: Option<Value>, text: &str) -> i64 {
    b.repo
        .fixture_transcript_insert(
            "retired-title-runtime",
            card,
            track,
            "title-thread",
            None,
            None,
            Some("userMessage"),
            "item/completed",
            &json!({"item":{"content":[{"text":text}]}}).to_string(),
            segments.map(|s| s.to_string()).as_deref(),
        )
        .await
        .unwrap()
}

async fn detail_title(b: &Boot, track: &str, card: &str) -> Value {
    let (status, body) = b
        .request("GET", &format!("/api/tracks/{track}"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == card)
        .unwrap()["title"]
        .clone()
}

#[tokio::test]
async fn conversation_title_manual_clear_single_card_event_and_track_rename() {
    let b = boot().await;
    let track = b.create_track("Track never names the conversation").await;
    let card = planner(&b, &track).await;
    assert_eq!(detail_title(&b, &track, &card).await, Value::Null);
    input(
        &b,
        &track,
        &card,
        None,
        "User says:\nFirst actual input\nmore",
    )
    .await;
    let explicit =
        "An explicit name is not truncated even when longer than forty eight UTF-16 units";
    for (patch, expected) in [
        (json!({"title":explicit}), explicit),
        (json!({"title":""}), "First actual input"),
    ] {
        let (status, body) = b
            .request("PATCH", &format!("/api/cards/{card}"), None, Some(patch))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["title"], expected);
        assert_eq!(detail_title(&b, &track, &card).await, expected);
    }
    let (status, body) = b
        .request(
            "PATCH",
            &format!("/api/tracks/{track}"),
            None,
            Some(json!({"title":"New Track name"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(detail_title(&b, &track, &card).await, "First actual input");
    let stored = b.repo.card_get(&card).await.unwrap().unwrap();
    assert_eq!(
        stored.title, None,
        "projection must not persist a derived title"
    );
    let mut event = calm_server::event::Event::CardUpdated(stored);
    calm_server::session_projection_lookup::project_runtime_into_event_payload(
        b.repo.as_ref(),
        &mut event,
    )
    .await
    .unwrap();
    match event {
        calm_server::event::Event::CardUpdated(card) => {
            assert_eq!(card.title.as_deref(), Some("First actual input"))
        }
        _ => unreachable!(),
    }
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn conversation_title_structured_system_legacy_isolation_and_replacement() {
    let b = boot().await;
    let track = b.create_track("Unrelated Track").await;
    let card = planner(&b, &track).await;
    let other_track = b.create_track("Another Track").await;
    let other = planner(&b, &other_track).await;
    input(&b, &other_track, &other, None, "Other card input").await;
    input(
        &b,
        &track,
        &card,
        Some(json!([{"presentation":"system","text":"system-only","attachments":[]}])),
        "never infer from flattened text",
    )
    .await;
    let first = input(
        &b,
        &track,
        &card,
        Some(json!([
            {"presentation":"system","text":"context","attachments":[]},
            {"presentation":"user","text":"Historical Track goal","attachments":[]},
            {"presentation":"user","text":"later user in same batch","attachments":[]}
        ])),
        "flattened words",
    )
    .await;
    for _ in 0..301 {
        input(&b, &track, &card, None, "Later page input").await;
    }
    assert_eq!(
        detail_title(&b, &track, &card).await,
        "Historical Track goal"
    );
    assert_eq!(
        detail_title(&b, &other_track, &other).await,
        "Other card input"
    );
    // Simulate the transcript cut performed by replace: the projection is over actual rows,
    // never a name-once value. No runtime is required for these persisted sources.
    sqlx::query("DELETE FROM harness_items WHERE id=?1")
        .bind(first)
        .execute(b.repo.db().writer_pool())
        .await
        .unwrap();
    assert_eq!(detail_title(&b, &track, &card).await, "Later page input");
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn conversation_title_assistant_create_replay_pending_and_no_live_session() {
    let b = boot().await;
    let track = b.create_track("Track title").await;
    let (status, first) = b
        .create_internal_conversation(&track, "title-replay", "Pending first input\nsecond")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["title"], "Pending first input");
    let card = first["id"].as_str().unwrap();
    // Freeze a typed, projectable pending snapshot so the pending-only assertion is
    // independent of when the background loop drains after the HTTP response.
    b.shutdown_harnesses().await;
    let mut tx = b.repo.db().writer_pool().begin().await.unwrap();
    calm_server::db::sqlite::harness_items_delete_by_card_tx(&mut tx, card)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let pending = calm_server::harness::HarnessSnapshot::initial(
        0,
        vec![
            calm_server::harness::queue::QueueEntry::user_message("  ".into(), None, Vec::new()),
            calm_server::harness::queue::QueueEntry::user_message(
                "Pending first input\nsecond".into(),
                None,
                Vec::new(),
            ),
        ],
    );
    sqlx::query("UPDATE worker_sessions SET state='idle',handle_state_json=?1 WHERE card_id=?2")
        .bind(serde_json::to_string(&pending).unwrap())
        .bind(card)
        .execute(b.repo.db().writer_pool())
        .await
        .unwrap();
    // Invisible persisted records must permit the same pending fallback as no records.
    input(
        &b,
        &track,
        card,
        None,
        "## Track state changes since your last turn\nchanged\n\n---\n\nUser says:\n",
    )
    .await;
    input(
        &b,
        &track,
        card,
        Some(json!([
            {"presentation":"system","text":"context","attachments":[]},
            {"presentation":"user","text":"User says:\n  ","attachments":[]}
        ])),
        "not authoritative",
    )
    .await;
    let (status, pending_rows) = b.list_conversations(&track).await;
    assert_eq!(status, StatusCode::OK, "{pending_rows}");
    assert_eq!(pending_rows[0]["title"], "Pending first input");
    assert_eq!(
        b.scalar("SELECT COUNT(*) FROM harness_items WHERE card_id=?1", card)
            .await,
        2,
        "only the two invisible records exist; the title comes from the pending snapshot"
    );
    let (status, replay) = b
        .create_internal_conversation(&track, "title-replay", "Pending first input\nsecond")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay["id"], first["id"]);
    assert_eq!(replay["title"], first["title"]);
    let image = input(&b, &track, card, Some(json!([
        {"presentation":"user","text":"", "attachments":[]},
        {"presentation":"user","text":"", "attachments":[{
            "id":"12345678-1234-4234-8234-123456789abc.png", "contentType":"image/png", "size":1,
            "url":"/api/cards/image/planner/attachments/12345678-1234-4234-8234-123456789abc.png"
        }]},
        {"presentation":"user","text":"Later segment cannot name image-only input", "attachments":[]}
    ])), "flattened").await;
    assert_eq!(
        b.list_conversations(&track).await.1[0]["title"],
        Value::Null
    );
    input(&b, &track, card, None, "Durable input wins over pending").await;
    assert_eq!(
        b.list_conversations(&track).await.1[0]["title"],
        Value::Null
    );
    sqlx::query("DELETE FROM harness_items WHERE id=?1")
        .bind(image)
        .execute(b.repo.db().writer_pool())
        .await
        .unwrap();
    b.shutdown_harnesses().await;
    sqlx::query("DELETE FROM worker_sessions WHERE card_id=?1")
        .bind(card)
        .execute(b.repo.db().writer_pool())
        .await
        .unwrap();
    let (status, rows) = b.list_conversations(&track).await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    assert_eq!(rows[0]["title"], "Durable input wins over pending");
    assert_eq!(rows[0]["state"], Value::Null);
}

#[tokio::test]
async fn conversation_title_skips_invisible_legacy_and_structured_records() {
    let b = boot().await;
    for segments in [
        None,
        Some(json!([{"presentation":"user","text":"User says:\n  ","attachments":[]}])),
    ] {
        let track = b.create_track("Invisible input").await;
        let card = planner(&b, &track).await;
        input(&b, &track, &card, segments, "User says:\n  ").await;
        input(&b, &track, &card, None, "Actual original title").await;
        assert_eq!(
            detail_title(&b, &track, &card).await,
            "Actual original title"
        );
    }
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn conversation_title_skips_empty_segments_in_same_batch() {
    let b = boot().await;
    let track = b.create_track("Empty segments").await;
    let card = planner(&b, &track).await;
    input(
        &b,
        &track,
        &card,
        Some(json!([
            {"presentation":"system","text":"context", "attachments":[]},
            {"presentation":"user","text":"", "attachments":[]},
            {"presentation":"user","text":"User says:\n \n", "attachments":[]},
            {"presentation":"user","text":"User says:\nFirst visible segment", "attachments":[]},
            {"presentation":"user","text":"Later segment", "attachments":[]}
        ])),
        "flattened",
    )
    .await;
    input(&b, &track, &card, None, "Later row").await;
    assert_eq!(
        detail_title(&b, &track, &card).await,
        "First visible segment"
    );
    b.shutdown_harnesses().await;
}
