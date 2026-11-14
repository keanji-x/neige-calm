//! Server-owned construction facts shared by managed Planner backends.
use crate::acp_planner::config::AcpPlannerHost;
use crate::claude_planner::config::ClaudePlannerHost;
use crate::plugin_host::PluginHost;
use std::sync::Arc;

#[derive(Clone)]
pub struct PlannerWiring {
    pub claude: Arc<ClaudePlannerHost>,
    pub acp: Arc<AcpPlannerHost>,
    pub plugin: Arc<PluginHost>,
}

pub struct PlannerRow<'a> {
    pub binding: crate::harness::profile::PlannerBinding,
    pub worker_session_id: &'a str,
    pub card_id: &'a str,
    pub track_id: &'a str,
    pub prior_total_tokens: i64,
}

#[cfg(any(test, feature = "fixtures"))]
impl PlannerWiring {
    /// Fixtures only: unconfigured local providers and no plugins, for tests that recover
    /// or start managed harnesses.
    pub fn unconfigured_for_test(repo: Arc<dyn crate::db::Repo>) -> Self {
        let route_repo: Arc<dyn crate::db::RouteRepo> = repo;
        Self {
            acp: Arc::new(
                crate::acp_planner::config::AcpPlannerHost::unconfigured_scratch()
                    .expect("ACP host"),
            ),
            claude: Arc::new(
                crate::claude_planner::config::ClaudePlannerHost::unconfigured_scratch()
                    .expect("scratch Claude Planner host"),
            ),
            plugin: Arc::new(crate::plugin_host::PluginHost::new_full(
                Arc::new(crate::plugin_host::PluginRegistry::empty()),
                route_repo,
                std::path::PathBuf::new(),
                std::env::temp_dir().join("calm-claude-planner-test-plugins-data"),
                Vec::new(),
                crate::event::EventBus::new(),
                crate::state::WriteContext::new(
                    crate::card_role_cache::CardRoleCache::new(),
                    crate::track_area_cache::TrackAreaCache::new(),
                ),
            )),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct BoundSessionCwd {
    pub worker_session_id: String,
    pub cwd: String,
}

/// A native identity keeps its original locator through same-path isolation upgrades.
pub(crate) async fn session_cwd(
    repo: &dyn crate::db::Repo,
    row: &PlannerRow<'_>,
    track: &crate::model::Track,
) -> crate::error::Result<String> {
    session_cwd_for(repo, row.card_id, row.worker_session_id, track).await
}

/// Read the locator for this exact runtime, including historical issuance.
pub(crate) async fn session_cwd_for(
    repo: &dyn crate::db::Repo,
    card_id: &str,
    worker_session_id: &str,
    track: &crate::model::Track,
) -> crate::error::Result<String> {
    let card = repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| crate::error::CalmError::NotFound("conversation card".into()))?;
    let session = repo
        .session_get(&worker_session_id.into())
        .await?
        .ok_or_else(|| {
            crate::error::CalmError::Conflict("session cwd owner no longer exists".into())
        })?;
    selected_session_cwd(&card.payload, card_id, &session, track)
}

/// Selection admission captures and rechecks the locator in its carrier transaction.
pub(crate) async fn session_cwd_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
    session: &calm_types::worker::WorkerSession,
    track: &crate::model::Track,
) -> crate::error::Result<String> {
    let shape = crate::db::sqlite::card_execution_shape_tx(tx, card_id).await?;
    selected_session_cwd(&shape.payload, card_id, session, track)
}

fn selected_session_cwd(
    payload: &serde_json::Value,
    card_id: &str,
    session: &calm_types::worker::WorkerSession,
    track: &crate::model::Track,
) -> crate::error::Result<String> {
    use crate::error::CalmError;
    if session.card_id.as_ref().map(|id| id.as_str()) != Some(card_id)
        || session.track_id != track.id
    {
        return Err(CalmError::Conflict("session cwd owner changed".into()));
    }
    if let Some(saved) = payload.get(calm_truth::validation::BOUND_SESSION_CWD_PAYLOAD_KEY) {
        let saved: BoundSessionCwd = serde_json::from_value(saved.clone())?;
        if saved.worker_session_id == session.id.as_str() {
            if !std::path::Path::new(&saved.cwd).is_absolute() {
                return Err(CalmError::Internal(
                    "bound session cwd is not absolute".into(),
                ));
            }
            return Ok(saved.cwd);
        }
        // A new, unbound runtime can inherit a card carrying its predecessor's
        // locator. An existing native identity cannot use that other binding.
        if has_native_identity(session) {
            return Err(CalmError::Conflict("session cwd binding changed".into()));
        }
    }
    Ok(track.workspace.agent_cwd().to_owned())
}

fn has_native_identity(session: &calm_types::worker::WorkerSession) -> bool {
    session.thread_id.is_some()
        || session.agent_session_id.is_some()
        || session
            .handle_state_json
            .as_ref()
            .and_then(|state| state.get("last_thread_id"))
            .is_some_and(|thread| !thread.is_null())
}

/// Record the native identity's locator in the identity-binding transaction.
pub(crate) async fn record_bound_session_cwd_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
    worker_session_id: &str,
    cwd: &str,
) -> crate::error::Result<()> {
    if !std::path::Path::new(cwd).is_absolute() {
        return Err(crate::error::CalmError::Internal(
            "bound session cwd is not absolute".into(),
        ));
    }
    sqlx::query("UPDATE cards SET payload=json_set(payload,?1,json(?2)) WHERE id=?3")
        .bind(format!(
            "$.{}",
            calm_truth::validation::BOUND_SESSION_CWD_PAYLOAD_KEY
        ))
        .bind(serde_json::to_string(&BoundSessionCwd {
            worker_session_id: worker_session_id.into(),
            cwd: cwd.into(),
        })?)
        .bind(card_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Backfill pre-existing bound identities before their Track selects a new cwd.
pub(crate) async fn preserve_bound_track_sessions_cwd_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &str,
    cwd: &str,
) -> crate::error::Result<()> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(concat!(
        "SELECT c.id, ws.id, c.payload FROM cards c ",
        "JOIN worker_sessions ws ON ws.id = c.session_id ",
        "WHERE c.track_id = ?1 AND ws.card_id = c.id ",
        "AND ws.track_id = c.track_id AND ws.mode = 'resumable' ",
        "AND ws.state IN ('starting','running','idle','turn_pending','failed') ",
        "AND (ws.thread_id IS NOT NULL OR ws.agent_session_id IS NOT NULL ",
        "OR json_extract(ws.handle_state_json, '$.last_thread_id') IS NOT NULL) ",
        "ORDER BY c.id"
    ))
    .bind(track_id)
    .fetch_all(&mut **tx)
    .await?;
    for (card_id, worker_session_id, payload) in rows {
        let payload: serde_json::Value = serde_json::from_str(&payload)?;
        if let Some(saved) = payload.get(calm_truth::validation::BOUND_SESSION_CWD_PAYLOAD_KEY) {
            let saved: BoundSessionCwd = serde_json::from_value(saved.clone())?;
            if saved.worker_session_id == worker_session_id {
                if !std::path::Path::new(&saved.cwd).is_absolute() {
                    return Err(crate::error::CalmError::Internal(
                        "bound session cwd is not absolute".into(),
                    ));
                }
                continue;
            }
        }
        record_bound_session_cwd_tx(tx, &card_id, &worker_session_id, cwd).await?;
    }
    Ok(())
}

mod session_cwd_undo;
pub(crate) use session_cwd_undo::SessionCwdUndo;
