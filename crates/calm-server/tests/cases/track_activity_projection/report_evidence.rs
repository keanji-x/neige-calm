//! E7 reads the production statement and projector, including its migration-backed access path.

use super::*;
use calm_server::track_activity::sql::E7_AGENT_REPORT_EDIT_SQL;
use sqlx::Row;

async fn report(f: &Fx, track: &str, author: EditAuthor, at: i64) {
    let event = Event::TrackReportEdited {
        track_id: track.to_string().into(),
        card_id: "report-fixture".into(),
        author,
        author_plugin_id: None,
        edit_id: format!("report-{at}"),
        summary_before: String::new(),
        summary_after: String::new(),
        body_before: String::new(),
        body_after: String::new(),
        agent_message: None,
    };
    let (_, id) = write_with_event_typed(
        f.repo_dyn.as_ref(),
        ActorId::Kernel,
        f.track_scope(track),
        None,
        &f.events,
        &f.write,
        move |_tx| Box::pin(async move { Ok(((), event)) }),
    )
    .await
    .unwrap();
    // Pin time only; event serialization and scope columns come from the production writer.
    sqlx::query("UPDATE events SET at = ? WHERE id = ?")
        .bind(at)
        .bind(id)
        .execute(&f.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn e7_report_evidence_is_track_scoped() {
    let f = fx().await;
    let target = f.track("target").await;
    let other = f.track("other").await;
    let empty = f.track("empty").await;
    let user_only = f.track("user only").await;
    report(&f, &target, EditAuthor::Planner, 1_000).await;
    report(&f, &target, EditAuthor::Assistant, 2_000).await;
    report(&f, &target, EditAuthor::User, 3_000).await;
    report(&f, &other, EditAuthor::Planner, 9_000).await;
    report(&f, &user_only, EditAuthor::User, 8_000).await;

    for (track, expected) in [
        (&target, Some(2_000)),
        (&other, Some(9_000)),
        (&empty, None),
        (&user_only, None),
    ] {
        let actual: Option<i64> = sqlx::query_scalar(E7_AGENT_REPORT_EDIT_SQL)
            .bind(track)
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(actual, expected, "E7 MAX for {track}");
        assert_eq!(
            f.recompute(track).await.activity_at_ms,
            expected,
            "projector E7 for {track}"
        );
    }

    let details: Vec<String> =
        sqlx::query(&format!("EXPLAIN QUERY PLAN {E7_AGENT_REPORT_EDIT_SQL}"))
            .bind(&target)
            .fetch_all(&f.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get("detail"))
            .collect();
    let events: Vec<_> = details.iter().filter(|d| d.contains("events")).collect();
    assert!(!events.is_empty(), "E7 must read events: {details:?}");
    assert!(
        events
            .iter()
            .all(|d| d.starts_with("SEARCH events ") && d.contains("scope_track=?")),
        "E7 must constrain every events search by scope_track: {details:?}"
    );
}
