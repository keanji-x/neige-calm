//! Objective projection and refusal roll back through the production user write doors.

use std::sync::Arc;

use super::{ReportDocOp, ReportEditTarget, TrackReportPayload, write};
use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::SqlxRepo;
use crate::db::{RepoEventWrite, RepoSyncDomainRaw};
use crate::error::CalmError;
use crate::event::EventBus;
use crate::model::{NewArea, NewCard, NewTrack};
use crate::state::WriteContext;
use crate::track_area_cache::TrackAreaCache;

use super::objective_tests::payload;
use calm_types::report_blocks::render_fence;

async fn fixture(enabled: bool) -> (Arc<SqlxRepo>, EventBus, WriteContext, String) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "reset".into(),
                color: "#000".into(),
                sort: None,
            },
        )
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

    if enabled {
        let mut tx = calm_truth::db::sqlite::begin_immediate_tx(repo.db())
            .await
            .unwrap();
        calm_truth::db::sqlite::track_capabilities::stamp_tx(
            &mut tx,
            &track.id,
            &[calm_types::track_capability::TrackCapability::Objectives],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    (repo, events, write, track_id)
}
fn create_op() -> ReportDocOp {
    ReportDocOp::UpsertBlock {
        id: None,
        kind: "objective".into(),
        content: render_fence("objective", &payload()),
        if_rev: None,
        if_doc_rev: Some(0),
        position: None,
    }
}
#[tokio::test]
async fn objective_user_write_and_reset_are_atomic() {
    let (repo, events, write, track) = fixture(true).await;
    let target = ReportEditTarget::resolve(repo.as_ref(), &track)
        .await
        .unwrap();
    write::rest_user_block_op(repo.as_ref(), &events, &write, target, create_op())
        .await
        .unwrap();
    let (seq, state) = calm_truth::objective_host::load_state(repo.db(), &track.clone().into())
        .await
        .unwrap();
    assert_eq!(seq.0, 1);
    assert_eq!(state.objectives.len(), 1);
    let before = super::resolve_report_for_track(repo.as_ref(), &track)
        .await
        .unwrap()
        .1;
    let log = repo.events_latest_id().await.unwrap();
    let target = ReportEditTarget::resolve(repo.as_ref(), &track)
        .await
        .unwrap();
    let error = write::rest_user_reset(repo.as_ref(), &events, &write, target, 1)
        .await
        .unwrap_err();
    assert!(matches!(error, CalmError::Conflict(_)), "{error:?}");
    assert_eq!(
        super::resolve_report_for_track(repo.as_ref(), &track)
            .await
            .unwrap()
            .1
            .payload,
        before.payload
    );
    assert_eq!(repo.events_latest_id().await.unwrap(), log);
    assert_eq!(
        calm_truth::objective_host::load_state(repo.db(), &track.into())
            .await
            .unwrap()
            .0
            .0,
        1
    );
}
#[tokio::test]
async fn objective_capability_absent_write_rolls_back_everything() {
    let (repo, events, write, track) = fixture(false).await;
    let before = super::resolve_report_for_track(repo.as_ref(), &track)
        .await
        .unwrap()
        .1
        .payload;
    let log = repo.events_latest_id().await.unwrap();
    let target = ReportEditTarget::resolve(repo.as_ref(), &track)
        .await
        .unwrap();
    let op = ReportDocOp::Batch {
        doc_anchor: super::DocAnchor::LastRead(0),
        summary: None,
        ops: vec![
            super::BatchBlockOp::Upsert {
                id: None,
                kind: "task".into(),
                content: render_fence(
                    "task",
                    &serde_json::json!({"key":"task","kind":"terminal","command":"true","declared_by":"user","ready":false}),
                ),
                if_rev: None,
                position: None,
            },
            super::BatchBlockOp::Upsert {
                id: None,
                kind: "objective".into(),
                content: render_fence("objective", &payload()),
                if_rev: None,
                position: None,
            },
        ],
    };
    let error = write::rest_user_block_op(repo.as_ref(), &events, &write, target, op)
        .await
        .unwrap_err();
    assert!(matches!(error,CalmError::Forbidden(reason) if reason.contains("capability")));
    let tasks: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE track_id=?")
        .bind(&track)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(tasks, 0);
    assert_eq!(
        super::resolve_report_for_track(repo.as_ref(), &track)
            .await
            .unwrap()
            .1
            .payload,
        before
    );
    assert_eq!(repo.events_latest_id().await.unwrap(), log);
    assert_eq!(
        calm_truth::objective_host::load_state(repo.db(), &track.into())
            .await
            .unwrap()
            .0
            .0,
        0
    );
}

#[tokio::test]
async fn objective_report_write_feeds_definition_in_the_same_tx() {
    let (repo, events, write, track) = fixture(true).await;
    let target = ReportEditTarget::resolve(repo.as_ref(), &track)
        .await
        .unwrap();
    write::test_report_op(
        repo.as_ref(),
        &events,
        &write,
        crate::event::EditAuthor::Planner,
        target,
        create_op(),
    )
    .await
    .unwrap();
    let before = super::resolve_report_for_track(repo.as_ref(), &track)
        .await
        .unwrap()
        .1
        .payload;
    let log = repo.events_latest_id().await.unwrap();
    assert_eq!(
        calm_truth::objective_host::load_state(repo.db(), &track.clone().into())
            .await
            .unwrap()
            .0
            .0,
        1
    );
    sqlx::query("CREATE TRIGGER refuse_objective BEFORE INSERT ON objective_events BEGIN SELECT RAISE(ABORT,'objective failure'); END").execute(repo.pool()).await.unwrap();
    let block = before["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["kind"] == "objective")
        .unwrap();
    let mut changed = payload();
    changed["title"] = serde_json::json!("changed");
    let op = ReportDocOp::UpsertBlock {
        id: Some(block["id"].as_str().unwrap().into()),
        kind: "objective".into(),
        content: render_fence("objective", &changed),
        if_rev: Some(block["rev"].as_u64().unwrap() as u32),
        if_doc_rev: None,
        position: None,
    };
    let target = ReportEditTarget::resolve(repo.as_ref(), &track)
        .await
        .unwrap();
    assert!(
        write::test_report_op(
            repo.as_ref(),
            &events,
            &write,
            crate::event::EditAuthor::Planner,
            target,
            op,
        )
        .await
        .is_err()
    );
    assert_eq!(
        super::resolve_report_for_track(repo.as_ref(), &track)
            .await
            .unwrap()
            .1
            .payload,
        before
    );
    assert_eq!(repo.events_latest_id().await.unwrap(), log);
    assert_eq!(
        calm_truth::objective_host::load_state(repo.db(), &track.into())
            .await
            .unwrap()
            .0
            .0,
        1
    );
}

#[tokio::test]
async fn objective_report_capability_absence_rolls_back() {
    let (repo, events, write, track) = fixture(false).await;
    let before = super::resolve_report_for_track(repo.as_ref(), &track)
        .await
        .unwrap()
        .1
        .payload;
    let log = repo.events_latest_id().await.unwrap();
    let target = ReportEditTarget::resolve(repo.as_ref(), &track)
        .await
        .unwrap();
    assert!(
        write::test_report_op(
            repo.as_ref(),
            &events,
            &write,
            crate::event::EditAuthor::Planner,
            target,
            create_op(),
        )
        .await
        .is_err()
    );
    assert_eq!(
        super::resolve_report_for_track(repo.as_ref(), &track)
            .await
            .unwrap()
            .1
            .payload,
        before
    );
    assert_eq!(repo.events_latest_id().await.unwrap(), log);
    assert_eq!(
        calm_truth::objective_host::load_state(repo.db(), &track.into())
            .await
            .unwrap()
            .0
            .0,
        0
    );
}

#[tokio::test]
async fn objective_evaluator_report_intent_refuses_non_planner_roles() {
    use crate::mcp_server::registry::ToolCallIdentity;
    use crate::model::CardRole;
    let (repo, events, write, track) = fixture(true).await;
    let before = super::resolve_report_for_track(repo.as_ref(), &track)
        .await
        .unwrap()
        .1
        .payload;
    let log = repo.events_latest_id().await.unwrap();
    for role in [CardRole::Assistant, CardRole::Worker, CardRole::ReportCard] {
        // Role rejection needs no session lookup or Report write.
        let identity = ToolCallIdentity {
            card_id: "non-planner".into(),
            role,
            provider: crate::session_projection_repo::AgentProvider::Codex,
            session_id: "non-planner-session".into(),
            track_id: Some(track.clone()),
            area_id: "unused".into(),
            thread_id: "unused".into(),
        };
        let error = write::agent_report_intent(
            repo.as_ref(),
            &events,
            &write,
            identity,
            write::AgentReportSource::ObjectiveEvaluator(track.clone().into()),
            create_op(),
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(CalmError::from(error), CalmError::Forbidden(_)),
            "{role:?}"
        );
    }
    assert_eq!(repo.events_latest_id().await.unwrap(), log);
    assert_eq!(
        super::resolve_report_for_track(repo.as_ref(), &track)
            .await
            .unwrap()
            .1
            .payload,
        before
    );
}
