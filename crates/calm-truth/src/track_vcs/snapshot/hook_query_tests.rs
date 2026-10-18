use super::*;
use crate::db::sqlite::SqlxRepo;

#[tokio::test]
async fn transcript_hook_identity_selector_uses_covering_event_access() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let details: Vec<String> = sqlx::query(&format!("EXPLAIN QUERY PLAN {}", hook_ids_query()))
        .bind("track query-plan probe")
        .bind("card query-plan probe")
        .bind(track_fs_view::HOOK_EVENT_TRANSCRIPT_CAP as i64)
        .fetch_all(repo.pool())
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get("detail"))
        .collect();
    // Constrain the cost invariant, not index names, node IDs, planner order or latency.
    let accesses: Vec<_> = details
        .iter()
        .filter(|detail| {
            (detail.starts_with("SEARCH ") || detail.starts_with("SCAN "))
                && !detail.to_ascii_lowercase().contains("subquery")
        })
        .collect();
    assert!(
        !accesses.is_empty(),
        "identity query must read its event relations: {details:?}"
    );
    assert!(
        accesses
            .iter()
            .all(|detail| detail.contains("USING COVERING INDEX")),
        "hook-ID validation must avoid event table payload access: {details:?}"
    );
}

#[tokio::test]
async fn transcript_hook_selector_caps_mixed_kinds_after_exact_scope_filtering() {
    use crate::event::{Event, EventScope};
    use crate::ids::ActorId;
    use crate::track_vcs::transcript_writer_tests::Fixture;

    let fx = Fixture::new().await;
    let correct = EventScope::Card {
        card: fx.card.id.clone(),
        track: fx.track.id.clone(),
        area: fx.track.area_id.clone(),
    };
    let cap = track_fs_view::HOOK_EVENT_TRANSCRIPT_CAP;
    let mut expected = Vec::new();
    for index in 0..cap + 40 {
        let payload =
            serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": index.to_string()});
        let event = if index % 2 == 0 {
            Event::CodexHook {
                card_id: fx.card.id.clone(),
                kind: "hook.codex.user_prompt_submit".into(),
                hook_idempotency_key: format!("mixed-cap-{index}"),
                payload,
            }
        } else {
            Event::ClaudeHook {
                card_id: fx.card.id.clone(),
                kind: "hook.claude.user_prompt_submit".into(),
                hook_idempotency_key: format!("mixed-cap-{index}"),
                payload,
            }
        };
        expected.push(
            fx.repo
                .event_append_fixture(ActorId::Kernel, correct.clone(), None, &event)
                .await
                .unwrap(),
        );
    }
    let expected = expected.split_off(40);
    // All excluded rows are newer than every qualifying row. Do not infer scope from current
    // card ownership; historical card/track mismatches and SQL NULLs must remain excluded.
    for (index, scope) in [
        EventScope::Card {
            card: fx.card.id.clone(),
            track: crate::ids::TrackId::from("different historical track"),
            area: fx.track.area_id.clone(),
        },
        EventScope::Track {
            track: fx.track.id.clone(),
            area: fx.track.area_id.clone(),
        },
        EventScope::System,
    ]
    .into_iter()
    .enumerate()
    {
        fx.repo.event_append_fixture(ActorId::Kernel, scope, None, &Event::CodexHook {
            card_id: fx.card.id.clone(), kind: "hook.codex.user_prompt_submit".into(),
            hook_idempotency_key: format!("excluded scope {index}"),
            payload: serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": "excluded scope"}),
        }).await.unwrap();
    }
    fx.repo
        .event_append_fixture(
            ActorId::Kernel,
            correct,
            None,
            &Event::CardUpdated(fx.card.clone()),
        )
        .await
        .unwrap();
    let rows = hook_rows_for_card(fx.repo.pool(), &fx.track.id, &fx.card.id)
        .await
        .unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(fx.repo.pool())
        .await
        .unwrap();
    let ids = hook_ids_for_card(&mut **tx, &fx.track.id, &fx.card.id)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(ids, expected);
    assert_eq!(rows.iter().map(|row| row.0).collect::<Vec<_>>(), expected);
    assert_eq!(rows.len(), cap);
    let events = hook_events_from_rows(rows).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "codex.hook"));
    assert!(events.iter().any(|event| event.kind == "claude.hook"));
}
