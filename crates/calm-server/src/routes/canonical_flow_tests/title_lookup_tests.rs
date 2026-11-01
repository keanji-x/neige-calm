//! Title metadata must not read an unrelated imported display prefix under writer admission.
use super::*;
use crate::db::sqlite::conversation::{
    PresentationSource, allocate_presentation_tx, import_item_tx, insert_message_tx,
};
use calm_types::conversation::{HumanPayload, MessageAuthor};
use calm_types::organization::LEGACY_OWNER_USER_ID;
use std::sync::atomic::{AtomicUsize, Ordering};

async fn read_titles(
    app: &axum::Router,
    cookie: &str,
    track: &str,
    card: &str,
    steps: &AtomicUsize,
    expected: Option<&str>,
) -> [usize; 2] {
    let mut work = [0; 2];
    for (index, suffix) in ["", "/conversations"].into_iter().enumerate() {
        let before = steps.load(Ordering::SeqCst);
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/api/tracks/{track}{suffix}"))
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
        let rows = if suffix.is_empty() {
            &body["cards"]
        } else {
            &body
        };
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == card)
            .unwrap();
        assert_eq!(row["title"], json!(expected), "{suffix}: {row}");
        work[index] = steps.load(Ordering::SeqCst) - before;
    }
    work
}

async fn human(fixture: &Fixture, card: &str, text: &str) -> String {
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    let stored = insert_message_tx(
        &mut tx,
        card,
        &MessageAuthor::AuthenticatedHuman {
            registered_user_id: LEGACY_OWNER_USER_ID.into(),
        },
        HumanPayload {
            text: text.into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    allocate_presentation_tx(
        &mut tx,
        card,
        PresentationSource::HumanMessage(&stored.reference.message_id),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    stored.reference.message_id.to_string()
}

#[tokio::test]
async fn conversation_title_http_imported_prefix_has_bounded_sql_work() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = super::assistant_canonical::track(&app, &cookie).await;
    fixture.shutdown_process(&state).await;
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    let card = crate::db::sqlite::card_create_with_id_tx(
        &mut tx,
        crate::model::new_id(),
        crate::model::NewCard {
            track_id: track.clone().into(),
            kind: "codex".into(),
            sort: None,
            title: None,
            payload: json!({"assistant_provider":"codex", "harness_profile":
                crate::harness::profile::ASSISTANT_MARKER}),
        },
        crate::model::CardRole::Assistant,
        true,
        fixture.repo.card_role_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let card = card.id.to_string();
    // Instrument every application connection: moving a scan to readers must not hide it.
    let db = state.sqlite_db().unwrap();
    let steps = Arc::new(AtomicUsize::new(0));
    for pool in [db.read_pool(), db.writer_pool()] {
        let mut held = Vec::new();
        for _ in 0..pool.options().get_max_connections() {
            let mut connection = pool.acquire().await.unwrap();
            let count = steps.clone();
            connection
                .lock_handle()
                .await
                .unwrap()
                .set_progress_handler(100, move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    true
                });
            held.push(connection);
        }
    }
    let baseline = read_titles(&app, &cookie, &track, &card, &steps, None).await;
    // Use production import primitives to preserve real provenance and payload contracts.
    let mut rows = Vec::new();
    for _ in 0..2048 {
        rows.push(
            fixture
                .repo
                .fixture_transcript_insert(
                    "historical",
                    &card,
                    &track,
                    "imported",
                    None,
                    None,
                    Some("agentMessage"),
                    "item/completed",
                    r#"{"item":{"text":"irrelevant imported body"}}"#,
                    None,
                )
                .await
                .unwrap(),
        );
    }
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    for row in rows {
        import_item_tx(&mut tx, &card, row, 0)
            .await
            .unwrap()
            .unwrap();
    }
    tx.commit().await.unwrap();
    let no_human = read_titles(&app, &cookie, &track, &card, &steps, None).await;
    let first = human(&fixture, &card, "First effective Human\nsecond line").await;
    let with_human = read_titles(
        &app,
        &cookie,
        &track,
        &card,
        &steps,
        Some("First effective Human"),
    )
    .await;
    sqlx::query("UPDATE conversation_messages SET withdrawn_at_ms=1 WHERE id=?1")
        .bind(&first)
        .execute(fixture.repo.db().writer_pool())
        .await
        .unwrap();
    let withdrawn = read_titles(&app, &cookie, &track, &card, &steps, None).await;
    human(&fixture, &card, "Next effective Human").await;
    let next = read_titles(
        &app,
        &cookie,
        &track,
        &card,
        &steps,
        Some("Next effective Human"),
    )
    .await;
    sqlx::query("UPDATE conversation_presentations SET erased_by_operation_id=?1 WHERE card_id=?2 AND source_kind='human_message'")
        .bind("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").bind(&card)
        .execute(fixture.repo.db().writer_pool()).await.unwrap();
    let erased = read_titles(&app, &cookie, &track, &card, &steps, None).await;
    // Remove instrumentation before asserting so a regression still releases the real pool.
    for pool in [db.read_pool(), db.writer_pool()] {
        let mut held = Vec::new();
        for _ in 0..pool.options().get_max_connections() {
            let mut connection = pool.acquire().await.unwrap();
            connection
                .lock_handle()
                .await
                .unwrap()
                .remove_progress_handler();
            held.push(connection);
        }
    }
    // Allow fixed metadata overhead while rejecting even one pass over the imported prefix.
    for (case, work) in [
        ("no Human", no_human),
        ("Human", with_human),
        ("withdrawn", withdrawn),
        ("next", next),
        ("erased", erased),
    ] {
        eprintln!(
            "title-cost {case}: ticks={work:?}, baseline={baseline:?} (100 VM instructions/tick)"
        );
        for index in 0..2 {
            assert!(
                work[index] <= baseline[index] + 30,
                "{case} endpoint={index}: SQL VM ticks {work:?}, baseline {baseline:?}; imported history must not be scanned"
            );
        }
    }
}
