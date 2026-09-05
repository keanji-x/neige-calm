//! Shared setup exercises report persistence, recovery admission and claim.
use crate::db::prelude::*;
use crate::db::sqlite::{
    TaskReporter, begin_immediate_tx, task_claim_pending_tx, task_fail_from_worker_tx,
};
use crate::event::EventBus;
use crate::ids::ActorId;
use crate::model::{NewCard, Task, TrackLifecycle, TrackPatch};
use crate::state::WriteContext;
use crate::track_report::{ReportDocOp, ReportEditTarget, TrackReportPayload};
use calm_types::task_recovery::TaskRecoveryRequest;
use serde_json::Value;
use std::sync::Arc;

pub(crate) struct RecoveryFixture {
    pub task: Task,
    pub repo: Arc<dyn Repo>,
    pub events: EventBus,
    pub write: WriteContext,
    block_id: String,
    block_revision: u32,
    declaration: Value,
}
impl RecoveryFixture {
    pub async fn withdraw(&self) {
        let mut payload = self.declaration.clone();
        payload["ready"] = Value::Bool(false);
        let target = ReportEditTarget::resolve(self.repo.as_ref(), &self.task.track_id)
            .await
            .unwrap();
        crate::track_report::write::rest_user_block_op(
            self.repo.as_ref(),
            &self.events,
            &self.write,
            target,
            ReportDocOp::UpsertBlock {
                id: Some(self.block_id.clone()),
                kind: "task".into(),
                content: calm_types::report_blocks::render_fence("task", &payload),
                if_rev: Some(self.block_revision),
                if_doc_rev: None,
                position: None,
            },
        )
        .await
        .unwrap();
    }
}

pub(crate) async fn recovered_claimed_task(
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
    track_id: &str,
    declaration: Value,
) -> RecoveryFixture {
    repo.track_update(
        track_id,
        TrackPatch {
            lifecycle: Some(TrackLifecycle::Working),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    repo.card_create(NewCard {
        track_id: track_id.into(),
        title: None,
        kind: "track-report".into(),
        sort: None,
        payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
    })
    .await
    .unwrap();
    let target = ReportEditTarget::resolve(repo.as_ref(), track_id)
        .await
        .unwrap();
    let (_, block) = crate::track_report::write::rest_user_block_op(
        repo.as_ref(),
        &events,
        &write,
        target,
        ReportDocOp::UpsertBlock {
            id: None,
            kind: "task".into(),
            content: calm_types::report_blocks::render_fence("task", &declaration),
            if_rev: None,
            if_doc_rev: Some(0),
            position: None,
        },
    )
    .await
    .unwrap();
    let block = block.unwrap();
    let key = declaration["key"].as_str().unwrap();
    let previous = repo.task_current_get(track_id, key).await.unwrap().unwrap();
    let monitor =
        crate::task_context::TaskContextMonitor::new(repo.clone(), events.clone(), write.clone());
    let closure = monitor.resolve_task_closure(track_id, key).await.unwrap();
    let pool = repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    task_claim_pending_tx(&mut tx, &previous.id, 1, &closure.refs, false)
        .await
        .unwrap();
    task_fail_from_worker_tx(
        &mut tx,
        &previous.id,
        track_id,
        TaskReporter::Kernel,
        "spawn-failed: before worker preparation",
        2,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let receipt = super::recover_failed_task(
        super::RecoveryContext {
            repo: repo.as_ref(),
            events: &events,
            write: &write,
        },
        track_id,
        key,
        TaskRecoveryRequest {
            expected_attempt_id: previous.id,
            idempotency_key: "prepare-fixture-recovery".into(),
            reason: "Recover preparation".into(),
        },
        ActorId::User,
    )
    .await
    .unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    task_claim_pending_tx(&mut tx, &receipt.attempt_id, 3, &closure.refs, false)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let task = repo.task_get(&receipt.attempt_id).await.unwrap().unwrap();
    RecoveryFixture {
        task,
        repo,
        events,
        write,
        block_id: block.id,
        block_revision: block.rev,
        declaration,
    }
}
