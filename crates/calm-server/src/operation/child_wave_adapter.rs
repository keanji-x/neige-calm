use async_trait::async_trait;
use calm_truth::decision_gate::PermissiveGate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::sqlite::{
    append_decision_event_in_tx, card_create_with_id_tx, overlay_upsert_tx, wave_create_tx,
};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventScope, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, CardId};
use crate::model::{CardRole, NewCard, NewOverlay, NewWave, RequestTheme, new_id, now_ms};
use crate::routes::waves::{spec_harness_card_payload, spec_harness_layout_payload};
use crate::wave_report::WaveReportPayload;

use super::{
    AppServerInteractOutcome, CompensationStateVersioned, Operation, PhaseTag, ProviderAdapter,
    SpawnCtx, SpawnHandle, SpawnOutcome, Tx, TxOutput, refuse_if_context_stale,
};

pub const CHILD_WAVE_KIND: &str = "child-wave";

/// PR-B moved every bounded wave-tree walk into one module in `calm-truth`,
/// so the schedulability predicate (which lives there) and this operation
/// share the same fragments AND the same static gate. Re-exported here
/// because the tree depth bound is part of this adapter's public contract.
pub use calm_truth::db::sqlite::{MAX_WAVE_TREE_DEPTH, WAVE_ROOT_DEPTH_SQL};
use calm_truth::db::sqlite::{
    WAVE_BOUNDED_PATH_SQL, can_add_tree_member, wave_tree_budget, wave_tree_member_count,
    wave_tree_spec_inventory,
};

const CHILD_WAVE_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::SpawnStarted,
    PhaseTag::SpawnSucceeded,
    PhaseTag::Succeeded,
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChildWaveOperationPayload {
    pub task_id: String,
    pub parent_wave_id: String,
    pub goal: String,
    pub acceptance: Option<String>,
    pub context: Value,
    pub cwd: Option<String>,
}

/// Stable first observation for the child spec. This function has no report
/// reader: all four fields come from the post-claim task row.
pub fn render_child_seed(payload: &ChildWaveOperationPayload) -> String {
    let acceptance = payload.acceptance.as_deref().unwrap_or("Not specified");
    let task_cwd = payload.cwd.as_deref().unwrap_or("Not specified");
    let context =
        serde_json::to_string_pretty(&payload.context).unwrap_or_else(|_| "null".to_string());
    format!(
        "# Goal\n{}\n\n# Acceptance\n{}\n\n# Context\n```json\n{}\n```\n\n# Task working directory\n{}",
        payload.goal, acceptance, context, task_cwd
    )
}

#[derive(Clone)]
pub struct ChildWaveAdapter {
    card_role_cache: crate::card_role_cache::CardRoleCache,
    wave_cove_cache: crate::wave_cove_cache::WaveCoveCache,
}

impl ChildWaveAdapter {
    pub fn new(
        card_role_cache: crate::card_role_cache::CardRoleCache,
        wave_cove_cache: crate::wave_cove_cache::WaveCoveCache,
    ) -> Self {
        Self {
            card_role_cache,
            wave_cove_cache,
        }
    }
}

async fn root_and_depth(tx: &mut Tx<'_>, parent_wave_id: &str) -> Result<(String, i64)> {
    let rows: Vec<(String, i64)> = sqlx::query_as(WAVE_ROOT_DEPTH_SQL)
        .bind(parent_wave_id)
        .bind(MAX_WAVE_TREE_DEPTH + 1)
        .fetch_all(&mut **tx)
        .await?;
    match rows.as_slice() {
        [(root, depth)] => Ok((root.clone(), *depth)),
        [] => {
            let parent_exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM waves WHERE id=?1)")
                    .bind(parent_wave_id)
                    .fetch_one(&mut **tx)
                    .await?;
            if !parent_exists {
                return Err(CalmError::NotFound(format!("wave {parent_wave_id}")));
            }
            let path: Vec<(String, i64)> = sqlx::query_as(WAVE_BOUNDED_PATH_SQL)
                .bind(parent_wave_id)
                .bind(MAX_WAVE_TREE_DEPTH + 1)
                .fetch_all(&mut **tx)
                .await?;
            let mut seen = std::collections::BTreeSet::new();
            let cycle = path.iter().any(|(id, _)| !seen.insert(id));
            Err(CalmError::Conflict(if cycle {
                "sub-wave-tree-cycle".into()
            } else {
                "sub-wave-depth-exceeded".into()
            }))
        }
        _ => Err(CalmError::Conflict("sub-wave-tree-ambiguous-root".into())),
    }
}

#[async_trait]
impl ProviderAdapter for ChildWaveAdapter {
    fn kind(&self) -> &'static str {
        CHILD_WAVE_KIND
    }

    fn phases(&self) -> &'static [PhaseTag] {
        CHILD_WAVE_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: ChildWaveOperationPayload = serde_json::from_value(input.clone())?;
        if payload.task_id.trim().is_empty() || payload.parent_wave_id.trim().is_empty() {
            return Err(CalmError::BadRequest(
                "child-wave requires task_id and parent_wave_id".into(),
            ));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: ChildWaveOperationPayload = serde_json::from_value(input.clone())?;

        // Fifth task-bound decision point. This is deliberately the first DB
        // action: a materialized frozen context may not create a child skeleton.
        refuse_if_context_stale(tx, Some(&payload.task_id)).await?;

        let (root_id, parent_depth) = root_and_depth(tx, &payload.parent_wave_id).await?;
        if parent_depth >= MAX_WAVE_TREE_DEPTH {
            return Err(CalmError::Conflict("sub-wave-depth-exceeded".into()));
        }

        // Enforcement point one for the tree budget (#985 §8). The whole tree's
        // non-terminal `declared_by='spec'` inventory — NOT this wave's — gates
        // child creation. The claiming parent task is itself one of those rows,
        // so `>=` (not `>`) is the right comparison: admitting a child at
        // `count == budget` would let the tree grow past its bound before any
        // schedulability verdict could see it.
        let budget = wave_tree_budget(tx, &root_id).await?;
        let inventory = wave_tree_spec_inventory(tx, &root_id).await?;
        if inventory >= budget {
            return Err(CalmError::Conflict(format!(
                "sub-wave-tree-budget-exhausted: wave tree rooted at {root_id} holds {inventory} \
                 unfinished spec task(s), at or over its tree_task_budget of {budget}"
            )));
        }
        let members = wave_tree_member_count(tx, &root_id).await?;
        if !can_add_tree_member(budget, members) {
            return Err(CalmError::Conflict(format!(
                "sub-wave-tree-budget-exhausted: wave tree rooted at {root_id} already has \
                 {members} member wave(s); adding one would exceed its tree_task_budget of {budget} \
                 and create a wave with zero schedulable share"
            )));
        }

        let parent: Option<(String, String)> =
            sqlx::query_as("SELECT cove_id, cwd FROM waves WHERE id=?1")
                .bind(&payload.parent_wave_id)
                .fetch_optional(&mut **tx)
                .await?;
        let (cove_id, parent_cwd) = parent.ok_or_else(|| {
            CalmError::Conflict(format!("parent wave {} is missing", payload.parent_wave_id))
        })?;
        let seed = render_child_seed(&payload);
        let child = wave_create_tx(
            tx,
            NewWave {
                cove_id: cove_id.into(),
                title: payload.goal.clone(),
                sort: None,
                cwd: parent_cwd,
                workflow_id: None,
                workflow_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            &self.wave_cove_cache,
        )
        .await?;
        // The child must inherit its parent's cove. A cross-cove parent edge
        // makes cove deletion fail its NO ACTION self-FK (tripwire test #21c).
        sqlx::query("UPDATE waves SET parent_wave_id=?1 WHERE id=?2")
            .bind(&payload.parent_wave_id)
            .bind(child.id.as_str())
            .execute(&mut **tx)
            .await?;

        let spec_card_id = new_id();
        let report_card_id = new_id();
        let spec_card = card_create_with_id_tx(
            tx,
            spec_card_id.clone(),
            NewCard {
                title: None,
                wave_id: child.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: spec_harness_card_payload(Some(seed.clone())),
            },
            CardRole::Spec,
            false,
            &self.card_role_cache,
        )
        .await?;
        let report_card = card_create_with_id_tx(
            tx,
            report_card_id.clone(),
            NewCard {
                title: None,
                wave_id: child.id.clone(),
                kind: "wave-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(WaveReportPayload::initial())?,
            },
            CardRole::ReportCard,
            false,
            &self.card_role_cache,
        )
        .await?;
        let layout = overlay_upsert_tx(
            tx,
            NewOverlay {
                plugin_id: "kernel".into(),
                entity_kind: "view".into(),
                entity_id: child.id.to_string(),
                kind: "layout".into(),
                payload: spec_harness_layout_payload(&spec_card_id, &report_card_id),
            },
        )
        .await?;

        let stamped = sqlx::query(
            "UPDATE tasks SET child_wave_id=COALESCE(child_wave_id,?1),updated_at_ms=?2 \
             WHERE id=?3 AND status='dispatched' \
               AND (child_wave_id IS NULL OR child_wave_id=?1)",
        )
        .bind(child.id.as_str())
        .bind(now_ms())
        .bind(&payload.task_id)
        .execute(&mut **tx)
        .await?;
        if stamped.rows_affected() == 0 {
            return Err(CalmError::Conflict(format!(
                "child-wave parent task {} is not dispatched",
                payload.task_id
            )));
        }

        let actor = ActorId::KernelDispatcher;
        let wave_scope = EventScope::Wave {
            wave: child.id.clone(),
            cove: child.cove_id.clone(),
        };
        let entries = [
            (
                wave_scope.clone(),
                Event::WaveUpdated(crate::event::WaveUpdatedPayload::new(child.clone(), None)),
            ),
            (
                EventScope::Card {
                    card: spec_card.id.clone(),
                    wave: child.id.clone(),
                    cove: child.cove_id.clone(),
                },
                Event::CardAdded(spec_card),
            ),
            (
                EventScope::Card {
                    card: report_card.id.clone(),
                    wave: child.id.clone(),
                    cove: child.cove_id.clone(),
                },
                Event::CardAdded(report_card),
            ),
            (wave_scope, Event::OverlaySet(layout)),
        ];
        let mut envelopes = Vec::with_capacity(entries.len());
        for (scope, event) in entries {
            let id = append_decision_event_in_tx(tx, &PermissiveGate, &actor, &scope, None, &event)
                .await?;
            envelopes.push(BroadcastEnvelope {
                id,
                event_version: SYNC_EVENT_VERSION,
                actor: actor.clone(),
                scope,
                event,
            });
        }

        let result = json!({
            "child_wave_id": child.id,
            "spec_card_id": CardId::from(spec_card_id),
            "report_card_id": CardId::from(report_card_id),
            "seed": seed,
            "cwd": child.cwd,
        });
        let mut output = TxOutput::new("wave", Some(child.id.to_string()), result.clone());
        output.data = result;
        output.post_commit_events = envelopes;
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: vec![],
        })
    }

    async fn compensate_step(
        &self,
        _step: &super::CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::{SqlxRepo, cove_create_tx, cove_delete_tx};
    use crate::model::{NewCove, Task, TaskKind, TaskStatus};
    use crate::operation::Phase;

    fn operation(payload: Value) -> Operation {
        Operation {
            id: "op-child".into(),
            operation_key: "op-key".into(),
            kind: CHILD_WAVE_KIND.into(),
            idempotency_key: Some("task".into()),
            payload_hash: "hash".into(),
            target_type: "unknown".into(),
            target_id: None,
            target: Value::Null,
            payload,
            tx_output: None,
            phase: Phase::Pending,
            phase_detail: None,
            attempt: 0,
            last_error: None,
            compensation_state: None,
            lease_owner: None,
            lease_until_ms: None,
            spawn_artifacts: None,
            parked_at_ms: None,
            parked_deadline_ms: None,
        }
    }

    async fn seed_parent(repo: &SqlxRepo, non_default_lifecycle_metadata: bool) -> String {
        let mut tx = repo.pool().begin().await.unwrap();
        let cove = cove_create_tx(
            &mut tx,
            NewCove {
                name: "c".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let wave = wave_create_tx(
            &mut tx,
            NewWave {
                cove_id: cove.id,
                title: "parent".into(),
                sort: None,
                cwd: "/parent-cwd".into(),
                workflow_id: Some("must-not-inherit".into()),
                workflow_input: Some(json!({"must":"not-inherit"})),
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            repo.wave_cove_cache(),
        )
        .await
        .unwrap();
        if non_default_lifecycle_metadata {
            // Acceptance #5 alone needs negative inheritance sentinels. Other
            // adapter tests keep a live Draft parent so their fixtures do not
            // normalize "terminal parents may spawn children" as valid.
            sqlx::query(
                "UPDATE waves SET archived_at=101,pinned_at=102,lifecycle='done',terminal_at=103 \
                 WHERE id=?1",
            )
            .bind(wave.id.as_str())
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
        wave.id.to_string()
    }

    async fn seed_task(repo: &SqlxRepo, wave_id: &str, stale: bool) -> Task {
        seed_task_with_key(repo, wave_id, "child", stale).await
    }

    async fn seed_task_with_key(repo: &SqlxRepo, wave_id: &str, key: &str, stale: bool) -> Task {
        let now = now_ms();
        let task = Task {
            id: format!("{wave_id}:{key}"),
            wave_id: wave_id.into(),
            key: key.into(),
            kind: TaskKind::Codex,
            goal: "frozen-goal".into(),
            context_json: json!({"frozen":"context"}).to_string(),
            acceptance_criteria: Some("frozen-acceptance".into()),
            cwd: Some("/task-only-cwd".into()),
            depends_on_json: "[]".into(),
            priority: 0,
            gate_json: None,
            status: TaskStatus::Dispatched,
            status_detail: None,
            worker_card_id: None,
            gate_result_json: None,
            gate_attempt: 0,
            gate_pid: None,
            gate_pid_starttime: None,
            gate_pid_boot_id: None,
            running_deadline_ms: None,
            context_stale_at_ms: stale.then_some(now),
            declared_by: "spec".into(),
            spawn: "sub-wave".into(),
            origin: "block".into(),
            created_at_ms: now,
            updated_at_ms: now,
            finished_at_ms: None,
        };
        let mut tx = repo.pool().begin().await.unwrap();
        crate::db::sqlite::task_insert_tx(&mut tx, &task)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        if stale {
            sqlx::query("UPDATE tasks SET context_stale_at_ms=?1 WHERE id=?2")
                .bind(now)
                .bind(&task.id)
                .execute(repo.pool())
                .await
                .unwrap();
        }
        task
    }

    fn payload(task: &Task) -> ChildWaveOperationPayload {
        ChildWaveOperationPayload {
            task_id: task.id.clone(),
            parent_wave_id: task.wave_id.clone(),
            goal: task.goal.clone(),
            acceptance: task.acceptance_criteria.clone(),
            context: serde_json::from_str(&task.context_json).unwrap(),
            cwd: task.cwd.clone(),
        }
    }

    /// Both fragments this adapter runs keep their only cycle-termination
    /// guard. The crate-wide property gate independently scans every SQL
    /// string touching `parent_wave_id`; there is intentionally no registry.
    #[test]
    fn upward_cte_keeps_its_only_cycle_termination_guard() {
        for sql in [WAVE_ROOT_DEPTH_SQL, WAVE_BOUNDED_PATH_SQL] {
            assert!(sql.contains("WHERE up.depth <= ?2"));
            assert!(sql.contains("UNION ALL"));
        }
    }

    #[tokio::test]
    async fn acceptance_5_child_seed_uses_all_four_frozen_fields_and_parent_cwd() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let parent = seed_parent(&repo, true).await;
        let task = seed_task(&repo, &parent, false).await;
        // The live report deliberately disagrees with every frozen field,
        // without marking the task stale. This DB seam proves that the real
        // adapter consumes the operation payload frozen from `tasks`, rather
        // than re-reading the current declaration.
        let report = WaveReportPayload {
            schema_version: WaveReportPayload::SCHEMA_VERSION,
            doc_rev: 9,
            summary: String::new(),
            body: String::new(),
            blocks: Some(vec![calm_types::wave_report::ReportBlock {
                id: "b_current".into(),
                kind: "task".into(),
                rev: 4,
                payload: json!({
                    "key":"child", "kind":"codex", "spawn":"sub-wave",
                    "goal":"current-goal", "acceptance":"current-acceptance",
                    "context":{"current":"context"}, "cwd":"/current-cwd"
                }),
            }]),
        };
        sqlx::query(
            "INSERT INTO cards(id,wave_id,kind,sort,payload,role,deletable,created_at,updated_at) \
             VALUES('current-report',?1,'wave-report',-1,?2,'reportcard',0,1,1)",
        )
        .bind(&parent)
        .bind(serde_json::to_string(&report).unwrap())
        .execute(repo.pool())
        .await
        .unwrap();
        let input = serde_json::to_value(payload(&task)).unwrap();
        let adapter = ChildWaveAdapter::new(
            repo.card_role_cache().clone(),
            repo.wave_cove_cache().clone(),
        );
        let mut tx = repo.pool().begin().await.unwrap();
        let output = adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            output.data["seed"],
            "# Goal\nfrozen-goal\n\n# Acceptance\nfrozen-acceptance\n\n# Context\n```json\n{\n  \"frozen\": \"context\"\n}\n```\n\n# Task working directory\n/task-only-cwd"
        );
        let spec_card_id = output.data["spec_card_id"].as_str().unwrap();
        let spec_payload: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
            .bind(spec_card_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
        for frozen_value in [
            "frozen-goal",
            "frozen-acceptance",
            "frozen",
            "/task-only-cwd",
        ] {
            assert!(spec_payload.contains(frozen_value), "{spec_payload}");
        }
        for current_value in [
            "current-goal",
            "current-acceptance",
            "current-cwd",
            "current",
        ] {
            assert!(!spec_payload.contains(current_value), "{spec_payload}");
        }
        assert_eq!(output.data["cwd"], "/parent-cwd");
        let child_id = output.data["child_wave_id"].as_str().unwrap();
        type InheritedChildFields = (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        );
        let inherited: InheritedChildFields = sqlx::query_as(
            "SELECT cwd,workflow_id,workflow_input,purpose,lifecycle,archived_at,pinned_at,terminal_at \
             FROM waves WHERE id=?1",
        )
        .bind(child_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
        assert_eq!(inherited.0, "/parent-cwd");
        assert_eq!(inherited.1, None, "workflow_id must not inherit");
        assert_eq!(inherited.2, None, "workflow_input must not inherit");
        assert_eq!(inherited.3, None, "purpose must not inherit");
        assert_eq!(
            inherited.4, "draft",
            "child must stay Draft before bootstrap"
        );
        assert_eq!(inherited.5, None, "archived_at must not inherit");
        assert_eq!(inherited.6, None, "pinned_at must not inherit");
        assert_eq!(inherited.7, None, "terminal_at must not inherit");
    }

    #[tokio::test]
    async fn acceptance_6_real_adapter_writes_direct_parent_and_enforces_depth_three() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let root = seed_parent(&repo, false).await;
        let mut direct_parent = root.clone();
        for level in 1..=3 {
            let task = seed_task(&repo, &direct_parent, false).await;
            let input = serde_json::to_value(payload(&task)).unwrap();
            let adapter = ChildWaveAdapter::new(
                repo.card_role_cache().clone(),
                repo.wave_cove_cache().clone(),
            );
            let mut tx = repo.pool().begin().await.unwrap();
            let output = adapter
                .prepare_tx(&mut tx, &input, &operation(input.clone()))
                .await
                .unwrap();
            tx.commit().await.unwrap();
            let child = output.data["child_wave_id"].as_str().unwrap().to_string();
            let stored_parent: String =
                sqlx::query_scalar("SELECT parent_wave_id FROM waves WHERE id=?1")
                    .bind(&child)
                    .fetch_one(repo.pool())
                    .await
                    .unwrap();
            assert_eq!(
                stored_parent, direct_parent,
                "level {level} must point to its direct parent"
            );
            direct_parent = child;
        }
        let cross_cove_edges: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM waves child JOIN waves parent \
             ON parent.id=child.parent_wave_id WHERE child.cove_id<>parent.cove_id",
        )
        .fetch_one(repo.pool())
        .await
        .unwrap();
        assert_eq!(cross_cove_edges, 0, "real adapter must inherit parent cove");
        let task = seed_task(&repo, &direct_parent, false).await;
        let input = serde_json::to_value(payload(&task)).unwrap();
        let adapter = ChildWaveAdapter::new(
            repo.card_role_cache().clone(),
            repo.wave_cove_cache().clone(),
        );
        let mut tx = repo.pool().begin().await.unwrap();
        let error = adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("sub-wave-depth-exceeded"));
    }

    #[tokio::test]
    async fn acceptance_21c_real_adapter_never_writes_a_cross_cove_edge() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let parent = seed_parent(&repo, false).await;
        let second_cove = {
            let mut tx = repo.pool().begin().await.unwrap();
            let cove = cove_create_tx(
                &mut tx,
                NewCove {
                    name: "unrelated-cove".into(),
                    color: "#111".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            cove.id.to_string()
        };
        let task = seed_task(&repo, &parent, false).await;
        let input = serde_json::to_value(payload(&task)).unwrap();
        let adapter = ChildWaveAdapter::new(
            repo.card_role_cache().clone(),
            repo.wave_cove_cache().clone(),
        );
        let mut tx = repo.pool().begin().await.unwrap();
        adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let cross_cove_edges: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM waves child JOIN waves parent \
             ON parent.id=child.parent_wave_id WHERE child.cove_id<>parent.cove_id",
        )
        .fetch_one(repo.pool())
        .await
        .unwrap();
        assert_eq!(cross_cove_edges, 0);

        // The unrelated cove is independently deletable: the adapter did not
        // accidentally route its child there and create a NO ACTION tripwire.
        let mut tx = repo.pool().begin().await.unwrap();
        cove_delete_tx(&mut tx, &second_cove).await.unwrap();
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn acceptance_7_two_cycle_fails_fast_with_cycle_reason() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let a = seed_parent(&repo, false).await;
        let b = {
            let task = seed_task(&repo, &a, false).await;
            let input = serde_json::to_value(payload(&task)).unwrap();
            let adapter = ChildWaveAdapter::new(
                repo.card_role_cache().clone(),
                repo.wave_cove_cache().clone(),
            );
            let mut tx = repo.pool().begin().await.unwrap();
            let output = adapter
                .prepare_tx(&mut tx, &input, &operation(input.clone()))
                .await
                .unwrap();
            tx.commit().await.unwrap();
            output.data["child_wave_id"].as_str().unwrap().to_string()
        };
        sqlx::query("UPDATE waves SET parent_wave_id=?1 WHERE id=?2")
            .bind(&b)
            .bind(&a)
            .execute(repo.pool())
            .await
            .unwrap();
        let mut tx = repo.pool().begin().await.unwrap();
        let error = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            root_and_depth(&mut tx, &a),
        )
        .await
        .expect("bounded ancestor query must return before 500ms")
        .unwrap_err();
        assert!(error.to_string().contains("sub-wave-tree-cycle"));
    }

    #[tokio::test]
    async fn acceptance_8_missing_parent_is_not_misreported_as_depth_exhaustion() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let mut tx = repo.pool().begin().await.unwrap();
        let error = root_and_depth(&mut tx, "missing").await.unwrap_err();
        assert!(
            matches!(&error, CalmError::NotFound(message) if message == "wave missing"),
            "missing parent must retain its diagnostic reason, got {error}"
        );
    }

    /// PR-B enforcement point one. The inventory counted is the WHOLE tree's
    /// non-terminal spec rows. At B=2, inventory is exactly 2 while member
    /// admission N=1 -> 2 is legal, so ONLY the inventory guard can refuse.
    /// At B=3 both guards admit. This keeps the inventory `>=` tripwire
    /// independent from the later member-count guard.
    #[tokio::test]
    async fn acceptance_tree_budget_refuses_child_creation_when_the_tree_is_full() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let parent = seed_parent(&repo, false).await;
        let task = seed_task(&repo, &parent, false).await;
        let _other = seed_task_with_key(&repo, &parent, "other-live-task", false).await;
        let input = serde_json::to_value(payload(&task)).unwrap();
        let adapter = ChildWaveAdapter::new(
            repo.card_role_cache().clone(),
            repo.wave_cove_cache().clone(),
        );
        sqlx::query("UPDATE waves SET tree_task_budget=2 WHERE id=?1")
            .bind(&parent)
            .execute(repo.pool())
            .await
            .unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM waves")
            .fetch_one(repo.pool())
            .await
            .unwrap();

        let mut tx = repo.pool().begin().await.unwrap();
        let error = adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("sub-wave-tree-budget-exhausted"),
            "{error}"
        );
        assert!(error.to_string().contains(&parent), "{error}");
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM waves")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(before, after, "a refused creation must write nothing");
        tx.rollback().await.unwrap();

        // Raising the ROOT's budget (the only place it lives) admits it.
        sqlx::query("UPDATE waves SET tree_task_budget=3 WHERE id=?1")
            .bind(&parent)
            .execute(repo.pool())
            .await
            .unwrap();
        let mut tx = repo.pool().begin().await.unwrap();
        let output = adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let child = output.data["child_wave_id"].as_str().unwrap().to_string();
        let child_budget: Option<i64> =
            sqlx::query_scalar("SELECT tree_task_budget FROM waves WHERE id=?1")
                .bind(&child)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(
            child_budget, None,
            "the child must not carry a budget of its own"
        );
    }

    /// Compatibility of the two enforcement points: after the first child is
    /// admitted under B=2 and its parent task finishes, inventory alone would
    /// allow another child. The member bound must still refuse it because an
    /// admitted N=3 tree would assign a zero share.
    #[tokio::test]
    async fn acceptance_tree_budget_never_admits_a_zero_share_member() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let parent = seed_parent(&repo, false).await;
        sqlx::query("UPDATE waves SET tree_task_budget=2 WHERE id=?1")
            .bind(&parent)
            .execute(repo.pool())
            .await
            .unwrap();
        let first = seed_task_with_key(&repo, &parent, "first-child", false).await;
        let input = serde_json::to_value(payload(&first)).unwrap();
        let adapter = ChildWaveAdapter::new(
            repo.card_role_cache().clone(),
            repo.wave_cove_cache().clone(),
        );
        let mut tx = repo.pool().begin().await.unwrap();
        adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        sqlx::query("UPDATE tasks SET status='done',finished_at_ms=1 WHERE id=?1")
            .bind(&first.id)
            .execute(repo.pool())
            .await
            .unwrap();

        let second = seed_task_with_key(&repo, &parent, "second-child", false).await;
        let input = serde_json::to_value(payload(&second)).unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM waves")
            .fetch_one(repo.pool())
            .await
            .unwrap();
        let mut tx = repo.pool().begin().await.unwrap();
        let error = adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("sub-wave-tree-budget-exhausted")
                && error.to_string().contains("2 member wave(s)")
                && error.to_string().contains("zero schedulable share"),
            "{error}"
        );
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM waves")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(before, after, "a shape-refused creation must write nothing");
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn acceptance_10_child_adapter_stale_fence_precedes_every_side_effect() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let parent = seed_parent(&repo, false).await;
        let task = seed_task(&repo, &parent, true).await;
        let input = serde_json::to_value(payload(&task)).unwrap();
        let adapter = ChildWaveAdapter::new(
            repo.card_role_cache().clone(),
            repo.wave_cove_cache().clone(),
        );
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM waves")
            .fetch_one(repo.pool())
            .await
            .unwrap();
        let mut tx = repo.pool().begin().await.unwrap();
        let error = adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("context-stale"));
        drop(tx);
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM waves")
            .fetch_one(repo.pool())
            .await
            .unwrap();
        assert_eq!(before, after);
    }
}
