//! Compact origination is checked at the shared persisted report door, regardless of caller.
use super::write::{InitialReportTarget, structural_init_report_tx};
use super::*;
use crate::db::RepoSyncDomainRaw;
use crate::db::sqlite::SqlxRepo;
use crate::model::{NewArea, NewCard, NewTrack};
use crate::{card_role_cache::CardRoleCache, track_area_cache::TrackAreaCache};
use serde_json::{Value, json};
use std::sync::Arc;

// The agent boundary accepts caller-provided attribution; the test gate allows admission.
struct AllowRecorderAdmission;

#[async_trait::async_trait]
impl RecorderAdmissionGate for AllowRecorderAdmission {
    async fn require_admission(
        &self,
        _: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        _: RecorderAdmissionDecisionKind,
    ) -> Result<(), CalmError> {
        Ok(())
    }
}

struct Fixture {
    repo: Arc<SqlxRepo>,
    events: EventBus,
    write: WriteContext,
    track_id: String,
}

async fn fixture() -> Fixture {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "compact".into(),
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
            title: "compact".into(),
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
    Fixture {
        repo,
        events,
        write,
        track_id,
    }
}

impl Fixture {
    async fn upsert(
        &self,
        author: EditAuthor,
        id: Option<String>,
        payload: Value,
    ) -> Result<Card, CalmError> {
        let target = ReportEditTarget::resolve(self.repo.as_ref(), &self.track_id)
            .await
            .unwrap();
        let if_rev = id.as_ref().map(|id| {
            target
                .current_payload
                .blocks
                .as_ref()
                .unwrap()
                .iter()
                .find(|block| &block.id == id)
                .unwrap()
                .rev
        });
        let if_doc_rev = id.is_none().then_some(target.current_payload.doc_rev);
        write::agent_report_op(
            self.repo.as_ref(),
            &self.events,
            &self.write,
            ActorId::KernelDispatcher,
            author,
            target,
            ReportDocOp::UpsertBlock {
                id,
                kind: "task".into(),
                content: calm_types::report_blocks::render_fence("task", &payload),
                if_rev,
                if_doc_rev,
                position: None,
            },
            None,
            None,
            Arc::new(AllowRecorderAdmission),
        )
        .await
        .map(|(card, _)| card)
    }
}

fn compact() -> Value {
    json!({"key":"compact", "kind":"compact", "context":"Continue from verified task evidence.", "ready":false, "declared_by":"spec"})
}

#[tokio::test]
async fn compact_origination_fence_refuses_every_non_planner_author() {
    let f = fixture().await;
    for author in [
        EditAuthor::User,
        EditAuthor::Assistant,
        EditAuthor::Plugin,
        EditAuthor::Kernel,
    ] {
        let error = f.upsert(author, None, compact()).await.unwrap_err();
        assert!(
            matches!(error, CalmError::Forbidden(_)),
            "{author:?}: {error:?}"
        );
        let (_, _, payload) = resolve_report_for_track(f.repo.as_ref(), &f.track_id)
            .await
            .unwrap();
        assert!(
            payload
                .blocks
                .unwrap_or_default()
                .iter()
                .all(|block| block.kind != "task"),
            "refusal must roll back the block"
        );
    }
    f.upsert(EditAuthor::Planner, None, compact())
        .await
        .unwrap();
}

#[tokio::test]
async fn compact_origination_fence_refuses_kind_conversion() {
    let f = fixture().await;
    let card = f.upsert(EditAuthor::Planner, None,
        json!({"key":"compact", "kind":"terminal", "command":"true", "ready":false,"declared_by":"spec"})).await.unwrap();
    let payload: TrackReportPayload = serde_json::from_value(card.payload).unwrap();
    let id = payload
        .blocks
        .unwrap()
        .into_iter()
        .find(|block| block.kind == "task")
        .unwrap()
        .id;
    for author in [
        EditAuthor::User,
        EditAuthor::Assistant,
        EditAuthor::Plugin,
        EditAuthor::Kernel,
    ] {
        let error = f
            .upsert(author, Some(id.clone()), compact())
            .await
            .unwrap_err();
        assert!(
            matches!(error, CalmError::Forbidden(_)),
            "{author:?}: {error:?}"
        );
    }
    f.upsert(EditAuthor::Planner, Some(id), compact())
        .await
        .unwrap();
}

#[tokio::test]
async fn compact_origination_fence_allows_existing_release_edit_and_tombstone() {
    let f = fixture().await;
    let card = f
        .upsert(EditAuthor::Planner, None, compact())
        .await
        .unwrap();
    let payload: TrackReportPayload = serde_json::from_value(card.payload).unwrap();
    let id = payload
        .blocks
        .unwrap()
        .into_iter()
        .find(|block| block.kind == "task")
        .unwrap()
        .id;
    for author in [
        EditAuthor::User,
        EditAuthor::Assistant,
        EditAuthor::Plugin,
        EditAuthor::Kernel,
    ] {
        let mut payload = compact();
        payload["released_by_user"] = json!(true);
        f.upsert(author, Some(id.clone()), payload).await.unwrap();
    }
    f.upsert(
        EditAuthor::User,
        Some(id.clone()),
        json!({"key":"compact", "tombstone": {"reason":"withdrawn"},
        "declared_by":"spec", "tombstoned_by":"user"}),
    )
    .await
    .unwrap();
    // A tombstone is no longer a live compact block; revival is a new origination.
    let error = f
        .upsert(EditAuthor::User, Some(id), compact())
        .await
        .unwrap_err();
    assert!(matches!(error, CalmError::Forbidden(_)), "{error:?}");
}

#[tokio::test]
async fn compact_task_requires_user_release_under_declare_and_wait() {
    let f = fixture().await;
    sqlx::query("UPDATE tracks SET automation_policy='declare-and-wait' WHERE id=?1")
        .bind(&f.track_id)
        .execute(f.repo.pool())
        .await
        .unwrap();
    let mut payload = compact();
    payload["ready"] = json!(true);
    let card = f
        .upsert(EditAuthor::Planner, None, payload.clone())
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tasks")
            .fetch_one(f.repo.pool())
            .await
            .unwrap(),
        0
    );
    let report: TrackReportPayload = serde_json::from_value(card.payload).unwrap();
    let id = report
        .blocks
        .unwrap()
        .into_iter()
        .find(|block| block.kind == "task")
        .unwrap()
        .id;
    payload["released_by_user"] = json!(true);
    f.upsert(EditAuthor::User, Some(id), payload).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM tasks")
            .fetch_one(f.repo.pool())
            .await
            .unwrap(),
        "pending"
    );
}

#[tokio::test]
async fn compact_structural_init_refuses_live_blocks_before_writing() {
    let f = fixture().await;
    let target = ReportEditTarget::resolve(f.repo.as_ref(), &f.track_id)
        .await
        .unwrap();
    let blocks = vec![ReportBlock {
        id: "b_1234".into(),
        kind: "task".into(),
        rev: 1,
        payload: compact(),
    }];
    let mut doc = ReportDoc::from_blocks_exact("structural compact", &blocks).unwrap();
    let (summary, body) = doc.project().unwrap();
    let mut payload = TrackReportPayload::new(summary, body);
    payload.blocks = Some(blocks.clone());
    let (declarations, diagnostics) =
        calm_types::report_blocks::tasks::project_task_declarations(&blocks);
    let mut tx = f.repo.pool().begin().await.unwrap();
    let result = structural_init_report_tx(
        &mut tx,
        InitialReportTarget {
            report_card_id: target.report_card.id.as_str(),
            track_id: &f.track_id,
            payload: &payload,
            doc: &mut doc,
            declarations: &declarations,
            diagnostics: &diagnostics,
        },
    )
    .await;
    assert!(
        matches!(result, Err(CalmError::Forbidden(ref reason)) if reason.contains("Only the Planner")),
        "{result:?}"
    );
    let stored: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
        .bind(target.report_card.id.as_str())
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&stored).unwrap(),
        target.report_card.payload
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn compact_task_submission_refuses_missing_handoff() {
    let f = fixture().await;
    let mut payload = compact();
    payload.as_object_mut().unwrap().remove("context");
    let error = f
        .upsert(EditAuthor::Planner, None, payload)
        .await
        .unwrap_err();
    assert!(matches!(error, CalmError::BadRequest(_)), "{error:?}");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tasks")
            .fetch_one(f.repo.pool())
            .await
            .unwrap(),
        0
    );
}
