//! Backend identity comes from immutable Operation/session binding, never card flags.
use super::{OPERATION_KIND, journal};
use crate::db::{RepoEventWrite, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::operation::Tx;
use crate::session_projection_repo::WorkerSessionProjection;
use std::path::PathBuf;

pub(crate) async fn is_isolated_card_tx(tx: &mut Tx<'_>, card_id: &str) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operations WHERE kind=?1 AND target_type='card' AND target_id=?2)")
        .bind(OPERATION_KIND).bind(card_id).fetch_one(&mut **tx).await?)
}
pub(crate) async fn is_isolated_task_tx(tx: &mut Tx<'_>, task_id: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM operations WHERE kind=?1 AND idempotency_key=?2)",
    )
    .bind(OPERATION_KIND)
    .bind(task_id)
    .fetch_one(&mut **tx)
    .await?)
}
pub(crate) async fn is_isolated_card(repo: &dyn RepoEventWrite, card_id: &str) -> Result<bool> {
    let id = card_id.to_string();
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { is_isolated_card_tx(tx, &id).await })
    })
    .await
}
pub(crate) async fn require_shared_card(repo: &dyn RepoEventWrite, card_id: &str) -> Result<()> {
    if is_isolated_card(repo, card_id).await? {
        return Err(CalmError::Conflict(
            "isolated Codex is managed by its task; shared interactive controls are unavailable"
                .into(),
        ));
    }
    Ok(())
}

/// Read-only transcript location. Native socket replacement does not erase logs;
/// only controller reconnect/start requires PrivateHome's live socket verification.
pub fn private_codex_home(
    repo: &dyn RepoEventWrite,
    session: &WorkerSessionProjection,
) -> impl std::future::Future<Output = Result<Option<PathBuf>>> + Send {
    let session = session.clone();
    async move {
        write_in_tx_typed(repo, move |tx| {
            Box::pin(async move {
                if !is_isolated_card_tx(tx, &session.card_id).await? {
                    return Ok(None);
                }
                let op_id: Option<String> = sqlx::query_scalar(
                    "SELECT spawn_op_id FROM worker_sessions WHERE id=?1 AND card_id=?2",
                )
                .bind(&session.id)
                .bind(&session.card_id)
                .fetch_optional(&mut **tx)
                .await?
                .flatten();
                let op_id = op_id.ok_or_else(|| {
                    CalmError::Conflict("isolated session operation binding is missing".into())
                })?;
                let record = journal::load_tx(tx, &op_id).await?;
                if record.request.identity.session_id != session.id
                    || record.request.identity.card_id != session.card_id
                {
                    return Err(CalmError::Conflict(
                        "isolated transcript session binding changed".into(),
                    ));
                }
                let owned = record.session()?;
                let thread = match &owned.phase {
                    crate::dedicated_codex::RequestPhase::ThreadReady { thread_id }
                    | crate::dedicated_codex::RequestPhase::IssuingTurn { thread_id, .. }
                    | crate::dedicated_codex::RequestPhase::TurnActive { thread_id, .. } => {
                        thread_id
                    }
                    _ => {
                        return Err(CalmError::Conflict(
                            "isolated transcript has no acknowledged thread".into(),
                        ));
                    }
                };
                if session.thread_id.as_deref() != Some(thread.as_str()) {
                    return Err(CalmError::Conflict(
                        "isolated transcript thread binding changed".into(),
                    ));
                }
                Ok(Some(owned.endpoint.home.home.clone()))
            })
        })
        .await
    }
}

pub(crate) async fn recorded_worker_kind_tx(
    tx: &mut Tx<'_>,
    task_id: &str,
) -> Result<Option<String>> {
    let kinds:Vec<String>=sqlx::query_scalar("SELECT kind FROM operations WHERE idempotency_key=?1 AND kind IN ('codex-worker','claude-worker','terminal-worker','codex-isolated-worker')")
        .bind(task_id).fetch_all(&mut **tx).await?;
    match kinds.as_slice() {
        [] => Ok(None),
        [kind] => Ok(Some(kind.clone())),
        _ => Err(CalmError::Conflict(
            "task has ambiguous recorded execution backends".into(),
        )),
    }
}

/// Resolve tool grants before the provider startup acknowledgement has attached
/// worker_card_id to the task. The immutable Operation/session binding is already
/// committed at this point. A missing or contradictory isolated binding is never legacy.
pub(crate) async fn delegated_plugin_tools(
    repo: &dyn RepoEventWrite,
    card_id: &str,
    session_id: &str,
    track_id: &str,
) -> Result<Option<Vec<String>>> {
    let (card, session, track) = (
        card_id.to_string(),
        session_id.to_string(),
        track_id.to_string(),
    );
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            if !is_isolated_card_tx(tx, &card).await? {
                return Ok(None);
            }
            let op_id: Option<String> = sqlx::query_scalar(
                "SELECT spawn_op_id FROM worker_sessions WHERE id=?1 AND card_id=?2",
            )
            .bind(&session)
            .bind(&card)
            .fetch_optional(&mut **tx)
            .await?
            .flatten();
            let op_id = op_id.ok_or_else(|| {
                CalmError::Conflict("isolated plugin session binding missing".into())
            })?;
            let record = journal::load_tx(tx, &op_id).await?;
            if record.request.identity.session_id != session
                || record.request.identity.card_id != card
                || record.track_id != track
            {
                return Err(CalmError::Conflict(
                    "isolated plugin identity mismatch".into(),
                ));
            }
            let task = crate::db::sqlite::task_get_tx(tx, &record.request.identity.attempt_id)
                .await?
                .ok_or_else(|| CalmError::Conflict("isolated plugin task missing".into()))?;
            if task.track_id != track || !super::selected(&task)? {
                return Err(CalmError::Conflict(
                    "isolated plugin task binding changed".into(),
                ));
            }
            let context = serde_json::from_str(&task.context_json)?;
            let selection =
                calm_types::task_execution::IsolatedCodexSelection::from_context(&context)
                    .map_err(CalmError::BadRequest)?
                    .ok_or_else(|| {
                        CalmError::Conflict("isolated plugin selection missing".into())
                    })?;
            if record.admission == super::record::Admission::Closed
                || !matches!(
                    task.status,
                    crate::model::TaskStatus::Dispatched | crate::model::TaskStatus::Running
                )
            {
                return Ok(Some(Vec::new()));
            }
            Ok(Some(selection.plugin_tools))
        })
    })
    .await
}
