//! Terminal owns its original target declaration. Authentication and Area policy
//! remain with their actual owners; the kernel owns insertion/claim conditions.

use super::{TerminalCreateOperationPayload, explicit_terminal_cwd};
use crate::{
    auth::execution::{AcceptedEnvironment, AuthenticatedSession},
    db::sqlite::{track_get_tx, workspace_observation_snapshot},
    error::{CalmError, Result},
    ids::TrackId,
    model::TrackWorkspaceKind,
    operation::{Operation, Tx, admission::OperationAdmission},
    state::TerminalExecutionTransport,
    workspace_service::execution_input::WorkspaceExecutionInput,
};
use async_trait::async_trait;
use calm_session::control::execution::{
    AuthorizationBindingId, ExecutionAuthority, ExecutionGrantId,
};
use calm_types::organization::AreaAccess;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum TerminalExecutionContract {
    Legacy,
    Registered,
}

pub(crate) struct TerminalAdmission<'a> {
    pub(crate) session: &'a AuthenticatedSession,
    pub(crate) transport: &'a TerminalExecutionTransport,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalExecutionGrant {
    pub(crate) authority: ExecutionAuthority,
    environment: AcceptedEnvironment,
    track_id: String,
    area_id: String,
    pub(crate) input: WorkspaceExecutionInput,
    pub(crate) contract: TerminalExecutionContract,
}

#[async_trait]
impl OperationAdmission for TerminalAdmission<'_> {
    async fn accept_tx(&self, tx: &mut Tx<'_>, original: &Operation) -> Result<()> {
        let payload: TerminalCreateOperationPayload =
            serde_json::from_value(original.payload.clone())?;
        if original.kind != "terminal-create" {
            return Err(CalmError::Conflict(
                "Terminal acceptance names another operation kind".into(),
            ));
        }
        let environment = self.session.accept_personal_environment()?;
        let track = track_get_tx(tx, &TrackId::from(payload.request.track_id.clone())).await?;
        crate::organization_access::require_area_access_tx(
            tx,
            track.area_id.as_str(),
            self.session.principal(),
            AreaAccess::Collaborate,
        )
        .await?;
        let input = if let Some(path) = explicit_terminal_cwd(Some(payload.request.cwd)) {
            WorkspaceExecutionInput::ExternalDirectory { path }
        } else if track.workspace.kind == TrackWorkspaceKind::Attached
            && track.workspace.worktree.is_none()
        {
            WorkspaceExecutionInput::ExternalDirectory {
                path: track.workspace.agent_cwd().into(),
            }
        } else {
            WorkspaceExecutionInput::selected(
                &workspace_observation_snapshot(tx, track.id.as_str()).await?,
            )?
        };
        if input.path().trim().is_empty() {
            return Err(CalmError::Conflict(
                "Terminal has no original declared cwd".into(),
            ));
        }
        let authority = ExecutionAuthority {
            grant_id: ExecutionGrantId::fresh()?,
            authorization_binding: AuthorizationBindingId::fresh()?,
            authorization_version: 1,
        };
        let grant = TerminalExecutionGrant {
            authority,
            environment,
            track_id: track.id.to_string(),
            area_id: track.area_id.to_string(),
            input,
            contract: match self.transport {
                TerminalExecutionTransport::Legacy => TerminalExecutionContract::Legacy,
                TerminalExecutionTransport::Registered(_) => TerminalExecutionContract::Registered,
            },
        };
        sqlx::query("INSERT INTO operation_execution_grants(operation_id,original_payload_hash,grant_id,grant_json) VALUES (?1,?2,?3,?4)")
            .bind(&original.id).bind(&original.payload_hash).bind(grant.authority.grant_id.as_str())
            .bind(serde_json::to_string(&grant)?).execute(&mut **tx).await?;
        // The real credential is checked after waiting for the writer and again
        // immediately before its accepted decision commits.
        self.session.revalidate()?;
        Ok(())
    }

    async fn authorize_original_tx(&self, tx: &mut Tx<'_>, original: &Operation) -> Result<()> {
        self.session.revalidate()?;
        match TerminalExecutionGrant::load_tx(tx, original).await? {
            Some(grant) if grant.environment.user_id() == self.session.principal().user_id => {
                Ok(())
            }
            Some(_) => Err(CalmError::Forbidden(
                "original Terminal result belongs to another subject".into(),
            )),
            None if self.session.principal().is_deployment_owner() => Ok(()),
            None => Err(CalmError::Forbidden(
                "legacy Terminal has no accepted subject for this identity".into(),
            )),
        }
    }
}

impl TerminalExecutionGrant {
    /// Original reads do not depend on today's phase, lease, path or selection.
    pub(crate) async fn load_tx(tx: &mut Tx<'_>, original: &Operation) -> Result<Option<Self>> {
        let admitted: i64 = sqlx::query_scalar(
            "SELECT execution_admitted FROM operations WHERE id=?1 AND payload_hash=?2",
        )
        .bind(&original.id)
        .bind(&original.payload_hash)
        .fetch_one(&mut **tx)
        .await?;
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT original_payload_hash,grant_id,grant_json FROM operation_execution_grants WHERE operation_id=?1")
            .bind(&original.id).fetch_optional(&mut **tx).await?;
        let Some((hash, id, json)) = row else {
            return if admitted == 0 {
                Ok(None)
            } else {
                Err(CalmError::Conflict(
                    "original operation's required execution grant is missing".into(),
                ))
            };
        };
        let grant: Self = serde_json::from_str(&json)?;
        if admitted != 1 || hash != original.payload_hash || id != grant.authority.grant_id.as_str()
        {
            return Err(CalmError::Conflict(
                "Terminal accepted grant does not match its original operation".into(),
            ));
        }
        Ok(Some(grant))
    }

    pub(crate) async fn require_current_tx(&self, tx: &mut Tx<'_>) -> Result<()> {
        let subject = self.environment.authorize_current()?;
        let track = track_get_tx(tx, &TrackId::from(self.track_id.clone())).await?;
        if track.area_id.as_str() != self.area_id {
            return Err(CalmError::Conflict(
                "Terminal's original track Area changed".into(),
            ));
        }
        crate::organization_access::require_area_access_subject_tx(
            tx,
            &self.area_id,
            &subject,
            AreaAccess::Collaborate,
        )
        .await?;
        self.input.require_current_tx(tx).await
    }
}
