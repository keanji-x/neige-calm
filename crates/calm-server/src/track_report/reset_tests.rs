//! The reset door's revision anchor, driven through `write::rest_user_reset` against a real store.

use std::sync::Arc;

use serde_json::json;

use super::{ReportDocOp, ReportEditTarget, TrackReportPayload, write};
use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::SqlxRepo;
use crate::db::{RepoEventWrite, RepoSyncDomainRaw};
use crate::error::CalmError;
use crate::event::EventBus;
use crate::model::{NewArea, NewCard, NewTrack};
use crate::state::WriteContext;
use crate::track_area_cache::TrackAreaCache;

/// The route reads the revision before it writes, so an edit landing in between reaches the door
/// as a stale `if_doc_rev`: that is a 409, and the report, its data blocks and the event log stay as they were.
#[tokio::test]
async fn a_reset_at_a_stale_revision_is_a_conflict_and_writes_nothing() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "reset".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "reset".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    repo.card_create(NewCard {
        track_id: track.id.clone(),
        kind: "track-report".into(),
        sort: Some(-1.0),
        payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        title: None,
    })
    .await
    .unwrap();
    let events = EventBus::new();
    let write = WriteContext::new(CardRoleCache::new(), TrackAreaCache::new());
    let track_id = track.id.to_string();
    let target = ReportEditTarget::resolve(repo.as_ref(), &track_id)
        .await
        .unwrap();
    write::rest_user_block_op(
        repo.as_ref(),
        &events,
        &write,
        target,
        ReportDocOp::UpsertBlock {
            id: None,
            kind: "table".into(),
            content: calm_types::report_blocks::render_fence(
                "table",
                &json!({ "columns": [{ "key": "k", "label": "K" }], "rows": [{ "k": 1 }] }),
            ),
            if_rev: None,
            if_doc_rev: Some(0),
            position: None,
        },
    )
    .await
    .unwrap();
    let report = || async {
        let (_, card, _) = super::resolve_report_for_track(repo.as_ref(), &track_id)
            .await
            .unwrap();
        card.payload
    };
    let before = report().await;
    assert_eq!(before["docRev"], 1, "{before}");
    let events_before = repo.events_latest_id().await.unwrap();
    let mut subscriber = events.subscribe();

    let target = ReportEditTarget::resolve(repo.as_ref(), &track_id)
        .await
        .unwrap();
    let error = write::rest_user_reset(repo.as_ref(), &events, &write, target, 0)
        .await
        .unwrap_err();

    assert!(matches!(error, CalmError::Conflict(_)), "{error:?}");
    assert_eq!(report().await, before, "a refused reset must write nothing");
    assert_eq!(repo.events_latest_id().await.unwrap(), events_before);
    assert!(
        subscriber.try_recv().is_err(),
        "a refused reset emits nothing"
    );
}

/// The op enforces its pairing with the user's door: under any other author it is refused and the
/// document, its data block included, is left as it was.
#[test]
fn only_the_user_may_apply_a_reset() {
    let mut doc = crate::track_report_doc::ReportDoc::from_payload(&TrackReportPayload::new(
        "s",
        &calm_types::report_blocks::render_fence("app", &json!({ "src": "/apps/x" })),
    ));
    let before = doc.project().unwrap();
    for author in [
        crate::event::EditAuthor::Planner,
        crate::event::EditAuthor::Assistant,
        crate::event::EditAuthor::Kernel,
        crate::event::EditAuthor::Plugin,
    ] {
        let error = super::apply_report_op(
            &mut doc,
            &ReportDocOp::ResetToInitial { if_doc_rev: 0 },
            author,
        )
        .unwrap_err();
        assert!(
            matches!(error, CalmError::Internal(_)),
            "{author:?}: {error:?}"
        );
        assert_eq!(doc.project().unwrap(), before, "{author:?} wrote");
    }
}
