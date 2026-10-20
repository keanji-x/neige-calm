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

pub(super) async fn ensure_child_workspace(output: &TxOutput) -> Result<()> {
    // Older operations already created borrowed children. Do not migrate those on recovery.
    let Some(track) = output.data.get("workspace_track") else {
        return Ok(());
    };
    let track: Track = serde_json::from_value(track.clone())?;
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
