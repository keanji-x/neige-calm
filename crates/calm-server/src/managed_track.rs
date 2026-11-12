//! Trusted creation metadata and live role-bound workspace report reads.

use crate::db::{Repo, RouteRepo};
use crate::error::{CalmError, Result};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{AppContext, ToolCallIdentity};
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

struct WorkspaceReportReader {
    zone: Tz,
    role: CardRole,
    managed_context: bool,
}

/// Read authority follows the live role; managed metadata configures calendar and context.
async fn workspace_report_reader(
    repo: &dyn RouteRepo,
    pool: Option<crate::db::sqlite::SqliteDb>,
    card_id: &str,
    track_id: &str,
) -> Result<Option<WorkspaceReportReader>> {
    let Some(card) = repo.card_get(card_id).await? else {
        return Ok(None);
    };
    if card.track_id.as_str() != track_id {
        return Ok(None);
    }
    let Some(role) = repo.card_role_get(card_id).await? else {
        return Ok(None);
    };
    let Some(pool) = pool else {
        return Ok(None);
    };
    let metadata: Option<(String, String)> = sqlx::query_as(
        "SELECT report_time_zone,report_read_scope FROM managed_track_identities WHERE track_id=?1",
    )
    .bind(track_id)
    .fetch_optional(pool.read_pool())
    .await?;
    let (zone, managed_context) = match metadata {
        Some((zone, scope)) => (
            zone.parse()
                .map_err(|e| CalmError::Internal(format!("report time zone: {e}")))?,
            scope == "workspace",
        ),
        None => (crate::workspace_reports::TIME_ZONE, false),
    };
    Ok(Some(WorkspaceReportReader {
        zone,
        role,
        managed_context,
    }))
}

/// Preserve the managed report-planning context without adding text to ordinary turns.
pub(crate) async fn workspace_report_instructions(
    repo: &dyn Repo,
    card_id: &str,
    track_id: &str,
) -> Result<Option<&'static str>> {
    const REPORTS: &str = include_str!("../prompts/workspace-reports.md");
    const PLANNER_REPORTS: &str = concat!(
        include_str!("../prompts/workspace-reports.md"),
        "\n",
        include_str!("../prompts/workspace-operations.md")
    );
    Ok(
        workspace_report_reader(repo, repo.sqlite_db(), card_id, track_id)
            .await?
            .filter(|reader| {
                reader.managed_context
                    && matches!(reader.role, CardRole::Planner | CardRole::Assistant)
            })
            .map(|reader| match reader.role {
                CardRole::Planner => PLANNER_REPORTS,
                _ => REPORTS,
            }),
    )
}

/// Validate the live binding once; metadata supplies calendar, never read authority.
pub(crate) async fn report_browse_context(
    ctx: &AppContext,
    identity: &ToolCallIdentity,
) -> std::result::Result<(crate::model::Track, Tz), RpcError> {
    let denied = || RpcError::forbidden("report browsing requires a live bound identity");
    let track_id = identity.track_id.as_deref().ok_or_else(denied)?;
    let track = ctx
        .repo
        .track_get(track_id)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?
        .ok_or_else(denied)?;
    if track.area_id.as_str() != identity.area_id {
        return Err(denied());
    }
    let reader = workspace_report_reader(
        ctx.repo.as_ref(),
        ctx.sqlite_db.clone(),
        &identity.card_id,
        track_id,
    )
    .await
    .map_err(crate::mcp_server::framing::calm_error)?
    .ok_or_else(denied)?;
    if reader.role != identity.role {
        return Err(denied());
    }
    Ok((track, reader.zone))
}

/// The one read of whether a Track has a kernel-owned creation identity.
async fn has_creation_identity(pool: &sqlx::SqlitePool, track_id: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM managed_track_identities WHERE track_id=?1)",
    )
    .bind(track_id)
    .fetch_one(pool)
    .await?)
}

pub(crate) async fn refuse_track_delete(
    pool: Option<crate::db::sqlite::SqliteDb>,
    track_id: &str,
) -> Result<()> {
    if let Some(pool) = pool
        && has_creation_identity(pool.read_pool(), track_id).await?
    {
        return Err(CalmError::Conflict(
            "This Track has a kernel-owned creation identity; its history must be retained.".into(),
        ));
    }
    Ok(())
}

/// A declared report-planning profile has no worker, terminal, lifecycle or plugin mutation tools.
/// These are owner-defined protocol identifiers; no application/template identity is consulted.
pub(crate) fn report_planning_tool(name: &str) -> bool {
    use crate::mcp_server::tools::{
        track_file, track_history, track_report, track_report_blocks, track_state,
    };
    matches!(
        name,
        track_report::TOOL_REPORT_READ
            | track_report_blocks::TOOL_REPORT_COMMIT
            | track_report_blocks::TOOL_REPORT_WRITE
            | track_report_blocks::TOOL_REPORT_DESCRIBE
            | track_state::TOOL_TRACK_STATUS
            | track_file::TOOL_LS
            | track_file::TOOL_CAT
            | track_history::TOOL_TRACK_LOG
            | track_history::TOOL_TRACK_DIFF
            | track_history::TOOL_TRACK_SHOW
    )
}

/// The one track-keyed owner of the reports-only policy query (#2130 §6).
pub(crate) async fn reports_only_track<'e, E>(executor: E, track_id: &str) -> Result<bool>
where
    E: sqlx::SqliteExecutor<'e>,
{
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM managed_track_identities WHERE track_id=?1 AND tool_policy='reports')",
    )
    .bind(track_id)
    .fetch_one(executor)
    .await?)
}

pub(crate) async fn reports_only_card(
    ctx: &AppContext,
    card_id: &str,
) -> std::result::Result<bool, RpcError> {
    let Some(pool) = ctx.sqlite_db.as_ref() else {
        return Ok(false);
    };
    let internal = |e: CalmError| RpcError::internal(e.to_string());
    let track: Option<String> =
        sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1 AND role=?2")
            .bind(card_id)
            .bind(CardRole::Planner.as_db_str())
            .fetch_optional(pool.read_pool())
            .await
            .map_err(|e| internal(e.into()))?;
    match track {
        Some(track) => reports_only_track(pool.read_pool(), &track)
            .await
            .map_err(internal),
        None => Ok(false),
    }
}

pub(crate) async fn require_tool_allowed(
    ctx: &AppContext,
    identity: &ToolCallIdentity,
    name: &str,
) -> std::result::Result<(), RpcError> {
    if !report_planning_tool(name) && reports_only_card(ctx, &identity.card_id).await? {
        return Err(RpcError::forbidden(
            "This Planner may read reports and maintain its own report only.",
        ));
    }
    Ok(())
}

/// A managed Track's Planner starts on its first send. `create_managed_track` is the only writer
/// of `managed_track_identities` and never starts a model (#2024), so no creator start of this
/// Track's cards can be pending: a person's send may run the card's first start (#2184). Any other
/// Track's creator owns its cards' first start, and a send must not pre-empt it.
pub(crate) async fn planner_starts_on_first_send(ctx: &AppContext, track_id: &str) -> Result<bool> {
    let Some(pool) = ctx.sqlite_db.as_ref() else {
        return Ok(false);
    };
    has_creation_identity(pool.read_pool(), track_id).await
}

pub(crate) async fn kernel_controls_lifecycle(ctx: &AppContext, track_id: &str) -> Result<bool> {
    let Some(pool) = ctx.sqlite_db.as_ref() else {
        return Ok(false);
    };
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM managed_track_identities WHERE track_id=?1 AND kernel_controls_lifecycle=1)")
        .bind(track_id).fetch_one(pool.read_pool()).await?)
}

pub(crate) async fn creation_identity(
    ctx: &AppContext,
    track_id: &str,
) -> Result<Option<serde_json::Value>> {
    let Some(pool) = ctx.sqlite_db.as_ref() else {
        return Ok(None);
    };
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT owner,identity,report_time_zone FROM managed_track_identities WHERE track_id=?1",
    )
    .bind(track_id)
    .fetch_optional(pool.read_pool())
    .await?;
    // The stored column stays `report_time_zone`; the tool output names it `timezone` (§4).
    Ok(row.map(|(owner,identity,timezone)|serde_json::json!({"owner":owner,"identity":identity,"timezone":timezone})))
}
