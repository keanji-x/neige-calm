//! Trusted creation metadata. Display names and templates never grant report access.

use crate::error::{CalmError, Result};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{AppContext, ToolCallIdentity, require_role};
use crate::model::CardRole;
use chrono_tz::Tz;
use sqlx::{Sqlite, Transaction};

#[derive(Clone)]
pub(crate) struct ManagedTrackIdentity {
    pub owner: String,
    pub identity: String,
    pub report_read_scope: ReportReadScope,
    pub report_time_zone: Tz,
    pub tool_policy: ToolPolicy,
    pub kernel_controls_lifecycle: bool,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(crate) enum ReportReadScope {
    Area,
    Workspace,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(crate) enum ToolPolicy {
    Standard,
    Reports,
}

impl ToolPolicy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Reports => "reports",
        }
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct ManagedTrackBinding {
    pub track_id: String,
    pub area_id: String,
    pub report_read_scope: String,
    pub report_time_zone: String,
    pub tool_policy: String,
    pub kernel_controls_lifecycle: bool,
    pub template_id: Option<String>,
}

impl ManagedTrackBinding {
    pub fn matches(
        &self,
        requested: &ManagedTrackIdentity,
        area_id: &str,
        template_id: &Option<String>,
    ) -> bool {
        let scope = match requested.report_read_scope {
            ReportReadScope::Area => "area",
            ReportReadScope::Workspace => "workspace",
        };
        self.area_id == area_id
            && self.report_read_scope == scope
            && self.report_time_zone == requested.report_time_zone.name()
            && self.tool_policy == requested.tool_policy.as_str()
            && self.kernel_controls_lifecycle == requested.kernel_controls_lifecycle
            && self.template_id == *template_id
    }
}

pub(crate) async fn bind_tx(
    tx: &mut Transaction<'_, Sqlite>,
    identity: &ManagedTrackIdentity,
    track_id: &str,
) -> Result<()> {
    let scope = match identity.report_read_scope {
        ReportReadScope::Area => "area",
        ReportReadScope::Workspace => "workspace",
    };
    sqlx::query(concat!(
        "INSERT INTO ",
        "managed_track_identities(owner,identity,track_id,report_read_scope,",
        "report_time_zone,tool_policy,kernel_controls_lifecycle)",
        " VALUES(?1,?2,?3,?4,?5,?6,?7)",
    ))
    .bind(&identity.owner)
    .bind(&identity.identity)
    .bind(track_id)
    .bind(scope)
    .bind(identity.report_time_zone.name())
    .bind(identity.tool_policy.as_str())
    .bind(identity.kernel_controls_lifecycle)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Check the live bound card and its declared read grant on every call. No write ledger entry.
pub(crate) async fn require_workspace_reports(
    ctx: &AppContext,
    identity: &ToolCallIdentity,
) -> std::result::Result<Tz, RpcError> {
    require_role(identity, CardRole::Planner)?;
    let denied = || {
        RpcError::custom(
            -32403,
            "workspace report reads require a kernel-issued grant",
        )
    };
    let card = ctx
        .repo
        .card_get(&identity.card_id)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?
        .ok_or_else(denied)?;
    if identity.track_id.as_deref() != Some(card.track_id.as_str())
        || ctx
            .repo
            .card_role_get(&identity.card_id)
            .await
            .map_err(|e| RpcError::internal(e.to_string()))?
            != Some(CardRole::Planner)
    {
        return Err(denied());
    }
    let track = ctx
        .repo
        .track_get(card.track_id.as_str())
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?
        .ok_or_else(denied)?;
    if track.area_id.as_str() != identity.area_id {
        return Err(denied());
    }
    let pool = ctx
        .sqlite_pool
        .as_ref()
        .ok_or_else(|| RpcError::internal("workspace reports require sqlite"))?;
    let zone: Option<String> = sqlx::query_scalar("SELECT report_time_zone FROM managed_track_identities WHERE track_id=?1 AND report_read_scope='workspace'")
        .bind(card.track_id.as_str()).fetch_optional(pool).await
        .map_err(|e| RpcError::internal(e.to_string()))?;
    zone.ok_or_else(denied)?
        .parse()
        .map_err(|e| RpcError::internal(format!("report time zone: {e}")))
}

pub(crate) async fn refuse_track_delete(
    pool: Option<sqlx::SqlitePool>,
    track_id: &str,
) -> Result<()> {
    if let Some(pool) = pool {
        let owned: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM managed_track_identities WHERE track_id=?1)",
        )
        .bind(track_id)
        .fetch_one(&pool)
        .await?;
        if owned {
            return Err(CalmError::Conflict(
                "This Track has a kernel-owned creation identity; its history must be retained."
                    .into(),
            ));
        }
    }
    Ok(())
}

/// A declared report-planning profile has no worker, terminal, lifecycle or plugin mutation tools.
/// These are owner-defined protocol identifiers; no application/template identity is consulted.
pub(crate) fn report_planning_tool(name: &str) -> bool {
    use crate::mcp_server::tools::{
        track_file, track_history, track_report, track_report_blocks, track_state,
        workspace_reports,
    };
    matches!(
        name,
        track_report::TOOL_REPORT_READ
            | track_report_blocks::TOOL_REPORT_COMMIT
            | track_report_blocks::TOOL_REPORT_WRITE
            | track_report_blocks::TOOL_REPORT_KINDS
            | track_state::TOOL_TRACK_STATE
            | track_file::TOOL_TRACK_LS
            | track_file::TOOL_TRACK_CAT
            | track_history::TOOL_TRACK_LOG
            | track_history::TOOL_TRACK_DIFF
            | track_history::TOOL_TRACK_SHOW
            | workspace_reports::TOOL_WORKSPACE_REPORTS
            | workspace_reports::TOOL_WORKSPACE_REPORT
            | workspace_reports::TOOL_WORKSPACE_CHANGES
            | workspace_reports::TOOL_WORKSPACE_EDITS
    )
}

pub(crate) async fn reports_only_card(
    ctx: &AppContext,
    card_id: &str,
) -> std::result::Result<bool, RpcError> {
    let Some(pool) = ctx.sqlite_pool.as_ref() else {
        return Ok(false);
    };
    sqlx::query_scalar(concat!(
        "SELECT EXISTS(SELECT 1 FROM managed_track_identities m JOIN cards c ON ",
        "c.track_id=m.track_id WHERE c.id=?1 AND c.role=?2 AND m.tool_policy='reports')",
    ))
    .bind(card_id)
    .bind(CardRole::Planner.as_db_str())
    .fetch_one(pool)
    .await
    .map_err(|e| RpcError::internal(e.to_string()))
}

pub(crate) async fn require_tool_allowed(
    ctx: &AppContext,
    identity: &ToolCallIdentity,
    name: &str,
) -> std::result::Result<(), RpcError> {
    if !report_planning_tool(name) && reports_only_card(ctx, &identity.card_id).await? {
        return Err(RpcError::custom(
            -32403,
            "This Planner may read reports and maintain its own report only.",
        ));
    }
    Ok(())
}

pub(crate) async fn kernel_controls_lifecycle(ctx: &AppContext, track_id: &str) -> Result<bool> {
    let Some(pool) = ctx.sqlite_pool.as_ref() else {
        return Ok(false);
    };
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM managed_track_identities WHERE track_id=?1 AND kernel_controls_lifecycle=1)")
        .bind(track_id).fetch_one(pool).await?)
}

pub(crate) async fn creation_identity(
    ctx: &AppContext,
    track_id: &str,
) -> Result<Option<serde_json::Value>> {
    let Some(pool) = ctx.sqlite_pool.as_ref() else {
        return Ok(None);
    };
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT owner,identity,report_time_zone FROM managed_track_identities WHERE track_id=?1",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(owner,identity,time_zone)|serde_json::json!({"owner":owner,"identity":identity,"time_zone":time_zone})))
}
