use async_trait::async_trait;
use calm_truth::decision_gate::PermissiveGate;
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

/// PR-B moved every bounded track-tree walk into one module in `calm-truth`,
/// so the schedulability predicate (which lives there) and this operation
/// share the same fragments AND the same static gate. Re-exported here
/// because the tree depth bound is part of this adapter's public contract.
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

/// Stable first observation for the child planner. This function has no report
/// reader: all four fields come from the post-claim task row.
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
    /// #1147 D2/D7 — the managed workspace root. The child's own workspace is
    /// derived under it and materialized in `prepare_tx`.
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

/// #1147 S4 (design D7, amended) — a child track's workspace plan, by the
/// parent's `kind`.
///
/// The original D7 said "a child must always allocate independently". That
/// conclusion was drawn from one hazard — deleting a child `rm -rf`s the
/// parent's repository — and that hazard exists **only for a managed parent**,
/// because S5 recycles `kind = managed` directories and nothing else. Stated
/// unconditionally it also broke the feature: a sub-track of an attached track
/// would get an empty repository and could not see the code it was spawned to
/// work on, which is the normal reason to spawn one.
///
/// * **Managed parent** → the child allocates its own managed workspace,
///   frozen at creation. Two rows must never share a *managed* directory.
/// * **Attached parent** → the child inherits the same attached path, frozen
///   at creation. Nothing recycles it, and several tracks pointing at one
///   checkout is an ordinary, pre-existing state.
///
/// Frozen on both branches: a child is machine-created inside a running planner
/// and its harness bootstraps on this path immediately, so there is no window
/// in which re-pointing it would be safe.
fn child_workspace_plan(
    parent: &TrackWorkspace,
    workspace_root: &std::path::Path,
) -> Result<TrackWorkspacePlan> {
    Ok(match parent.kind {
        TrackWorkspaceKind::Managed => {
            TrackWorkspacePlan::ManagedFrozenUnder(workspace_root.to_path_buf())
        }
        // `AttachedInheritedPath::new` refuses a path inside the managed root:
        // recycling works on directories, not rows, so an attached track living
        // under the root would lose its workspace when the managed track owning
        // that directory is deleted. Unreachable from here (an attached parent
        // under the root is already an invariant violation), and checked
        // anyway because the check belongs to the type, not to this caller.
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

        // Fifth task-bound decision point. This is deliberately the first DB
        // action: a materialized frozen context may not create a child skeleton.
        refuse_if_context_stale(tx, Some(&payload.task_id)).await?;

        let (root_id, parent_depth) = root_and_depth(tx, &payload.parent_track_id).await?;
        if parent_depth >= MAX_TRACK_TREE_DEPTH {
            return Err(CalmError::Conflict("sub-track-depth-exceeded".into()));
        }

        // Enforcement point one for the tree budget (#985 §8). The whole tree's
        // non-terminal `declared_by='spec'` inventory — NOT this track's — gates
        // child creation. The claiming parent task is itself one of those rows,
        // so `>=` (not `>`) is the right comparison: admitting a child at
        // `count == budget` would let the tree grow past its bound before any
        // schedulability verdict could see it.
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

        // #1147 S4 — the parent's WORKSPACE KIND decides the child's plan, so
        // it is read here together with the area and the plugin scope. The
        // parent's *path* is read on exactly one branch (attached) and is
        // otherwise unused; see `child_workspace_plan`.
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
            // Not read by `child_workspace_plan`; the child's own stamp is set
            // by the plan, not copied.
            frozen_at: None,
        };
        let plan = child_workspace_plan(&parent_workspace, &self.workspace_root)?;
        let seed = render_child_seed(&payload);
        let child = track_create_tx(
            tx,
            NewTrack {
                area_id: area_id.into(),
                title: payload.goal.clone(),
                sort: None,
                // Ignored by both plans this adapter can pick: the managed
                // path is derived from the id (which does not exist until
                // `track_create_tx` mints it) and the attached path travels
                // inside `InheritAttachedFrozen`. Empty rather than the
                // parent's path so no plan can pick up an inherited path
                // through a field that is supposed to be dead here.
                cwd: String::new(),
                template_id: None,
                plugin_scope: parent_plugin_scope,
                template_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            None,
            &plan,
            &self.track_area_cache,
        )
        .await?;
        // #1147 S4 — the child-track adapter, one of the four track-create entry
        // points (`POST /api/tracks`, area chat, launchpad, child track; template
        // seeding was a fifth until #1300 S2 deleted it). The enumeration lives
        // once, in `tests/cases/track_workspace_materialize.rs`; this comment
        // and `routes/today.rs` both used to call themselves "the fifth",
        // which is why neither carries an ordinal any more. On the
        // managed branch
        // it does real work (the child's directory is its own, so nothing else
        // has created it); on the attached branch it is a no-op by contract —
        // `materialize_workspace` never creates, `git init`s or writes to a
        // directory the user owns.
        //
        // On the managed branch the ownership marker names the CHILD, which is
        // what makes the allocation checkable rather than merely intended: the
        // same marker check that refuses a third-party repository also refuses
        // a directory already owned by another track, so "the child quietly
        // ended up on the parent's managed path" cannot survive this call.
        // Under S2 the marker had to name the *parent* precisely because the
        // path was the parent's — that asymmetry was the shape of the bug
        // (issue #1147 N11).
        crate::workspace_materialize::materialize_workspace(
            &child.workspace,
            &self.workspace_root,
            child.id.as_str(),
        )?;
        // The child must inherit its parent's area. A cross-area parent edge
        // makes area deletion fail its NO ACTION self-FK (tripwire test #21c).
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
                payload: planner_harness_card_payload(Some(seed.clone())),
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

        // `N` has changed, so every old member's deterministic share may have
        // shrunk. Reuse the same bounded whole-tree routine as a root budget
        // PATCH before this transaction can expose the child.
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
            let id = append_decision_event_in_tx(
                tx,
                &PermissiveGate,
                &event_actor,
                &scope,
                None,
                &event,
            )
            .await?;
            envelopes.push(BroadcastEnvelope {
                id,
                event_version: SYNC_EVENT_VERSION,
                actor: event_actor,
                scope,
                event,
            });
        }

        // #1147 S4 — `cwd` here is not a convenience copy. The scheduler's
        // child-track bootstrap (`scheduler::drive_child_track`) never re-reads
        // the track row: it takes `cwd` from THIS result — including from the
        // persisted `tx_output` of an older operation on an idempotency
        // collision — and hands it to `planner-harness-start`. Changing the
        // adapter's allocation without changing this field would leave the
        // child's harness anchored on the parent's directory, which is the
        // adapter-only half of the same bug.
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
    use crate::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
    use crate::state::WriteContext;
    use crate::track_area_cache::TrackAreaCache;
    use crate::track_report::tasks_rebuild_tx;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::{Instant, sleep};

    fn operation(payload: Value) -> Operation {
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

    /// #1147 S4 — a REAL workspace root for adapter fixtures.
    ///
    /// Under S2 this was a path that did not exist, because a child inherited
    /// its (attached) parent's workspace and materialization was a no-op. Since
    /// S4 every child allocates and materializes its own managed directory, so
    /// every one of these tests now writes real repositories — a fake root
    /// would turn them all into materialization failures.
    ///
    /// One process-wide `TempDir`, deliberately never dropped: adapter tests
    /// only ever write ids under it, so they cannot collide, and keeping it
    /// alive means no test can observe a root that was removed by another
    /// test's teardown. It lives in the OS temp dir, never in `$HOME`.
    fn test_workspace_root() -> std::path::PathBuf {
        static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| tempfile::TempDir::new().expect("adapter test workspace root"))
            .path()
            .to_path_buf()
    }

    async fn seed_parent(repo: &SqlxRepo, non_default_lifecycle_metadata: bool) -> String {
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
            repo.track_area_cache(),
        )
        .await
        .unwrap();
        if non_default_lifecycle_metadata {
            // Acceptance #5 alone needs negative inheritance sentinels. Other
            // adapter tests keep a live Draft parent so their fixtures do not
            // normalize "terminal parents may spawn children" as valid.
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
        track.id.to_string()
    }

    async fn seed_task(repo: &SqlxRepo, track_id: &str, stale: bool) -> Task {
        seed_task_with_key(repo, track_id, "child", stale).await
    }

    async fn seed_task_with_key(repo: &SqlxRepo, track_id: &str, key: &str, stale: bool) -> Task {
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

    async fn create_child_from_task(
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

    fn payload(task: &Task) -> ChildTrackOperationPayload {
        ChildTrackOperationPayload {
            task_id: task.id.clone(),
            parent_track_id: task.track_id.clone(),
            goal: task.goal.clone(),
            acceptance: task.acceptance_criteria.clone(),
            context: serde_json::from_str(&task.context_json).unwrap(),
            cwd: task.cwd.clone(),
        }
    }

    /// #1147 S4 (design D7) — the child-track adapter is one of the four
    /// track-create entry points and does not go through
    /// `create_track_structure`, so it carries its own allocation and its own
    /// materialize call.
    ///
    /// What is asserted: the child's directory is ITS OWN
    /// (`<root>/<area>/<child_id>`, not the parent's), it is frozen at
    /// creation (design "更换与冻结": freeze before any non-re-anchorable cwd
    /// consumer, and child creation is named there), the row says `managed`,
    /// and the directory is a real repository with a resolvable `HEAD` — i.e.
    /// the child's first codex worker can `git worktree add` in it.
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
            repo.track_area_cache(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(parent.workspace.kind, TrackWorkspaceKind::Managed);
        // The parent was minted through the DB writer directly, so nothing has
        // materialized it yet — that is what the route does. Do it here so the
        // child's own call is exercised against the real steady state.
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

    /// **#1147 N11 — REPLACES the S2 gap test of the same subject.**
    ///
    /// The pinned gap asserted the hazard itself: S2's adapter wrote child rows
    /// with `kind = managed` and a path equal to the parent's directory, so
    /// S5's recycle would destroy the parent's repository when a child was
    /// deleted. This test asserts the fixed behaviour instead.
    ///
    /// No data migration accompanies it, and that is a checked fact rather than
    /// an omission: the hazardous row can only be produced by S2's adapter, S2
    /// was never deployed (both live databases sit below it with zero child
    /// tracks), and S2 and S4 ship together — see the N11 row in the design's
    /// 已知缺口 table, including the ordering constraint it records.
    ///
    /// Two assertions, both of which the S2 shape fails:
    ///
    /// * no two track rows share a **managed** workspace path — checked over the
    ///   WHOLE table, not just this pair, because "the child got its own
    ///   directory" is only worth anything as a table-wide invariant. Scoped to
    ///   managed because attached sharing is legal and pre-existing (see
    ///   `child_of_an_attached_parent_shares_the_parents_path`);
    /// * removing the child's directory (what S5 will do) leaves the parent's
    ///   repository usable — the design's acceptance line
    ///   "删除子 track 后父仓库仍可用", executed rather than argued.
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
        // Real parent work: a commit that must still be there afterwards, so
        // "the parent repository survived" is a claim about its history and
        // not merely about a directory still existing.
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

        // The invariant is scoped to MANAGED paths, and the scope is the S4
        // amendment to design D7: sharing is a hazard only where recycling can
        // reach, and S5 recycles `kind = managed` exclusively. Attached paths
        // are shared today in production (several tracks open the same
        // checkout), so an unscoped version of this assertion would call a
        // long-standing legal state a violation — see
        // `child_of_an_attached_parent_shares_the_parents_path`.
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

        // What S5 will do to a deleted child.
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

        // The scheduler bootstraps the child's planner harness from THIS field,
        // never from the track row.
        assert_eq!(output.data["cwd"], child_path);
        assert_eq!(output.result["cwd"], child_path);
    }

    /// #1147 S4 — an attached path INSIDE the managed workspace root may not
    /// be inherited.
    ///
    /// "Attached rows are never recycled" is a claim about the row; S5 recycles
    /// by DIRECTORY. An attached track parked under `<workspace-root>` loses its
    /// workspace as collateral when the managed track owning that directory is
    /// deleted. Unreachable from the adapter (an attached parent under the root
    /// is already an invariant violation elsewhere), so the guard lives in
    /// `AttachedInheritedPath::new` where a cross-crate caller also hits it —
    /// this test drives that constructor through the plan chooser.
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

        // The ordinary case still works, and a managed parent is unaffected.
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

    /// #1147 S4 amendment to design D7 — the child of an ATTACHED parent
    /// inherits the parent's path, and that is the correct answer, not a
    /// leftover of the S2 bug.
    ///
    /// This is a POSITIVE case: sharing an attached directory is legal, and
    /// pre-existing in production (several tracks are pointed at the same
    /// checkout today). D7's "always allocate independently" was derived from
    /// one hazard — a deleted child recycling its parent's repository — and
    /// S5 recycles `kind = managed` only, so the derivation does not reach
    /// attached parents. Stated unconditionally it also broke the feature: a
    /// sub-track spawned to work on the parent's code would be handed an empty
    /// repository instead.
    ///
    /// Asserted here: same path, `kind = attached`, frozen at creation, the
    /// scheduler's bootstrap cwd is that same path, and the user's directory
    /// was NOT touched — no `git init`, no ownership marker, nothing created.
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

        // The user's directory is untouched: attached means the server never
        // creates, `git init`s, marks or writes anything here.
        assert_eq!(
            std::fs::read_dir(&user_path).unwrap().count(),
            0,
            "materialization wrote into a user-owned attached directory"
        );

        // And the narrowed invariant still holds over the whole table: the
        // shared path is attached, so no MANAGED path is shared.
        let shared_managed: Vec<(String, i64)> = sqlx::query_as(
            "SELECT workspace_path, count(*) FROM tracks WHERE workspace_kind='managed' \
             GROUP BY workspace_path HAVING count(*) > 1",
        )
        .fetch_all(repo.pool())
        .await
        .unwrap();
        assert!(shared_managed.is_empty(), "{shared_managed:?}");
    }

    /// Both fragments this adapter runs keep their only cycle-termination
    /// guard. The crate-wide property gate independently scans every SQL
    /// string touching `parent_track_id`; there is intentionally no registry.
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
        // The live report deliberately disagrees with every frozen field,
        // without marking the task stale. This DB seam proves that the real
        // adapter consumes the operation payload frozen from `tasks`, rather
        // than re-reading the current declaration.
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
        // #1147 S4 (D7 as amended) — this parent is ATTACHED (`/parent-cwd`, a
        // directory the user owns), so the child inherits that path and stays
        // attached. Sharing an attached directory arms nothing: S5 recycles
        // `kind = managed` only. The alternative — handing the child an empty
        // managed repository — would mean a sub-track spawned to work on the
        // parent's code cannot see it, which is what this test's `cwd`
        // assertions exist to keep visible.
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

        // §6.2 hole: child of Only(X) must not become All. Column copy plus
        // the gate reading plugin_scope is the composition; pin the gate.
        let _trusted = trust_inherited_plugin();
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

        // The unrelated area is independently deletable: the adapter did not
        // accidentally route its child there and create a NO ACTION tripwire.
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

    /// PR-B enforcement point one. The inventory counted is the WHOLE tree's
    /// non-terminal planner rows. At B=2, inventory is exactly 2 while member
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

        // Raising the ROOT's budget (the only place it lives) admits it.
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

    /// Compatibility of the two enforcement points: after the first child is
    /// admitted under B=2 and its parent task finishes, inventory alone would
    /// allow another child. The member bound must still refuse it because an
    /// admitted N=3 tree would assign a zero share.
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

    /// Load-bearing D.4 #7 acceptance. Both cases use only the production
    /// projection, claim, admission predicates, and child adapter. They are
    /// the review counterexamples: without the post-create whole-tree
    /// reprojection they finish at 9/8 and 15/12 respectively.
    #[tokio::test]
    async fn whole_tree_live_planner_never_exceeds_budget_across_admitted_growth_sequences() {
        // B=8: root keeps three rows; a four-row first child supplies the
        // second child-track operation. N=3 shrinks that member's share to 3,
        // so the shared rebuild must cull one pending row before child 2 can
        // consume its two-row share.
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

        // B=12: three already-claimed root declarations create three siblings.
        // The final N=4 rebuild shrinks root from six live rows to share=3;
        // each child may then independently consume its three-row share.
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

    /// The member postcondition is the only guard that can reject this legal
    /// point-one admission: inventory 5 < B=8 and N+1=2 <= B, but the new
    /// two-member share is 4 while all five root rows are already in-flight.
    /// The adapter must surface Conflict and its enclosing transaction must be
    /// rollback-clean (the HTTP operation layer maps Conflict to 409).
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

    /// A singleton with equal numeric ceiling and budget reports both binding
    /// settings. The ordinary and whole-tree rebuild entrypoints must produce
    /// the same codes for the same report.
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

    fn trust_inherited_plugin() -> InheritedTrustGuard {
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
        InheritedTrustGuard { previous }
    }

    struct InheritedTrustGuard {
        previous: Option<String>,
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
            repo: route_repo,
            track_vcs: None,
            events: EventBus::new(),
            write: WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
            daemon_token_hash: None,
            gate_logs_dir: std::env::temp_dir().join("neige-test-gate-logs"),
            plugin_host,
            operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
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
