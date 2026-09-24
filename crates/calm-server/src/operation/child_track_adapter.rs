use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::sqlite::{
    AttachedInheritedPath, TrackWorkspacePlan, append_decision_event_in_tx, card_create_with_id_tx,
    overlay_upsert_tx, track_create_tx,
};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventScope, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, CardId};
use crate::model::{
    CardRole, NewCard, NewOverlay, NewTrack, RequestTheme, TrackWorkspace, TrackWorkspaceKind,
    new_id, now_ms,
};
use crate::routes::tracks::{planner_harness_card_payload, planner_harness_layout_payload};
use crate::track_report::{TrackReportPayload, tasks_rebuild_tree_tx};

use super::{
    AppServerInteractOutcome, CompensationStateVersioned, Operation, PhaseTag, ProviderAdapter,
    SpawnCtx, SpawnHandle, SpawnOutcome, Tx, TxOutput, refuse_if_context_stale,
};

pub const CHILD_TRACK_KIND: &str = "child-track";

/// A child track's Planner runs on its parent Planner's backend; a parent Planner card that is
/// not a harness card (`PlannerBinding` is the one decoder) refuses the child rather than guessing.
async fn parent_planner_provider_tx(
    tx: &mut Tx<'_>,
    parent_track_id: &str,
) -> Result<crate::session_projection_repo::AgentProvider> {
    let card = sqlx::query_as::<_, crate::db::rows::CardRow>(
        "SELECT id, track_id, kind, sort, payload, title, deletable, created_at, updated_at \
           FROM cards WHERE track_id=?1 AND role='planner'",
    )
    .bind(parent_track_id)
    .fetch_optional(&mut **tx)
    .await?
    .map(crate::model::Card::from);
    card.as_ref()
        .and_then(|card| {
            crate::harness::profile::PlannerBinding::from_card(card, CardRole::Planner)
        })
        .map(|binding| binding.provider)
        .ok_or_else(|| {
            CalmError::Conflict(format!(
                "parent track {parent_track_id} has no Planner card naming its planner_provider"
            ))
        })
}

/// Re-exported because the tree depth bound is part of this adapter's public contract.
pub use calm_truth::db::sqlite::{MAX_TRACK_TREE_DEPTH, TRACK_ROOT_DEPTH_SQL};
use calm_truth::db::sqlite::{
    TRACK_BOUNDED_PATH_SQL, can_add_tree_member, track_tree_budget, track_tree_member_count,
    track_tree_planner_inventory,
};

const CHILD_TRACK_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::SpawnStarted,
    PhaseTag::SpawnSucceeded,
    PhaseTag::Succeeded,
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChildTrackOperationPayload {
    pub task_id: String,
    pub parent_track_id: String,
    pub goal: String,
    pub acceptance: Option<String>,
    pub context: Value,
    pub cwd: Option<String>,
}

/// Stable first observation for the child planner; all four fields come from the post-claim task row, never a report reader.
pub fn render_child_seed(payload: &ChildTrackOperationPayload) -> String {
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
pub struct ChildTrackAdapter {
    card_role_cache: crate::card_role_cache::CardRoleCache,
    track_area_cache: crate::track_area_cache::TrackAreaCache,
    /// The managed workspace root; the child's own workspace is derived under it and materialized in `prepare_tx`.
    workspace_root: std::path::PathBuf,
}

impl ChildTrackAdapter {
    pub fn new(
        card_role_cache: crate::card_role_cache::CardRoleCache,
        track_area_cache: crate::track_area_cache::TrackAreaCache,
        workspace_root: std::path::PathBuf,
    ) -> Self {
        Self {
            card_role_cache,
            track_area_cache,
            workspace_root,
        }
    }
}

/// Managed parent → the child allocates its own managed workspace (two rows must never share a managed directory, recycling is by directory).
/// Attached parent → the child inherits the same attached path. Frozen on both branches: the harness bootstraps on this path immediately.
fn child_workspace_plan(
    parent: &TrackWorkspace,
    workspace_root: &std::path::Path,
) -> Result<TrackWorkspacePlan> {
    Ok(match parent.kind {
        TrackWorkspaceKind::Managed => {
            TrackWorkspacePlan::ManagedFrozenUnder(workspace_root.to_path_buf())
        }
        // `AttachedInheritedPath::new` refuses a path inside the managed root; unreachable from here, checked anyway because the check belongs to the type.
        TrackWorkspaceKind::Attached => TrackWorkspacePlan::InheritAttachedFrozen(
            AttachedInheritedPath::new(parent.path.clone(), workspace_root)?,
        ),
    })
}

async fn root_and_depth(tx: &mut Tx<'_>, parent_track_id: &str) -> Result<(String, i64)> {
    let rows: Vec<(String, i64)> = sqlx::query_as(TRACK_ROOT_DEPTH_SQL)
        .bind(parent_track_id)
        .bind(MAX_TRACK_TREE_DEPTH + 1)
        .fetch_all(&mut **tx)
        .await?;
    match rows.as_slice() {
        [(root, depth)] => Ok((root.clone(), *depth)),
        [] => {
            let parent_exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tracks WHERE id=?1)")
                    .bind(parent_track_id)
                    .fetch_one(&mut **tx)
                    .await?;
            if !parent_exists {
                return Err(CalmError::NotFound(format!("track {parent_track_id}")));
            }
            let path: Vec<(String, i64)> = sqlx::query_as(TRACK_BOUNDED_PATH_SQL)
                .bind(parent_track_id)
                .bind(MAX_TRACK_TREE_DEPTH + 1)
                .fetch_all(&mut **tx)
                .await?;
            let mut seen = std::collections::BTreeSet::new();
            let cycle = path.iter().any(|(id, _)| !seen.insert(id));
            Err(CalmError::Conflict(if cycle {
                "sub-track-tree-cycle".into()
            } else {
                "sub-track-depth-exceeded".into()
            }))
        }
        _ => Err(CalmError::Conflict("sub-track-tree-ambiguous-root".into())),
    }
}

#[async_trait]
impl ProviderAdapter for ChildTrackAdapter {
    fn kind(&self) -> &'static str {
        CHILD_TRACK_KIND
    }

    fn phases(&self) -> &'static [PhaseTag] {
        CHILD_TRACK_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: ChildTrackOperationPayload = serde_json::from_value(input.clone())?;
        if payload.task_id.trim().is_empty() || payload.parent_track_id.trim().is_empty() {
            return Err(CalmError::BadRequest(
                "child-track requires task_id and parent_track_id".into(),
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
        let payload: ChildTrackOperationPayload = serde_json::from_value(input.clone())?;

        // Deliberately the first DB action: a materialized frozen context may not create a child skeleton.
        refuse_if_context_stale(tx, Some(&payload.task_id)).await?;

        let (root_id, parent_depth) = root_and_depth(tx, &payload.parent_track_id).await?;
        if parent_depth >= MAX_TRACK_TREE_DEPTH {
            return Err(CalmError::Conflict("sub-track-depth-exceeded".into()));
        }

        // The whole tree's non-terminal `declared_by='spec'` inventory gates child creation. The claiming parent task is itself one of
        // those rows, so `>=` (not `>`): admitting a child at `count == budget` would let the tree grow past its bound.
        let budget = track_tree_budget(tx, &root_id).await?;
        let inventory = track_tree_planner_inventory(tx, &root_id).await?;
        if inventory >= budget {
            return Err(CalmError::Conflict(format!(
                "sub-track-tree-budget-exhausted: track tree rooted at {root_id} holds {inventory} \
                 unfinished planner task(s), at or over its tree_task_budget of {budget}"
            )));
        }
        let members = track_tree_member_count(tx, &root_id).await?;
        if !can_add_tree_member(budget, members) {
            return Err(CalmError::Conflict(format!(
                "sub-track-tree-budget-exhausted: track tree rooted at {root_id} already has \
                 {members} member track(s); adding one would exceed its tree_task_budget of {budget} \
                 and create a track with zero schedulable share"
            )));
        }

        // The parent's WORKSPACE KIND decides the child's plan; the parent's path is read only on the attached branch.
        let parent: Option<(String, Option<String>, String, String)> = sqlx::query_as(
            "SELECT area_id, plugin_scope, workspace_kind, workspace_path FROM tracks WHERE id=?1",
        )
        .bind(&payload.parent_track_id)
        .fetch_optional(&mut **tx)
        .await?;
        let (area_id, parent_plugin_scope, parent_workspace_kind, parent_workspace_path) =
            parent.ok_or_else(|| {
                CalmError::Conflict(format!(
                    "parent track {} is missing",
                    payload.parent_track_id
                ))
            })?;
        let parent_workspace = TrackWorkspace {
            kind: TrackWorkspaceKind::try_from(parent_workspace_kind)
                .map_err(CalmError::Internal)?,
            path: parent_workspace_path,
            // Not read by `child_workspace_plan`; the child's own stamp is set by the plan, not copied.
            frozen_at: None,
        };
        let plan = child_workspace_plan(&parent_workspace, &self.workspace_root)?;
        let planner_provider = parent_planner_provider_tx(tx, &payload.parent_track_id).await?;
        let seed = render_child_seed(&payload);
        let child = track_create_tx(
            tx,
            NewTrack {
                area_id: area_id.into(),
                title: payload.goal.clone(),
                sort: None,
                // Ignored by both plans; empty rather than the parent's path so no plan can pick up an inherited path through a dead field.
                cwd: String::new(),
                template_id: None,
                plugin_scope: parent_plugin_scope,
                template_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            None,
            &plan,
            // A parent made from a recipe does not pass its origin down: a recipe id here would claim the child carries content it never got.
            None,
            &self.track_area_cache,
        )
        .await?;
        // Managed branch: real work, and the ownership marker names the CHILD, so a child that ended up on the parent's managed path is refused here.
        // Attached branch: a no-op by contract — `materialize_workspace` never creates, `git init`s or writes to a directory the user owns.
        crate::workspace_materialize::materialize_workspace(
            &child.workspace,
            &self.workspace_root,
            child.id.as_str(),
        )?;
        // The child must inherit its parent's area: a cross-area parent edge makes area deletion fail its NO ACTION self-FK.
        sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
            .bind(&payload.parent_track_id)
            .bind(child.id.as_str())
            .execute(&mut **tx)
            .await?;

        let planner_card_id = new_id();
        let report_card_id = new_id();
        let planner_card = card_create_with_id_tx(
            tx,
            planner_card_id.clone(),
            NewCard {
                title: None,
                track_id: child.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: planner_harness_card_payload(Some(seed.clone()), planner_provider),
            },
            CardRole::Planner,
            false,
            &self.card_role_cache,
        )
        .await?;
        let report_card = card_create_with_id_tx(
            tx,
            report_card_id.clone(),
            NewCard {
                title: None,
                track_id: child.id.clone(),
                kind: "track-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(TrackReportPayload::initial())?,
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
                payload: planner_harness_layout_payload(&planner_card_id, &report_card_id),
            },
        )
        .await?;

        let stamped = sqlx::query(
            "UPDATE tasks SET child_track_id=COALESCE(child_track_id,?1),updated_at_ms=?2 \
             WHERE id=?3 AND status='dispatched' \
               AND (child_track_id IS NULL OR child_track_id=?1)",
        )
        .bind(child.id.as_str())
        .bind(now_ms())
        .bind(&payload.task_id)
        .execute(&mut **tx)
        .await?;
        if stamped.rows_affected() == 0 {
            return Err(CalmError::Conflict(format!(
                "child-track parent task {} is not dispatched",
                payload.task_id
            )));
        }

        // `N` has changed, so every old member's deterministic share may have shrunk; rebuild the whole tree before this transaction can expose the child.
        let projections = tasks_rebuild_tree_tx(tx, &root_id).await?;

        let actor = ActorId::KernelDispatcher;
        let track_scope = EventScope::Track {
            track: child.id.clone(),
            area: child.area_id.clone(),
        };
        let mut entries = vec![
            (
                actor.clone(),
                track_scope.clone(),
                Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(child.clone(), None)),
            ),
            (
                actor.clone(),
                EventScope::Card {
                    card: planner_card.id.clone(),
                    track: child.id.clone(),
                    area: child.area_id.clone(),
                },
                Event::CardAdded(planner_card),
            ),
            (
                actor.clone(),
                EventScope::Card {
                    card: report_card.id.clone(),
                    track: child.id.clone(),
                    area: child.area_id.clone(),
                },
                Event::CardAdded(report_card),
            ),
            (actor.clone(), track_scope, Event::OverlaySet(layout)),
        ];
        for (projected_track, projection) in projections {
            if !projection.changed_keys.is_empty() {
                entries.push((
                    actor.clone(),
                    EventScope::Track {
                        track: projected_track.id.clone(),
                        area: projected_track.area_id.clone(),
                    },
                    Event::PlanUpdated {
                        track_id: projected_track.id,
                        changed_keys: projection.changed_keys,
                        agent_message: None,
                    },
                ));
            }
            entries.extend(projection.kernel_events);
        }
        let mut envelopes = Vec::with_capacity(entries.len());
        for (event_actor, scope, event) in entries {
            let id = append_decision_event_in_tx(tx, &event_actor, &scope, None, &event).await?;
            envelopes.push(BroadcastEnvelope {
                id,
                event_version: SYNC_EVENT_VERSION,
                actor: event_actor,
                scope,
                event,
            });
        }

        // `cwd` here is not a convenience copy: `scheduler::drive_child_track` never re-reads the track row, it takes `cwd` from THIS result
        // (including the persisted `tx_output` of an older operation on an idempotency collision) and hands it to `planner-harness-start`.
        let result = json!({
            "child_track_id": child.id,
            "planner_card_id": CardId::from(planner_card_id),
            "report_card_id": CardId::from(report_card_id),
            "seed": seed,
            "cwd": child.workspace.path,
        });
        let mut output = TxOutput::new("track", Some(child.id.to_string()), result.clone());
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
mod provider_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_role_cache::CardRoleCache;
    use crate::db::RepoOutOfDomain;
    use crate::db::sqlite::{
        SqlxRepo, area_create_tx, area_delete_tx, task_claim_pending_tx, track_update_tx,
    };
    use crate::event::EventBus;
    use crate::forge_trust::trusted_forge_plugin;
    use crate::mcp_server::registry::AppContext;
    use crate::mcp_server::tool_visibility::{TrackPluginScope, plugin_scope_for_track};
    use crate::model::{NewArea, Task, TaskKind, TaskStatus, TrackPatch};
    use crate::operation::Phase;
    use crate::operation::child_track_adapter::provider_tests;
    use crate::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
    use crate::state::WriteContext;
    use crate::track_area_cache::TrackAreaCache;
    use crate::track_report::tasks_rebuild_tx;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::{Instant, sleep};

    pub(super) fn operation(payload: Value) -> Operation {
        Operation {
            id: "op-child".into(),
            operation_key: "op-key".into(),
            kind: CHILD_TRACK_KIND.into(),
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

    /// A REAL workspace root: every child materializes its own managed directory, so a fake root would fail materialization.
    /// One process-wide `TempDir`, deliberately never dropped, so no test can observe a root removed by another test's teardown.
    pub(super) fn test_workspace_root() -> std::path::PathBuf {
        static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| tempfile::TempDir::new().expect("adapter test workspace root"))
            .path()
            .to_path_buf()
    }

    pub(super) async fn seed_parent(
        repo: &SqlxRepo,
        non_default_lifecycle_metadata: bool,
    ) -> String {
        let mut tx = repo.pool().begin().await.unwrap();
        let area = area_create_tx(
            &mut tx,
            NewArea {
                name: "c".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let track = track_create_tx(
            &mut tx,
            NewTrack {
                area_id: area.id,
                title: "parent".into(),
                sort: None,
                cwd: "/parent-cwd".into(),
                template_id: Some("must-not-inherit".into()),
                plugin_scope: Some("must-inherit-plugin".into()),
                template_input: Some(json!({"must":"not-inherit"})),
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            None,
            &TrackWorkspacePlan::AttachedFromCwd,
            None,
            repo.track_area_cache(),
        )
        .await
        .unwrap();
        if non_default_lifecycle_metadata {
            // Only this acceptance needs negative inheritance sentinels; other tests keep a live Draft parent.
            sqlx::query(
                "UPDATE tracks SET archived_at=101,pinned_at=102,lifecycle='done',terminal_at=103 \
                 WHERE id=?1",
            )
            .bind(track.id.as_str())
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
        provider_tests::ensure_planner_card(repo, track.id.as_str()).await;
        track.id.to_string()
    }

    pub(super) async fn seed_task(repo: &SqlxRepo, track_id: &str, stale: bool) -> Task {
        seed_task_with_key(repo, track_id, "child", stale).await
    }

    async fn seed_task_with_key(repo: &SqlxRepo, track_id: &str, key: &str, stale: bool) -> Task {
        provider_tests::ensure_planner_card(repo, track_id).await;
        let now = now_ms();
        let task = Task {
            id: format!("{track_id}:{key}"),
            track_id: track_id.into(),
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
            created_at_ms: now,
            updated_at_ms: now,
            finished_at_ms: None,
        };
        let mut tx = repo.pool().begin().await.unwrap();
        crate::test_support::insert_task_tx(&mut tx, &task)
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

    async fn project_pending_tasks(
        repo: &SqlxRepo,
        track_id: &str,
        prefix: &str,
        count: usize,
    ) -> Vec<String> {
        let blocks = (0..count)
            .map(|index| calm_types::track_report::ReportBlock {
                id: format!("b_{prefix}_{index}"),
                rev: 1,
                kind: "task".into(),
                payload: json!({
                    "key": format!("{prefix}-{index}"),
                    "kind": "codex",
                    "goal": format!("{prefix} goal {index}"),
                    "acceptance": "done",
                    "no_gate_reason": "not needed",
                    "declared_by": "spec",
                    "ready": true
                }),
            })
            .collect::<Vec<_>>();
        let mut report = TrackReportPayload::new(
            "",
            blocks
                .iter()
                .map(calm_types::report_blocks::flat_text)
                .collect::<Vec<_>>()
                .join("\n"),
        );
        report.blocks = Some(blocks);
        let mut tx = repo.pool().begin().await.unwrap();
        let updated = sqlx::query(
            "UPDATE cards SET payload=?1,body_crdt=NULL WHERE track_id=?2 AND kind='track-report'",
        )
        .bind(serde_json::to_string(&report).unwrap())
        .bind(track_id)
        .execute(&mut *tx)
        .await
        .unwrap();
        if updated.rows_affected() == 0 {
            card_create_with_id_tx(
                &mut tx,
                new_id(),
                NewCard {
                    title: None,
                    track_id: track_id.to_owned().into(),
                    kind: "track-report".into(),
                    sort: Some(-1.0),
                    payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
                },
                CardRole::ReportCard,
                false,
                repo.card_role_cache(),
            )
            .await
            .unwrap();
            sqlx::query(
                "UPDATE cards SET payload=?1,body_crdt=NULL WHERE track_id=?2 AND kind='track-report'",
            )
            .bind(serde_json::to_string(&report).unwrap())
            .bind(track_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        tasks_rebuild_tx(&mut tx, track_id).await.unwrap();
        tx.commit().await.unwrap();
        (0..count)
            .map(|index| format!("{track_id}:{prefix}-{index}"))
            .collect()
    }

    async fn claim_for_child(repo: &SqlxRepo, task_id: &str) {
        let mut tx = repo.pool().begin().await.unwrap();
        assert_eq!(
            task_claim_pending_tx(&mut tx, task_id, now_ms(), &[], false)
                .await
                .unwrap(),
            1
        );
        tx.commit().await.unwrap();
    }

    pub(super) async fn create_child_from_task(
        repo: &SqlxRepo,
        parent_track_id: &str,
        task_id: &str,
    ) -> String {
        let root_id = root_and_depth(&mut repo.pool().begin().await.unwrap(), parent_track_id)
            .await
            .unwrap()
            .0;
        let mut conn = repo.pool().acquire().await.unwrap();
        let budget = track_tree_budget(&mut conn, &root_id).await.unwrap();
        let inventory = track_tree_planner_inventory(&mut conn, &root_id)
            .await
            .unwrap();
        let members = track_tree_member_count(&mut conn, &root_id).await.unwrap();
        assert!(inventory < budget, "point one inventory must admit");
        assert!(
            can_add_tree_member(budget, members),
            "point one member bound must admit"
        );
        drop(conn);

        let input = serde_json::to_value(ChildTrackOperationPayload {
            task_id: task_id.into(),
            parent_track_id: parent_track_id.into(),
            goal: "child goal".into(),
            acceptance: Some("done".into()),
            context: json!({}),
            cwd: None,
        })
        .unwrap();
        let adapter = ChildTrackAdapter::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
            test_workspace_root(),
        );
        let mut tx = repo.pool().begin().await.unwrap();
        let output = adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        output.data["child_track_id"].as_str().unwrap().to_owned()
    }

    pub(super) fn payload(task: &Task) -> ChildTrackOperationPayload {
        ChildTrackOperationPayload {
            task_id: task.id.clone(),
            parent_track_id: task.track_id.clone(),
            goal: task.goal.clone(),
            acceptance: task.acceptance_criteria.clone(),
            context: serde_json::from_str(&task.context_json).unwrap(),
            cwd: task.cwd.clone(),
        }
    }

    /// The child's directory is ITS OWN, frozen at creation, `managed`, and a real repository with a resolvable `HEAD`.
    #[tokio::test]
    async fn child_allocates_and_materializes_its_own_frozen_managed_workspace() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace_root = tmp.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();

        let mut tx = repo.pool().begin().await.unwrap();
        let area = area_create_tx(
            &mut tx,
            NewArea {
                name: "c".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let parent = track_create_tx(
            &mut tx,
            NewTrack {
                area_id: area.id,
                title: "parent".into(),
                sort: None,
                cwd: "/ignored-by-managed".into(),
                template_id: None,
                plugin_scope: None,
                template_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            None,
            &TrackWorkspacePlan::ManagedUnder(workspace_root.clone()),
            None,
            repo.track_area_cache(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(parent.workspace.kind, TrackWorkspaceKind::Managed);
        // The parent was minted through the DB writer directly, so materialize it here the way the route would.
        crate::workspace_materialize::materialize_workspace(
            &parent.workspace,
            &workspace_root,
            parent.id.as_str(),
        )
        .unwrap();

        let task = seed_task(&repo, parent.id.as_str(), false).await;
        let input = serde_json::to_value(payload(&task)).unwrap();
        let adapter = ChildTrackAdapter::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
            workspace_root.clone(),
        );
        let mut tx = repo.pool().begin().await.unwrap();
        let output = adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let child_id = output.data["child_track_id"].as_str().unwrap().to_string();

        let (kind, path, frozen_at): (String, String, Option<i64>) = sqlx::query_as(
            "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM tracks WHERE id=?1",
        )
        .bind(&child_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
        assert_eq!(kind, "managed");
        assert_eq!(
            path,
            crate::workspace_materialize::managed_workspace_path(
                &workspace_root,
                parent.area_id.as_str(),
                &child_id,
            )
            .to_string_lossy(),
            "the child's path must be derived from its OWN id"
        );
        assert_ne!(
            path, parent.workspace.path,
            "a child must never be handed its parent's directory (design D7)"
        );
        assert!(
            frozen_at.is_some(),
            "a child workspace is frozen at creation: the very next thing that \
             happens to it is a harness bootstrap on this exact path"
        );
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&path)
                .args(["rev-parse", "--verify", "HEAD"])
                .output()
                .unwrap()
                .status
                .success(),
            "the child's workspace has no init commit; its first codex worker \
             would die in `git worktree add`"
        );
        assert_eq!(
            std::fs::read_to_string(
                std::path::Path::new(&path)
                    .join(".git")
                    .join("neige-workspace")
            )
            .unwrap()
            .trim(),
            child_id,
            "the ownership marker must name the child; under S2 it named the \
             parent, which is exactly what let two tracks claim one directory"
        );
    }

    /// No two track rows share a MANAGED workspace path (table-wide; attached sharing is legal), and removing the
    /// child's directory leaves the parent's repository usable.
    #[tokio::test]
    async fn n11_deleting_a_child_workspace_cannot_destroy_the_parents_repository() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace_root = tmp.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();

        let mut tx = repo.pool().begin().await.unwrap();
        let area = area_create_tx(
            &mut tx,
            NewArea {
                name: "c".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let parent = track_create_tx(
            &mut tx,
            NewTrack {
                area_id: area.id,
                title: "parent".into(),
                sort: None,
                cwd: "/ignored-by-managed".into(),
                template_id: None,
                plugin_scope: None,
                template_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            None,
            &TrackWorkspacePlan::ManagedUnder(workspace_root.clone()),
            None,
            repo.track_area_cache(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        crate::workspace_materialize::materialize_workspace(
            &parent.workspace,
            &workspace_root,
            parent.id.as_str(),
        )
        .unwrap();
        // A commit that must still be there afterwards, so "the parent repository survived" is a claim about its history.
        std::fs::write(
            std::path::Path::new(&parent.workspace.path).join("parent-work.txt"),
            "parent work",
        )
        .unwrap();

        let task = seed_task(&repo, parent.id.as_str(), false).await;
        let input = serde_json::to_value(payload(&task)).unwrap();
        let adapter = ChildTrackAdapter::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
            workspace_root.clone(),
        );
        let mut tx = repo.pool().begin().await.unwrap();
        let output = adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let child_id = output.data["child_track_id"].as_str().unwrap().to_string();
        let child_path: String =
            sqlx::query_scalar("SELECT workspace_path FROM tracks WHERE id=?1")
                .bind(&child_id)
                .fetch_one(repo.pool())
                .await
                .unwrap();

        // Scoped to MANAGED paths: attached paths are shared today in production (several tracks open the same checkout).
        let shared: Vec<(String, i64)> = sqlx::query_as(
            "SELECT workspace_path, count(*) FROM tracks WHERE workspace_kind='managed' \
             GROUP BY workspace_path HAVING count(*) > 1",
        )
        .fetch_all(repo.pool())
        .await
        .unwrap();
        assert!(
            shared.is_empty(),
            "two track rows share a MANAGED workspace path: {shared:?}"
        );

        // What recycling will do to a deleted child.
        std::fs::remove_dir_all(&child_path).unwrap();
        assert!(
            std::path::Path::new(&parent.workspace.path)
                .join("parent-work.txt")
                .exists(),
            "recycling the child's workspace took the parent's work with it"
        );
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&parent.workspace.path)
                .args(["rev-parse", "--verify", "HEAD"])
                .output()
                .unwrap()
                .status
                .success(),
            "recycling the child's workspace destroyed the parent's repository"
        );

        // The scheduler bootstraps the child's planner harness from THIS field, never from the track row.
        assert_eq!(output.data["cwd"], child_path);
        assert_eq!(output.result["cwd"], child_path);
    }

    /// Recycling is by DIRECTORY, so an attached track parked under `<workspace-root>` would lose its workspace as collateral;
    /// the guard lives in `AttachedInheritedPath::new` and this test drives it through the plan chooser.
    #[test]
    fn an_attached_path_inside_the_managed_root_cannot_be_inherited() {
        let root = tempfile::TempDir::new().unwrap();
        let inside = root.path().join("area").join("some-managed-track");
        let error = child_workspace_plan(
            &TrackWorkspace {
                kind: TrackWorkspaceKind::Attached,
                path: inside.to_string_lossy().into_owned(),
                frozen_at: None,
            },
            root.path(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("inside the managed workspace root"),
            "{error}"
        );

        let outside = tempfile::TempDir::new().unwrap();
        assert!(matches!(
            child_workspace_plan(
                &TrackWorkspace {
                    kind: TrackWorkspaceKind::Attached,
                    path: outside.path().to_string_lossy().into_owned(),
                    frozen_at: None,
                },
                root.path(),
            )
            .unwrap(),
            TrackWorkspacePlan::InheritAttachedFrozen(_)
        ));
        assert!(matches!(
            child_workspace_plan(
                &TrackWorkspace {
                    kind: TrackWorkspaceKind::Managed,
                    path: inside.to_string_lossy().into_owned(),
                    frozen_at: None,
                },
                root.path(),
            )
            .unwrap(),
            TrackWorkspacePlan::ManagedFrozenUnder(_)
        ));
    }

    /// POSITIVE case: sharing an attached directory is legal and pre-existing in production; a sub-track spawned to work on
    /// the parent's code must see it. The user's directory is NOT touched.
    #[tokio::test]
    async fn child_of_an_attached_parent_shares_the_parents_path() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let user_repo = tempfile::TempDir::new().unwrap();
        let user_path = user_repo.path().to_string_lossy().into_owned();

        let mut tx = repo.pool().begin().await.unwrap();
        let area = area_create_tx(
            &mut tx,
            NewArea {
                name: "c".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let parent = track_create_tx(
            &mut tx,
            NewTrack {
                area_id: area.id,
                title: "parent".into(),
                sort: None,
                cwd: user_path.clone(),
                template_id: None,
                plugin_scope: None,
                template_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            None,
            &TrackWorkspacePlan::AttachedFromCwd,
            None,
            repo.track_area_cache(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(parent.workspace.kind, TrackWorkspaceKind::Attached);

        let task = seed_task(&repo, parent.id.as_str(), false).await;
        let input = serde_json::to_value(payload(&task)).unwrap();
        let adapter = ChildTrackAdapter::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
            test_workspace_root(),
        );
        let mut tx = repo.pool().begin().await.unwrap();
        let output = adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let child_id = output.data["child_track_id"].as_str().unwrap().to_string();

        let (kind, path, frozen_at): (String, String, Option<i64>) = sqlx::query_as(
            "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM tracks WHERE id=?1",
        )
        .bind(&child_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
        assert_eq!(
            (kind.as_str(), path.as_str()),
            ("attached", user_path.as_str()),
            "a sub-track of an attached track must see the parent's checkout"
        );
        assert!(frozen_at.is_some(), "child workspaces freeze at creation");
        assert_eq!(output.data["cwd"], user_path);
        assert_eq!(output.result["cwd"], user_path);

        // Attached means the server never creates, `git init`s, marks or writes anything here.
        assert_eq!(
            std::fs::read_dir(&user_path).unwrap().count(),
            0,
            "materialization wrote into a user-owned attached directory"
        );

        // The shared path is attached, so no MANAGED path is shared.
        let shared_managed: Vec<(String, i64)> = sqlx::query_as(
            "SELECT workspace_path, count(*) FROM tracks WHERE workspace_kind='managed' \
             GROUP BY workspace_path HAVING count(*) > 1",
        )
        .fetch_all(repo.pool())
        .await
        .unwrap();
        assert!(shared_managed.is_empty(), "{shared_managed:?}");
    }

    /// The crate-wide property gate independently scans every SQL string touching `parent_track_id`; there is intentionally no registry.
    #[test]
    fn upward_cte_keeps_its_only_cycle_termination_guard() {
        for sql in [TRACK_ROOT_DEPTH_SQL, TRACK_BOUNDED_PATH_SQL] {
            assert!(sql.contains("WHERE up.depth <= ?2"));
            assert!(sql.contains("UNION ALL"));
        }
    }

    #[tokio::test]
    async fn acceptance_5_child_seed_uses_all_four_frozen_fields_and_parent_cwd() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let parent = seed_parent(&repo, true).await;
        let task = seed_task(&repo, &parent, false).await;
        // The live report deliberately disagrees with every frozen field, without marking the task stale: the adapter must consume the payload frozen from `tasks`.
        let report = TrackReportPayload {
            schema_version: TrackReportPayload::SCHEMA_VERSION,
            doc_rev: 9,
            summary: String::new(),
            body: String::new(),
            blocks: Some(vec![calm_types::track_report::ReportBlock {
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
            "INSERT INTO cards(id,track_id,kind,sort,payload,role,deletable,created_at,updated_at) \
             VALUES('current-report',?1,'track-report',-1,?2,'reportcard',0,1,1)",
        )
        .bind(&parent)
        .bind(serde_json::to_string(&report).unwrap())
        .execute(repo.pool())
        .await
        .unwrap();
        let input = serde_json::to_value(payload(&task)).unwrap();
        let adapter = ChildTrackAdapter::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
            test_workspace_root(),
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
        let planner_card_id = output.data["planner_card_id"].as_str().unwrap();
        let planner_payload: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
            .bind(planner_card_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
        for frozen_value in [
            "frozen-goal",
            "frozen-acceptance",
            "frozen",
            "/task-only-cwd",
        ] {
            assert!(planner_payload.contains(frozen_value), "{planner_payload}");
        }
        for current_value in [
            "current-goal",
            "current-acceptance",
            "current-cwd",
            "current",
        ] {
            assert!(
                !planner_payload.contains(current_value),
                "{planner_payload}"
            );
        }
        let child_id = output.data["child_track_id"].as_str().unwrap();
        // This parent is ATTACHED (`/parent-cwd`, a directory the user owns), so the child inherits that path and stays attached.
        let expected_child_workspace = "/parent-cwd".to_string();
        assert_eq!(output.data["cwd"], expected_child_workspace);
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT workspace_kind FROM tracks WHERE id=?1")
                .bind(child_id)
                .fetch_one(repo.pool())
                .await
                .unwrap(),
            "attached",
            "inheriting the path must inherit the kind; a `managed` row on a \
             user directory would arm S5 against it"
        );
        type InheritedChildFields = (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        );
        let inherited: InheritedChildFields = sqlx::query_as(
            "SELECT workspace_path,template_id,plugin_scope,template_input,purpose,lifecycle,archived_at,pinned_at,terminal_at \
             FROM tracks WHERE id=?1",
        )
        .bind(child_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
        assert_eq!(
            inherited.0, expected_child_workspace,
            "an attached parent's path IS inherited (design D7, S4 amendment)"
        );
        assert_eq!(inherited.1, None, "template_id must not inherit");
        assert_eq!(
            inherited.2.as_deref(),
            Some("must-inherit-plugin"),
            "plugin_scope must inherit so Only(X) does not widen to All"
        );
        assert_eq!(inherited.3, None, "template_input must not inherit");
        assert_eq!(inherited.4, None, "purpose must not inherit");
        assert_eq!(
            inherited.5, "draft",
            "child must stay Draft before bootstrap"
        );
        assert_eq!(inherited.6, None, "archived_at must not inherit");
        assert_eq!(inherited.7, None, "pinned_at must not inherit");
        assert_eq!(inherited.8, None, "terminal_at must not inherit");

        // Child of Only(X) must not become All; pin the gate reading plugin_scope.
        let _trusted = trust_inherited_plugin().await;
        let repo = Arc::new(repo);
        let (host, _tmp) = plugin_host_with_id(repo.clone(), "must-inherit-plugin").await;
        host.spawn("must-inherit-plugin")
            .await
            .expect("spawn inherited plugin");
        wait_for_running(&host, "must-inherit-plugin").await;
        let ctx = app_context(repo, Some(host.clone()));
        assert_eq!(
            plugin_scope_for_track(&ctx, Some(child_id)).await,
            TrackPluginScope::Only("must-inherit-plugin".into()),
        );
        host.stop("must-inherit-plugin")
            .await
            .expect("stop inherited plugin");
        assert_eq!(
            plugin_scope_for_track(&ctx, Some(child_id)).await,
            TrackPluginScope::None,
            "stopped owner must fail closed, not widen to All"
        );
    }

    #[tokio::test]
    async fn acceptance_6_real_adapter_writes_direct_parent_and_enforces_depth_three() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let root = seed_parent(&repo, false).await;
        let mut direct_parent = root.clone();
        for level in 1..=3 {
            let task = seed_task(&repo, &direct_parent, false).await;
            let input = serde_json::to_value(payload(&task)).unwrap();
            let adapter = ChildTrackAdapter::new(
                repo.card_role_cache().clone(),
                repo.track_area_cache().clone(),
                test_workspace_root(),
            );
            let mut tx = repo.pool().begin().await.unwrap();
            let output = adapter
                .prepare_tx(&mut tx, &input, &operation(input.clone()))
                .await
                .unwrap();
            tx.commit().await.unwrap();
            let child = output.data["child_track_id"].as_str().unwrap().to_string();
            let stored_parent: String =
                sqlx::query_scalar("SELECT parent_track_id FROM tracks WHERE id=?1")
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
        let cross_area_edges: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM tracks child JOIN tracks parent \
             ON parent.id=child.parent_track_id WHERE child.area_id<>parent.area_id",
        )
        .fetch_one(repo.pool())
        .await
        .unwrap();
        assert_eq!(cross_area_edges, 0, "real adapter must inherit parent area");
        let task = seed_task(&repo, &direct_parent, false).await;
        let input = serde_json::to_value(payload(&task)).unwrap();
        let adapter = ChildTrackAdapter::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
            test_workspace_root(),
        );
        let mut tx = repo.pool().begin().await.unwrap();
        let error = adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("sub-track-depth-exceeded"));
    }

    #[tokio::test]
    async fn acceptance_21c_real_adapter_never_writes_a_cross_area_edge() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let parent = seed_parent(&repo, false).await;
        let second_area = {
            let mut tx = repo.pool().begin().await.unwrap();
            let area = area_create_tx(
                &mut tx,
                NewArea {
                    name: "unrelated-area".into(),
                    color: "#111".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            area.id.to_string()
        };
        let task = seed_task(&repo, &parent, false).await;
        let input = serde_json::to_value(payload(&task)).unwrap();
        let adapter = ChildTrackAdapter::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
            test_workspace_root(),
        );
        let mut tx = repo.pool().begin().await.unwrap();
        adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let cross_area_edges: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM tracks child JOIN tracks parent \
             ON parent.id=child.parent_track_id WHERE child.area_id<>parent.area_id",
        )
        .fetch_one(repo.pool())
        .await
        .unwrap();
        assert_eq!(cross_area_edges, 0);

        // The unrelated area is independently deletable: the adapter did not route its child there.
        let mut tx = repo.pool().begin().await.unwrap();
        area_delete_tx(&mut tx, &second_area).await.unwrap();
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn acceptance_7_two_cycle_fails_fast_with_cycle_reason() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let a = seed_parent(&repo, false).await;
        let b = {
            let task = seed_task(&repo, &a, false).await;
            let input = serde_json::to_value(payload(&task)).unwrap();
            let adapter = ChildTrackAdapter::new(
                repo.card_role_cache().clone(),
                repo.track_area_cache().clone(),
                test_workspace_root(),
            );
            let mut tx = repo.pool().begin().await.unwrap();
            let output = adapter
                .prepare_tx(&mut tx, &input, &operation(input.clone()))
                .await
                .unwrap();
            tx.commit().await.unwrap();
            output.data["child_track_id"].as_str().unwrap().to_string()
        };
        sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
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
        assert!(error.to_string().contains("sub-track-tree-cycle"));
    }

    #[tokio::test]
    async fn acceptance_8_missing_parent_is_not_misreported_as_depth_exhaustion() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let mut tx = repo.pool().begin().await.unwrap();
        let error = root_and_depth(&mut tx, "missing").await.unwrap_err();
        assert!(
            matches!(&error, CalmError::NotFound(message) if message == "track missing"),
            "missing parent must retain its diagnostic reason, got {error}"
        );
    }

    /// At B=2, inventory is exactly 2 while member admission N=1 -> 2 is legal, so ONLY the inventory guard can refuse.
    #[tokio::test]
    async fn acceptance_tree_budget_refuses_child_creation_when_the_tree_is_full() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let parent = seed_parent(&repo, false).await;
        let task = seed_task(&repo, &parent, false).await;
        let _other = seed_task_with_key(&repo, &parent, "other-live-task", false).await;
        let input = serde_json::to_value(payload(&task)).unwrap();
        let adapter = ChildTrackAdapter::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
            test_workspace_root(),
        );
        sqlx::query("UPDATE tracks SET tree_task_budget=2 WHERE id=?1")
            .bind(&parent)
            .execute(repo.pool())
            .await
            .unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
            .fetch_one(repo.pool())
            .await
            .unwrap();

        let mut tx = repo.pool().begin().await.unwrap();
        let error = adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("sub-track-tree-budget-exhausted"),
            "{error}"
        );
        assert!(error.to_string().contains(&parent), "{error}");
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(before, after, "a refused creation must write nothing");
        tx.rollback().await.unwrap();

        // The budget lives only on the ROOT.
        sqlx::query("UPDATE tracks SET tree_task_budget=3 WHERE id=?1")
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
        let child = output.data["child_track_id"].as_str().unwrap().to_string();
        let child_budget: Option<i64> =
            sqlx::query_scalar("SELECT tree_task_budget FROM tracks WHERE id=?1")
                .bind(&child)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(
            child_budget, None,
            "the child must not carry a budget of its own"
        );
    }

    /// After the first child's parent task finishes, inventory alone would allow another child; the member bound must still refuse a zero share.
    #[tokio::test]
    async fn acceptance_tree_budget_never_admits_a_zero_share_member() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let parent = seed_parent(&repo, false).await;
        sqlx::query("UPDATE tracks SET tree_task_budget=2 WHERE id=?1")
            .bind(&parent)
            .execute(repo.pool())
            .await
            .unwrap();
        let first = seed_task_with_key(&repo, &parent, "first-child", false).await;
        let input = serde_json::to_value(payload(&first)).unwrap();
        let adapter = ChildTrackAdapter::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
            test_workspace_root(),
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
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
            .fetch_one(repo.pool())
            .await
            .unwrap();
        let mut tx = repo.pool().begin().await.unwrap();
        let error = adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("sub-track-tree-budget-exhausted")
                && error.to_string().contains("2 member track(s)")
                && error.to_string().contains("zero schedulable share"),
            "{error}"
        );
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(before, after, "a shape-refused creation must write nothing");
        tx.rollback().await.unwrap();
    }

    /// Without the post-create whole-tree reprojection these finish at 9/8 and 15/12 respectively.
    #[tokio::test]
    async fn whole_tree_live_planner_never_exceeds_budget_across_admitted_growth_sequences() {
        // B=8: N=3 shrinks the first child's share to 3, so the shared rebuild must cull one pending row before child 2 can consume its share.
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let root = seed_parent(&repo, false).await;
        let mut tx = repo.pool().begin().await.unwrap();
        track_update_tx(
            &mut tx,
            &root,
            TrackPatch {
                tree_task_budget: Some(Some(8)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let root_tasks = project_pending_tasks(&repo, &root, "root-eight", 3).await;
        for task_id in &root_tasks[..2] {
            claim_for_child(&repo, task_id).await;
        }
        let first_child = create_child_from_task(&repo, &root, &root_tasks[0]).await;
        let first_child_tasks = project_pending_tasks(&repo, &first_child, "child-eight", 4).await;
        claim_for_child(&repo, &first_child_tasks[0]).await;
        let second_child = create_child_from_task(&repo, &first_child, &first_child_tasks[0]).await;
        project_pending_tasks(&repo, &second_child, "leaf-eight", 2).await;
        let mut conn = repo.pool().acquire().await.unwrap();
        let total_eight = track_tree_planner_inventory(&mut conn, &root)
            .await
            .unwrap();

        // B=12: the final N=4 rebuild shrinks root from six live rows to share=3.
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let root = seed_parent(&repo, false).await;
        let mut tx = repo.pool().begin().await.unwrap();
        track_update_tx(
            &mut tx,
            &root,
            TrackPatch {
                tree_task_budget: Some(Some(12)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let root_tasks = project_pending_tasks(&repo, &root, "root-twelve", 6).await;
        for task_id in &root_tasks[..3] {
            claim_for_child(&repo, task_id).await;
        }
        let mut children = Vec::new();
        for task_id in &root_tasks[..3] {
            children.push(create_child_from_task(&repo, &root, task_id).await);
        }
        for (index, child) in children.iter().enumerate() {
            project_pending_tasks(&repo, child, &format!("leaf-twelve-{index}"), 3).await;
        }
        let mut conn = repo.pool().acquire().await.unwrap();
        let total_twelve = track_tree_planner_inventory(&mut conn, &root)
            .await
            .unwrap();
        assert_eq!(
            (total_eight, total_twelve),
            (8, 12),
            "admitted B=8/B=12 growth must settle at, never above, each B"
        );
    }

    /// Inventory 5 < B=8 and N+1=2 <= B, but the new two-member share is 4 while all five root rows are already in-flight;
    /// the enclosing transaction must be rollback-clean.
    #[tokio::test]
    async fn child_creation_409s_when_inflight_member_exceeds_its_new_share() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let root = seed_parent(&repo, false).await;
        let mut tx = repo.pool().begin().await.unwrap();
        track_update_tx(
            &mut tx,
            &root,
            TrackPatch {
                tree_task_budget: Some(Some(8)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let tasks = project_pending_tasks(&repo, &root, "root-overage", 5).await;
        for task in &tasks {
            claim_for_child(&repo, task).await;
        }
        let before: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM tracks), (SELECT count(*) FROM cards), \
             (SELECT count(*) FROM tasks), (SELECT count(*) FROM events)",
        )
        .fetch_one(repo.pool())
        .await
        .unwrap();
        let input = serde_json::to_value(ChildTrackOperationPayload {
            task_id: tasks[0].clone(),
            parent_track_id: root.clone(),
            goal: "must roll back".into(),
            acceptance: Some("no child committed".into()),
            context: json!({}),
            cwd: None,
        })
        .unwrap();
        let adapter = ChildTrackAdapter::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
            test_workspace_root(),
        );
        let mut tx = repo.pool().begin().await.unwrap();
        let error = adapter
            .prepare_tx(&mut tx, &input, &operation(input.clone()))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, CalmError::Conflict(message) if message.contains("5 unfinished planner task(s)") && message.contains("new share of 4")),
            "{error}"
        );
        tx.rollback().await.unwrap();
        let after: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM tracks), (SELECT count(*) FROM cards), \
             (SELECT count(*) FROM tasks), (SELECT count(*) FROM events)",
        )
        .fetch_one(repo.pool())
        .await
        .unwrap();
        assert_eq!(
            after, before,
            "the refused child operation must roll back every write"
        );
    }

    /// The ordinary and whole-tree rebuild entrypoints must produce the same codes for the same report.
    #[tokio::test]
    async fn singleton_rebuild_entrypoints_agree_when_budget_equals_ceiling() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let root = seed_parent(&repo, false).await;
        let mut tx = repo.pool().begin().await.unwrap();
        track_update_tx(
            &mut tx,
            &root,
            TrackPatch {
                planner_task_ceiling: Some(Some(2)),
                tree_task_budget: Some(Some(2)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        project_pending_tasks(&repo, &root, "equal", 3).await;

        let mut tx = repo.pool().begin().await.unwrap();
        let plain = tasks_rebuild_tx(&mut tx, &root).await.unwrap();
        let tree = tasks_rebuild_tree_tx(&mut tx, &root).await.unwrap();
        let tree = &tree
            .iter()
            .find(|(track, _)| track.id.as_str() == root)
            .expect("singleton root projection")
            .1;
        let codes = |outcome: &crate::db::sqlite::TaskProjectionOutcome| {
            outcome
                .diagnostics
                .iter()
                .filter(|verdict| !verdict.schedulable)
                .flat_map(|verdict| {
                    verdict
                        .diagnostics
                        .iter()
                        .map(|diagnostic| diagnostic.code.clone())
                })
                .collect::<Vec<_>>()
        };
        let plain_codes = codes(&plain);
        let tree_codes = codes(tree);
        assert_eq!(plain_codes, tree_codes, "rebuild entrypoints drifted");
        assert_eq!(
            plain_codes,
            ["planner_task_ceiling", "tree_budget_exhausted"]
        );
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn acceptance_10_child_adapter_stale_fence_precedes_every_side_effect() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let parent = seed_parent(&repo, false).await;
        let task = seed_task(&repo, &parent, true).await;
        let input = serde_json::to_value(payload(&task)).unwrap();
        let adapter = ChildTrackAdapter::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
            test_workspace_root(),
        );
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
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
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
            .fetch_one(repo.pool())
            .await
            .unwrap();
        assert_eq!(before, after);
    }

    const INHERITED_PLUGIN_ID: &str = "must-inherit-plugin";

    /// Takes the crate-wide env lock: this is the second writer of `NEIGE_TRUSTED_FORGE_PLUGINS` in the lib-test binary
    /// (`track_binding::tests::TrustGuard` is the other).
    async fn trust_inherited_plugin() -> InheritedTrustGuard {
        let lock = crate::forge_trust::trusted_forge_plugins_env_lock()
            .lock()
            .await;
        let previous = std::env::var("NEIGE_TRUSTED_FORGE_PLUGINS").ok();
        let combined = match previous.as_deref() {
            Some(configured)
                if configured
                    .split(',')
                    .any(|id| id.trim() == INHERITED_PLUGIN_ID) =>
            {
                configured.to_string()
            }
            Some(configured) => format!("{configured},{INHERITED_PLUGIN_ID}"),
            None => format!("dev.neige.git-forge,{INHERITED_PLUGIN_ID}"),
        };
        unsafe { std::env::set_var("NEIGE_TRUSTED_FORGE_PLUGINS", &combined) };
        assert!(
            trusted_forge_plugin(INHERITED_PLUGIN_ID),
            "{INHERITED_PLUGIN_ID} must be trusted for the Only pin"
        );
        InheritedTrustGuard {
            previous,
            _lock: lock,
        }
    }

    struct InheritedTrustGuard {
        previous: Option<String>,
        _lock: tokio::sync::MutexGuard<'static, ()>,
    }

    impl Drop for InheritedTrustGuard {
        fn drop(&mut self) {
            match self.previous.as_deref() {
                Some(previous) => unsafe {
                    std::env::set_var("NEIGE_TRUSTED_FORGE_PLUGINS", previous)
                },
                None => unsafe { std::env::remove_var("NEIGE_TRUSTED_FORGE_PLUGINS") },
            }
        }
    }

    fn app_context(repo: Arc<SqlxRepo>, host: Option<Arc<PluginHost>>) -> Arc<AppContext> {
        let sqlite_pool = crate::db::Repo::sqlite_pool(repo.as_ref());
        let repo_dyn: Arc<dyn crate::db::Repo> = repo;
        let route_repo: Arc<dyn crate::db::RouteRepo> = repo_dyn;
        let plugin_host = Arc::new(tokio::sync::OnceCell::new());
        if let Some(host) = host {
            assert!(
                plugin_host.set(host).is_ok(),
                "late-bound plugin host cell must be set once"
            );
        }
        Arc::new(AppContext {
            terminal_interaction: Arc::new(tokio::sync::OnceCell::new()),
            repo: route_repo,
            track_vcs: None,
            events: EventBus::new(),
            write: WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
            daemon_token_hash: None,
            gate_logs_dir: std::env::temp_dir().join("neige-test-gate-logs"),
            task_budget_default: crate::scheduler::DEFAULT_TRACK_TASK_BUDGET,
            plugin_host,
            operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
            scheduler_poke: Arc::new(tokio::sync::OnceCell::new()),
            series_resolver: Arc::new(crate::report_series::SeriesResolver::new_unstarted(None)),
            plugin_results: Arc::new(crate::plugin_results::PluginResults::new()),
            preview: Arc::new(crate::preview::PreviewRegistry::disabled()),
            sqlite_pool,
        })
    }

    async fn plugin_host_with_id(
        repo: Arc<SqlxRepo>,
        plugin_id: &str,
    ) -> (Arc<PluginHost>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plugins_dir = tmp.path().join("plugins");
        let plugins_data_dir = tmp.path().join("plugins-data");
        let install_dir = plugins_dir.join(plugin_id);
        let bin_dir = install_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
        std::fs::create_dir_all(&plugins_data_dir).expect("create plugins data dir");
        std::os::unix::fs::symlink(stub_echo_bin(), bin_dir.join("stub"))
            .expect("symlink echo stub");
        let manifest_json = json!({
            "manifest_version": 1,
            "id": plugin_id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Child Inherit Stub",
            "entrypoint": { "command": "bin/stub" },
            "templates": [],
            "permissions": {}
        });
        let manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest parses");
        let registry = PluginRegistry::from_manifests([(manifest, Some(install_dir.clone()))]);
        repo.plugin_install(crate::model::NewPlugin {
            id: plugin_id.to_string(),
            version: "0.1.0".into(),
            install_path: install_dir.display().to_string(),
            manifest: manifest_json,
            enabled: true,
            user_config: json!({}),
        })
        .await
        .expect("seed plugin row");
        let repo_dyn: Arc<dyn crate::db::Repo> = repo;
        let host = Arc::new(PluginHost::new_full(
            Arc::new(registry),
            repo_dyn,
            plugins_dir,
            plugins_data_dir,
            Vec::new(),
            EventBus::new(),
            WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
        ));
        (host, tmp)
    }

    async fn wait_for_running(host: &Arc<PluginHost>, plugin_id: &str) {
        let start = Instant::now();
        loop {
            if let Some(status) = host.status(plugin_id).await
                && matches!(status.status, PluginRuntimeStatus::Running)
            {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "timed out waiting for plugin {plugin_id} to run"
            );
            sleep(Duration::from_millis(25)).await;
        }
    }

    fn stub_echo_bin() -> PathBuf {
        if let Some(path) = std::env::var_os("CARGO_BIN_EXE_plugin-host-stub-echo") {
            return path.into();
        }
        if let Some(path) = option_env!("CARGO_BIN_EXE_plugin-host-stub-echo") {
            return path.into();
        }
        let current = std::env::current_exe().expect("current test executable");
        let deps_dir = current.parent().expect("test executable parent");
        let debug_dir = deps_dir.parent().expect("target debug dir");
        let candidate = debug_dir.join("plugin-host-stub-echo");
        assert!(
            candidate.exists(),
            "missing plugin-host-stub-echo at {}",
            candidate.display()
        );
        candidate
    }
}
