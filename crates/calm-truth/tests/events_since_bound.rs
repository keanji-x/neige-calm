//! `events_since` must not permit an unbounded read: no call shape can express sqlite's `LIMIT -1`.

use calm_truth::card_role_cache::CardRoleCache;
use calm_truth::db::RepoEventWrite;
use calm_truth::db::sqlite::SqlxRepo;
use calm_truth::event::{Event, EventBus, EventScope};
use calm_truth::ids::ActorId;
use calm_truth::model::{Area, AreaKind};
use calm_truth::track_area_cache::TrackAreaCache;

async fn seed_area_updates(repo: &SqlxRepo, n: usize) -> Vec<i64> {
    let bus = EventBus::new();
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let id = repo
            .log_pure_event(
                ActorId::User,
                EventScope::System,
                None,
                &bus,
                &CardRoleCache::new(),
                &TrackAreaCache::new(),
                Event::AreaUpdated(Area {
                    id: format!("c-{i}").into(),
                    name: "n".into(),
                    color: "#000".into(),
                    sort: 0.0,
                    kind: AreaKind::User,
                    default_template_id: None,
                    default_cwd: None,
                    created_at: 0,
                    updated_at: 0,
                }),
            )
            .await
            .expect("seed event");
        ids.push(id);
    }
    ids
}

#[tokio::test]
async fn events_since_enforces_caller_bound() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    let seeded = seed_area_updates(&repo, 8).await;

    let rows = repo.events_since(0, 5).await.expect("events_since");
    assert_eq!(
        rows.len(),
        5,
        "events_since must enforce the caller-supplied bound"
    );
    let got: Vec<i64> = rows.iter().map(|(id, ..)| *id).collect();
    assert_eq!(
        got,
        seeded[..5],
        "window is the first `limit` rows in id order"
    );

    let rest = repo
        .events_since(seeded[4], 5)
        .await
        .expect("events_since tail");
    let got: Vec<i64> = rest.iter().map(|(id, ..)| *id).collect();
    assert_eq!(got, seeded[5..], "next page resumes past the bound");
}

#[tokio::test]
async fn events_since_non_positive_limit_returns_no_rows() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    seed_area_updates(&repo, 3).await;

    for limit in [0, -1, -100] {
        let rows = repo.events_since(0, limit).await.expect("events_since");
        assert!(rows.is_empty(), "limit {limit} must return no rows");
    }
}

#[tokio::test]
async fn events_since_keeps_pre_3b_prime_task_context_frozen_events() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    sqlx::query(
        r#"INSERT INTO events (kind, payload, actor, at, event_version)
           VALUES ('task.context_frozen', '{"task_id":"w:old","refs":[]}', 'kernel', 0, 1)"#,
    )
    .execute(repo.pool())
    .await
    .expect("insert historical freeze");

    let rows = repo.events_since(0, 10).await.expect("events_since");
    assert_eq!(
        rows.len(),
        1,
        "historical freeze must not be silently dropped"
    );
    assert!(matches!(
        &rows[0].3,
        Event::TaskContextFrozen {
            task_id,
            refs,
            doc_revs,
            truncated: false,
            ..
        } if task_id == "w:old" && refs.is_empty() && doc_revs.is_empty()
    ));
}

/// Pre-telemetry `harness.transcript.cleared` rows must still deserialize: a dropped row would
/// desync the WS replay cursor and splice the post-reset transcript onto the pre-reset one.
#[tokio::test]
async fn events_since_keeps_pre_1252_harness_transcript_cleared_events() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    sqlx::query(
        r#"INSERT INTO events (kind, payload, actor, at, event_version)
           VALUES (
             'harness.transcript.cleared',
             '{"card_id":"card-old","worker_session_id":"rt-old","track_id":"track-old"}',
             'kernel',
             0,
             1
           )"#,
    )
    .execute(repo.pool())
    .await
    .expect("insert historical transcript reset");

    let rows = repo.events_since(0, 10).await.expect("events_since");
    assert_eq!(
        rows.len(),
        1,
        "historical transcript reset must not be silently dropped"
    );
    assert!(
        matches!(
            &rows[0].3,
            Event::HarnessTranscriptCleared {
                worker_session_id: runtime_id,
                card_id,
                track_id,
                cleared_item_count: None,
                cleared_params_bytes: None,
                card_age_ms_at_clear: None,
            } if runtime_id == "rt-old"
                && card_id.as_str() == "card-old"
                && track_id.as_str() == "track-old"
        ),
        "pre-#1252 row must replay with unmeasured telemetry, got {:?}",
        rows[0].3
    );
}

#[tokio::test]
async fn events_since_skips_retired_workflow_registered_without_error() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    let before = seed_area_updates(&repo, 1).await;
    sqlx::query(
        r#"INSERT INTO events (kind, payload, actor, at, event_version)
           VALUES (
             'workflow.registered',
             '{"pluginId":"gitforge","workflowId":"issue-development"}',
             'kernel',
             0,
             9
           )"#,
    )
    .execute(repo.pool())
    .await
    .expect("insert retired workflow.registered");
    let after = seed_area_updates(&repo, 1).await;

    let rows = repo
        .events_since(0, 100)
        .await
        .expect("replay of a WorkflowRegistered envelope must not fail");
    let ids: Vec<i64> = rows.iter().map(|(id, _, _, _)| *id).collect();
    assert_eq!(
        ids,
        [before[0], after[0]],
        "retired kind is skipped, neighbors kept"
    );
    assert!(
        rows.iter()
            .all(|(_, _, _, ev)| ev.kind_tag() != "workflow.registered"),
        "retired variant must not deserialize back into Event"
    );
}

#[tokio::test]
async fn events_raw_window_since_probes_raw_rows_and_respects_probe_limit() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    let seeded = seed_area_updates(&repo, 4).await;
    // Unknown kind seeded LAST so the `max_id` assertion proves the probe sees past what `events_since` surfaces.
    let unknown_id: i64 = sqlx::query_scalar(
        r#"INSERT INTO events (kind, payload, actor, at, event_version)
           VALUES ('test.unknown_kind', '{}', 'user', 0, 1)
           RETURNING id"#,
    )
    .fetch_one(repo.pool())
    .await
    .expect("insert unknown-kind row");

    let filtered = repo.events_since(0, 100).await.expect("events_since");
    assert_eq!(
        filtered.len(),
        4,
        "unknown-kind row is filtered from events_since"
    );
    assert_eq!(
        repo.events_raw_window_since(0, 100)
            .await
            .expect("raw probe"),
        (5, Some(unknown_id)),
        "raw probe must count rows events_since drops and report the raw window end"
    );

    assert_eq!(
        repo.events_raw_window_since(0, 3).await.expect("raw probe"),
        (3, Some(seeded[2]))
    );
    assert_eq!(
        repo.events_raw_window_since(seeded[1], 100)
            .await
            .expect("raw probe"),
        (3, Some(unknown_id)),
        "two good rows + the unknown-kind row remain past seeded[1]"
    );
    assert_eq!(
        repo.events_raw_window_since(unknown_id, 100)
            .await
            .expect("raw probe"),
        (0, None)
    );
    for limit in [0, -1, -100] {
        assert_eq!(
            repo.events_raw_window_since(0, limit)
                .await
                .expect("raw probe"),
            (0, None),
            "probe limit {limit} must probe zero rows"
        );
    }
}

#[tokio::test]
async fn retired_review_rows_are_preserved_but_do_not_block_typed_readers() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let payload = serde_json::json!({
        "track_id": "retired-track", "subject": {"phase": "impl", "slice_id": "5b", "pr_number": 760},
        "head_sha": "head-sha", "n": 1, "cap": 8, "converged": false,
        "channels": [{"role": "correctness", "verdict": "changes_requested"}],
        "root_cause": "historical", "idempotency_key": "review.round:retired-track:impl:5b:760:1"
    }).to_string();
    let actor = serde_json::to_string(&ActorId::AiPlanner("historical-planner".into())).unwrap();
    let retired: i64 = sqlx::query_scalar(
        "INSERT INTO events(kind,payload,actor,at,scope_kind,scope_track) \
         VALUES('review.round',?,?,0,'track','retired-track') RETURNING id",
    )
    .bind(&payload)
    .bind(&actor)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    let live: i64 = sqlx::query_scalar(
        "INSERT INTO events(kind,payload,actor,at,scope_kind,scope_track) \
         VALUES('ask.requested',?,?,0,'track','retired-track') RETURNING id",
    )
    .bind(r#"{"track_id":"retired-track","questions":[{"title":"Continue?","options":[]}]}"#)
    .bind(&actor)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert!(
        Event::from_kind_and_payload("review.round", serde_json::from_str(&payload).unwrap())
            .is_err()
    );
    let ids: Vec<_> = repo
        .events_since(0, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|(id, ..)| id)
        .collect();
    assert_eq!(ids, vec![live]);
    let rows = repo
        .events_for_track("retired-track", &["review.round", "ask.requested"], None)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, live);
    assert_eq!(
        repo.events_raw_window_since(0, 1).await.unwrap(),
        (1, Some(retired))
    );
    assert_eq!(
        repo.events_raw_window_since(retired, 1).await.unwrap(),
        (1, Some(live))
    );
    let stored: String = sqlx::query_scalar("SELECT payload FROM events WHERE id=?")
        .bind(retired)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(stored, payload);
}

#[tokio::test]
async fn retired_ratify_rows_preserve_history_and_do_not_hide_current_asks() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let actor = serde_json::to_string(&ActorId::User).unwrap();
    let mut historical = Vec::new();
    for (kind, payload) in [
        (
            "ratify.requested",
            r#"{"track_id":"track","reason":"Merge?"}"#,
        ),
        (
            "ratify.resolved",
            r#"{"track_id":"track","decision":"grant"}"#,
        ),
        (
            "ratify.resolved",
            r#"{"track_id":"track","decision":"deny","message":"Hold"}"#,
        ),
    ] {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO events(kind,payload,actor,at,scope_kind,scope_track) \
             VALUES(?,?,?,0,'track','track') RETURNING id",
        )
        .bind(kind)
        .bind(payload)
        .bind(&actor)
        .fetch_one(repo.pool())
        .await
        .unwrap();
        historical.push((id, kind, payload));
    }
    let request = Event::AskRequested {
        track_id: "track".into(),
        questions: vec![calm_types::event::AskQuestion {
            title: "Continue?".into(),
            options: Vec::new(),
        }],
        source_item_id: None,
    };
    let live: i64 = sqlx::query_scalar(
        "INSERT INTO events(kind,payload,actor,at,scope_kind,scope_track) \
         VALUES(?,?,?,0,'track','track') RETURNING id",
    )
    .bind(request.kind_tag())
    .bind(request.payload_value().to_string())
    .bind(&actor)
    .fetch_one(repo.pool())
    .await
    .unwrap();

    assert!(repo.events_since(0, 3).await.unwrap().is_empty());
    let events = repo.events_since(0, 4).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, live);
    assert_eq!(events[0].3.payload_value(), request.payload_value());
    let rows = repo
        .events_for_track(
            "track",
            &["ratify.requested", "ratify.resolved", "ask.requested"],
            None,
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, live);
    assert_eq!(rows[0].event.payload_value(), request.payload_value());
    assert!(
        repo.events_for_track(
            "track",
            &["ratify.requested", "ratify.resolved"],
            Some(historical[0].0),
        )
        .await
        .unwrap()
        .is_empty()
    );

    let mut cursor = 0;
    for (id, kind, payload) in historical {
        assert_eq!(
            repo.events_raw_window_since(cursor, 1).await.unwrap(),
            (1, Some(id))
        );
        assert!(repo.events_since(cursor, 1).await.unwrap().is_empty());
        let stored: (String, String) = sqlx::query_as("SELECT kind,payload FROM events WHERE id=?")
            .bind(id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
        assert_eq!(stored, (kind.to_owned(), payload.to_owned()));
        cursor = id;
    }
    assert_eq!(repo.events_since(cursor, 1).await.unwrap()[0].0, live);
    assert_eq!(
        repo.events_raw_window_since(live, 1).await.unwrap(),
        (0, None)
    );
}
