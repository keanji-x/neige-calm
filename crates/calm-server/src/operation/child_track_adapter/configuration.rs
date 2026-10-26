//! Creation-time observations and post-commit materialization. Retry consumes only tx_output.
use super::*;
use crate::harness::profile::PlannerBinding;
use crate::model::{Card, CardRole, Track};
use crate::planner_model::CardModelSelection;
use crate::planner_permission_mode::{self, PlannerPermissionMode};
use crate::session_projection_repo::AgentProvider;

pub(super) struct FrozenParent {
    pub track: Track,
    pub provider: AgentProvider,
    pub permission: PlannerPermissionMode,
    pub selection: CardModelSelection,
    pub head: Option<String>,
}

impl FrozenParent {
    pub async fn read(conn: &mut sqlx::SqliteConnection, parent: &str) -> Result<Self> {
        let track: Track =
            sqlx::query_as::<_, crate::db::rows::TrackRow>("SELECT * FROM tracks WHERE id=?1")
                .bind(parent)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or_else(|| CalmError::NotFound(format!("parent track {parent}")))?
                .into();
        let card: Card = sqlx::query_as::<_, crate::db::rows::CardRow>(
            "SELECT id,track_id,kind,sort,payload,title,deletable,created_at,updated_at \
             FROM cards WHERE track_id=?1 AND role='planner'",
        )
        .bind(parent)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| CalmError::Conflict(format!("track {parent} has no planner_provider")))?
        .into();
        let provider = PlannerBinding::from_card(&card, CardRole::Planner)
            .ok_or_else(|| CalmError::Conflict(format!("track {parent} has no planner_provider")))?
            .provider;
        let permission = planner_permission_mode::read(&card.payload)
            .map_err(|e| CalmError::Conflict(e.to_string()))?;
        let selection = CardModelSelection::from_payload(&card.payload)
            .map_err(|e| CalmError::Conflict(e.to_string()))?;
        Ok(Self {
            track,
            provider,
            permission,
            selection,
            head: None,
        })
    }
}

pub(super) struct PreparedChild {
    pub adapter: ChildTrackAdapter,
    pub input: Value,
    pub frozen: FrozenParent,
}

#[async_trait]
impl super::super::PreparedTx for PreparedChild {
    async fn apply(
        self: Box<Self>,
        tx: &mut Tx<'_>,
        _: &Operation,
    ) -> Result<super::super::PreparedTxResult> {
        self.adapter
            .create_tx(tx, &self.input, Some(self.frozen))
            .await
            .map(super::super::PreparedTxResult::Ready)
    }
}

/// Shared decoder: released five-string, frozen Workspace/HEAD, or U11 worker journal.
fn workspace_contract(output: &TxOutput) -> Result<Option<Track>> {
    let invalid = || CalmError::Conflict("invalid child Workspace contract".into());
    let data = output.data.as_object().ok_or_else(invalid)?;
    let mut creation = data.clone();
    creation.remove("workspace_error");
    if output.target_type != "track" || output.result != Value::Object(creation.clone()) {
        return Err(invalid());
    }
    for key in [
        "child_track_id",
        "planner_card_id",
        "report_card_id",
        "seed",
        "cwd",
    ] {
        if !data
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
        {
            return Err(invalid());
        }
    }
    if output.target_id.as_deref() != data["child_track_id"].as_str() {
        return Err(invalid());
    }
    if creation.len() == 5 {
        return Ok(None); // Released borrowed-child contract, never a missing-field default.
    }
    if !matches!(creation.len(), 7 | 8)
        || (creation.len() == 8 && !creation.contains_key("canonical_worker"))
    {
        return Err(invalid());
    }
    let frozen: Track =
        serde_json::from_value(data.get("workspace_track").ok_or_else(invalid)?.clone())?;
    let head = data.get("parent_head").ok_or_else(invalid)?;
    if frozen.workspace.frozen_at.is_none() {
        return Err(invalid());
    }
    if frozen.workspace.kind == TrackWorkspaceKind::Attached {
        if frozen
            .workspace
            .worktree
            .as_deref()
            .is_none_or(str::is_empty)
        {
            return Err(invalid());
        }
        if !head
            .as_str()
            .is_some_and(|s| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(invalid());
        }
    } else if !head.is_null() || frozen.workspace.worktree.is_some() {
        return Err(invalid());
    }
    if let Some(worker) = data.get("canonical_worker")
        && !worker.is_null()
        && !worker.as_str().is_some_and(|s| !s.is_empty())
    {
        return Err(invalid());
    }
    if frozen.id.as_str() != data["child_track_id"].as_str().unwrap()
        || frozen.workspace.agent_cwd() != data["cwd"].as_str().unwrap()
    {
        return Err(invalid());
    }
    Ok(Some(frozen))
}

pub(super) async fn ensure_child_workspace(output: &TxOutput) -> Result<()> {
    let Some(track) = workspace_contract(output)? else {
        return Ok(()); // Released borrowed Child: recovery never migrates it.
    };
    if track.workspace.worktree.is_some() {
        let head = output.data["parent_head"].as_str().ok_or_else(|| {
            CalmError::Internal("attached child missing frozen parent HEAD".into())
        })?;
        workspace_runtime::track_worktree::ensure_track_worktree_from_base(
            &track,
            crate::workspace_service::ports::ServerWorkspacePorts,
            head.to_owned(),
            crate::workspace_service::ports::observe,
        )
        .await?;
    }
    Ok(())
}

/// Child-owned read-only admission proof; the shared lifecycle never prepares or repairs.
pub(crate) struct WorkspaceReady(());

pub(crate) async fn workspace_ready_tx(tx: &mut Tx<'_>, track_id: &str) -> Result<WorkspaceReady> {
    let creation: Option<(String, String)> = sqlx::query_as(
        "SELECT phase,tx_output_json FROM operations
         WHERE kind=?1 AND target_type='track' AND target_id=?2
           AND json_extract(payload_json,'$.child_track.continue') IS NULL",
    )
    .bind(CHILD_TRACK_KIND)
    .bind(track_id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((phase, raw)) = creation {
        let output: TxOutput = serde_json::from_str(&raw)?;
        if !matches!(
            phase.as_str(),
            "spawn_started" | "spawn_succeeded" | "succeeded"
        ) || output.data.get("workspace_error").is_some()
        {
            return Err(CalmError::Conflict(
                "child Workspace preparation has not succeeded".into(),
            ));
        }
        let frozen = workspace_contract(&output)?;
        let current: Track =
            sqlx::query_as::<_, crate::db::rows::TrackRow>("SELECT * FROM tracks WHERE id=?1")
                .bind(track_id)
                .fetch_one(&mut **tx)
                .await?
                .into();
        let matches = match frozen {
            Some(frozen) => frozen.id == current.id && frozen.workspace == current.workspace,
            None => {
                output.target_id.as_deref() == Some(track_id)
                    && current.workspace.worktree.is_none()
                    && output.data["cwd"].as_str() == Some(current.workspace.path.as_str())
            }
        };
        if !matches {
            return Err(CalmError::Conflict(
                "child Workspace qualification changed".into(),
            ));
        }
    }
    Ok(WorkspaceReady(()))
}

/// Durable original-worker completion survives cold boot; a registry handle is insufficient.
pub(crate) async fn activation_succeeded(
    repo: &dyn crate::db::Repo,
    runtime: &super::super::OperationRuntime,
    card: &str,
    worker: &str,
) -> Result<bool> {
    let db = repo
        .sqlite_db()
        .ok_or_else(|| CalmError::Internal("child activation requires SQLite".into()))?;
    let activation: Option<Option<String>> = sqlx::query_scalar(
        "SELECT ws.spawn_op_id FROM cards c JOIN worker_sessions ws ON ws.id=c.session_id
         WHERE c.id=?1 AND ws.id=?2",
    )
    .bind(card)
    .bind(worker)
    .fetch_optional(db.read_pool())
    .await?;
    let Some(activation) = activation.flatten() else {
        return Ok(false);
    };
    Ok(runtime
        .operation_result(&activation)
        .await?
        .is_some_and(|completed| {
            matches!(completed.outcome,
            super::super::OperationOutcome::Succeeded { ref result }
            | super::super::OperationOutcome::SucceededViaCollision { ref result, .. }
            if result.get("worker_session_id").and_then(Value::as_str) == Some(worker))
        }))
}
