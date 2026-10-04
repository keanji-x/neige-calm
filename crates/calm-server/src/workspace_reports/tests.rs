use super::*;
use crate::daily_planner::TIME_ZONE;
use crate::daily_planner::tests::{fixture, foreign_track};
use axum::extract::FromRef;
use serde_json::json;

async fn event(
    pool: &SqlitePool,
    track: &crate::model::Track,
    at: i64,
    before: &str,
    after: &str,
) -> i64 {
    // Historical edit fixtures model persisted events; the production projection is exercised below.
    let payload = json!({"track_id":track.id,"edit_id":format!("edit-{at}"),"summary_before":"before","summary_after":"after","body_before":before,"body_after":after});
    sqlx::query_scalar(concat!(
"INSERT INTO events(kind,payload,actor,at,event_version,scope_kind,scope_area,scope_track) ",
"VALUES('track.report_edited',?1,'\"kernel\"',?2,1,'track',?3,?4) RETURNING id",
))
        .bind(payload.to_string()).bind(at).bind(track.area_id.as_str()).bind(track.id.as_str()).fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn report_day_changes_include_reverts_closed_tracks_and_exact_date_edges() {
    let (_tmp, repo, state) = fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let track = foreign_track(&route).await;
    let (start, end) = day_window(parse_date("2026-10-03").unwrap(), TIME_ZONE).unwrap();
    event(repo.pool(), &track, start - 1, "excluded", "also excluded").await;
    let first = event(repo.pool(), &track, start, "original", "changed").await;
    let last = event(repo.pool(), &track, end - 1, "changed", "original").await;
    event(repo.pool(), &track, end, "excluded", "also excluded").await;
    let close_id = track.id.clone();
    crate::db::write_with_event_typed(
        route.repo.as_ref(),
        crate::ids::ActorId::User,
        crate::event::EventScope::Track {
            track: track.id.clone(),
            area: track.area_id.clone(),
        },
        None,
        &route.events,
        &route.write,
        |tx| {
            Box::pin(async move {
                let closed = crate::db::sqlite::track_update_tx(
                    tx,
                    close_id.as_str(),
                    crate::model::TrackPatch {
                        closed: Some(true),
                        ..Default::default()
                    },
                )
                .await?;
                Ok((
                    (),
                    crate::event::Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                        closed, None,
                    )),
                ))
            })
        },
    )
    .await
    .unwrap();
    let page = changes(
        repo.pool(),
        &ReportChangesQuery {
            date: "2026-10-03".into(),
            after: None,
            through_event_id: None,
        },
        TIME_ZONE,
    )
    .await
    .unwrap();
    assert_eq!(page.changes.len(), 1);
    let row = &page.changes[0];
    assert_eq!(row.edit_count, 2);
    assert_eq!(row.first_event_id, first);
    assert_eq!(row.last_event_id, last);
    assert_eq!(row.patch, "");
    assert!(!row.patch_truncated);
    assert_eq!(page.time_zone, "Asia/Shanghai");
    let edits = edits(
        repo.pool(),
        &ReportEditsQuery {
            date: page.date,
            track_id: track.id.to_string(),
            after: None,
            through_event_id: page.through_event_id,
        },
        TIME_ZONE,
    )
    .await
    .unwrap();
    assert_eq!(edits.edits.len(), 2);
    assert_eq!(edits.edits[0].edit.body_after, "changed");
    assert_eq!(edits.edits[1].edit.body_after, "original");
}

#[tokio::test]
async fn report_edit_pagination_pins_a_snapshot_and_reports_malformed_data() {
    let (_tmp, repo, state) = fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let track = foreign_track(&route).await;
    let (start, _) = day_window(parse_date("2026-10-03").unwrap(), TIME_ZONE).unwrap();
    for offset in 0..25 {
        event(repo.pool(), &track, start + offset, "old", "new").await;
    }
    let page = changes(
        repo.pool(),
        &ReportChangesQuery {
            date: "2026-10-03".into(),
            after: None,
            through_event_id: None,
        },
        TIME_ZONE,
    )
    .await
    .unwrap();
    event(repo.pool(), &track, start + 26, "new", "late").await;
    let query = ReportEditsQuery {
        date: page.date,
        track_id: track.id.to_string(),
        after: None,
        through_event_id: page.through_event_id,
    };
    let first = edits(repo.pool(), &query, TIME_ZONE).await.unwrap();
    assert_eq!(first.edits.len(), 20);
    let second = edits(
        repo.pool(),
        &ReportEditsQuery {
            after: first.next_cursor,
            ..query
        },
        TIME_ZONE,
    )
    .await
    .unwrap();
    assert_eq!(second.edits.len(), 5);
    assert!(second.next_cursor.is_none());
    sqlx::query("UPDATE events SET payload='{}' WHERE id=?1")
        .bind(second.edits[4].event_id)
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(
        changes(
            repo.pool(),
            &ReportChangesQuery {
                date: "2026-10-03".into(),
                after: None,
                through_event_id: Some(page.through_event_id)
            },
            TIME_ZONE
        )
        .await
        .is_err()
    );
    assert!(parse_date("2026-2-1").is_err());
    assert!(parse_date("2026-02-30").is_err());
}

#[tokio::test]
async fn report_group_pages_cover_every_track_and_exclude_later_edits() {
    let (_tmp, repo, state) = fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let (start, _) = day_window(parse_date("2026-10-03").unwrap(), TIME_ZONE).unwrap();
    let mut expected = std::collections::BTreeSet::new();
    let mut last = None;
    for offset in 0..21 {
        let track = foreign_track(&route).await;
        event(repo.pool(), &track, start + offset, "before", "after").await;
        expected.insert(track.id.to_string());
        last = Some(track);
    }
    let first = changes(
        repo.pool(),
        &ReportChangesQuery {
            date: "2026-10-03".into(),
            after: None,
            through_event_id: None,
        },
        TIME_ZONE,
    )
    .await
    .unwrap();
    assert_eq!(first.changes.len(), 20);
    assert!(first.next_cursor.is_some());
    event(repo.pool(), &last.unwrap(), start + 22, "after", "late").await;
    let second = changes(
        repo.pool(),
        &ReportChangesQuery {
            date: first.date.clone(),
            after: first.next_cursor,
            through_event_id: Some(first.through_event_id),
        },
        TIME_ZONE,
    )
    .await
    .unwrap();
    assert_eq!(second.changes.len(), 1);
    assert!(second.next_cursor.is_none());
    let actual = first
        .changes
        .into_iter()
        .chain(second.changes)
        .map(|change| {
            assert_eq!(change.edit_count, 1);
            change.track_id
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(actual, expected);
}
