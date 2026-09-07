use crate::codex_appserver::{DynamicToolCallParams, InputItem};
use crate::db::{Repo, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::model::now_ms;
use calm_types::task_recovery::TaskRecoveryCapability;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Sqlite, Transaction};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Action {
    pub key: String,
    pub expected_attempt_id: String,
    pub event_id: i64,
    pub request_key: String,
    pub capability: TaskRecoveryCapability,
}
#[derive(Clone, Debug)]
pub(crate) struct BoundTurn {
    pub session_id: String,
    pub track_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub actions: Vec<Action>,
}

pub(crate) async fn register(repo: &dyn Repo, card_id: &str, thread_id: &str) -> Result<()> {
    let card_id = card_id.to_owned();
    let thread_id = thread_id.to_owned();
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        // The role-less deferred mint path has no persisted Planner card and
        // must not manufacture a registration. Normal mint checks this first.
        let track: Option<String> = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1 AND role='planner'")
            .bind(&card_id).fetch_optional(&mut **tx).await?;
        let track = track.ok_or_else(|| CalmError::Forbidden("semantic recovery requires a persisted Planner card".into()))?;
        sqlx::query("INSERT INTO planner_recovery_threads(thread_id,track_id,card_id,registered_at_ms) VALUES(?1,?2,?3,?4)")
            .bind(thread_id).bind(track).bind(card_id).bind(now_ms()).execute(&mut **tx).await?;
        Ok(())
    })).await
}

pub(crate) async fn registered(repo: &dyn Repo, card_id: &str, thread_id: &str) -> Result<bool> {
    let pool = repo
        .sqlite_pool()
        .ok_or_else(|| CalmError::Internal("semantic recovery requires SQLite".into()))?;
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM planner_recovery_threads WHERE thread_id=?1 AND card_id=?2",
    )
    .bind(thread_id)
    .bind(card_id)
    .fetch_one(&pool)
    .await?
        == 1)
}

/// Deterministic representability limits only. Storage/identity failures are
/// errors, not invitations to silently weaken semantic recovery.
pub(crate) fn binding_problem(input: &str, actions: &[Action]) -> Option<&'static str> {
    if input.len() > 4 * 1024 * 1024 {
        return Some("input exceeds the 4 MiB semantic binding limit");
    }
    if actions.len() > 128 {
        return Some("batch exceeds the 128 semantic action limit");
    }
    let mut keys = std::collections::HashSet::new();
    if actions.iter().any(|action| !keys.insert(&action.key)) {
        return Some("multiple original recovery facts name the same task key");
    }
    None
}

pub(crate) async fn prepare(
    repo: &dyn Repo,
    session: &str,
    track: &str,
    thread: &str,
    input: &[InputItem],
    actions: Vec<Action>,
) -> Result<String> {
    let input = serde_json::to_string(input)?;
    if let Some(problem) = binding_problem(&input, &actions) {
        return Err(CalmError::BadRequest(problem.into()));
    }
    if actions.is_empty()
        || actions.iter().any(|a| {
            !calm_types::report_blocks::tasks::key_is_valid(&a.key)
                || a.expected_attempt_id.is_empty()
                || a.event_id <= 0
                || a.request_key.is_empty()
                || a.request_key.len() > 200
        })
    {
        return Err(CalmError::BadRequest("invalid recovery issuance".into()));
    }
    let actions = serde_json::to_string(&actions)?;
    let id = uuid::Uuid::new_v4().to_string();
    let result = id.clone();
    let session = session.to_owned();
    let track = track.to_owned();
    let thread = thread.to_owned();
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        sqlx::query("INSERT INTO planner_recovery_issuances(id,track_id,session_id,thread_id,input_json,actions_json,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)")
            .bind(id).bind(track).bind(session).bind(thread).bind(input).bind(actions).bind(now_ms()).execute(&mut **tx).await?;
        Ok(())
    })).await?;
    Ok(result)
}

pub(crate) async fn bind_turn(repo: &dyn Repo, issuance: &str, turn: &str) -> Result<()> {
    let issuance = issuance.to_owned();
    let turn = turn.to_owned();
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        let (session,track,thread): (String,String,String) = sqlx::query_as("SELECT session_id,track_id,thread_id FROM planner_recovery_issuances WHERE id=?1")
            .bind(&issuance).fetch_one(&mut **tx).await?;
        let old: Option<String> = sqlx::query_scalar("SELECT issuance_id FROM planner_recovery_turns WHERE thread_id=?1 AND turn_id=?2")
            .bind(&thread).bind(&turn).fetch_optional(&mut **tx).await?;
        if let Some(old) = old {
            return if old==issuance {Ok(())} else {Err(CalmError::Conflict("provider turn is already bound to another issuance".into()))};
        }
        sqlx::query("INSERT INTO planner_recovery_turns(thread_id,turn_id,session_id,track_id,issuance_id,confirmed_at_ms) VALUES(?1,?2,?3,?4,?5,?6)")
            .bind(thread).bind(turn).bind(session).bind(track).bind(issuance).bind(now_ms()).execute(&mut **tx).await?;
        Ok(())
    })).await
}

pub(super) async fn lookup(repo: &dyn Repo, thread: &str, turn: &str) -> Result<Option<BoundTurn>> {
    let thread = thread.to_owned();
    let turn = turn.to_owned();
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        let row: Option<(String,String,String)> = sqlx::query_as("SELECT t.session_id,t.track_id,i.actions_json FROM planner_recovery_turns t JOIN planner_recovery_issuances i ON i.id=t.issuance_id WHERE t.thread_id=?1 AND t.turn_id=?2")
            .bind(&thread).bind(&turn).fetch_optional(&mut **tx).await?;
        let Some((session_id,track_id,actions)) = row else { return Ok(None); };
        let bound = BoundTurn {session_id,track_id,thread_id:thread,turn_id:turn,actions:serde_json::from_str(&actions)?};
        authenticate_tx(tx,&bound).await?;
        Ok(Some(bound))
    })).await
}

/// Rechecked inside the actual recovery transaction, before receipt replay.
/// A replaced session/thread may never use an otherwise authentic old binding.
pub(crate) async fn authenticate_tx(
    tx: &mut Transaction<'_, Sqlite>,
    bound: &BoundTurn,
) -> Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_sessions s JOIN cards c ON c.session_id=s.id AND s.card_id=c.id JOIN planner_recovery_threads r ON r.thread_id=s.thread_id AND r.card_id=c.id AND r.track_id=c.track_id WHERE s.id=?1 AND s.thread_id=?2 AND c.track_id=?3 AND c.role='planner' AND s.state IN ('starting','running','idle','turn_pending')")
        .bind(&bound.session_id).bind(&bound.thread_id).bind(&bound.track_id).fetch_one(&mut **tx).await?;
    if count != 1 {
        return Err(CalmError::Forbidden(
            "the bound Planner session/thread is no longer current".into(),
        ));
    }
    Ok(())
}

pub(super) async fn record_call(
    repo: &dyn Repo,
    bound: &BoundTurn,
    params: &DynamicToolCallParams,
    canonical_args: Vec<u8>,
) -> Result<()> {
    if bound.thread_id != params.thread_id || bound.turn_id != params.turn_id {
        return Err(CalmError::Forbidden(
            "call does not belong to bound provider turn".into(),
        ));
    }
    let bound = bound.clone();
    let params = params.clone();
    let fingerprint = format!("{:x}", Sha256::digest(canonical_args));
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        authenticate_tx(tx,&bound).await?;
        let old: Option<(String,String)> = sqlx::query_as("SELECT tool,arguments_sha256 FROM planner_recovery_calls WHERE thread_id=?1 AND turn_id=?2 AND call_id=?3")
            .bind(&params.thread_id).bind(&params.turn_id).bind(&params.call_id).fetch_optional(&mut **tx).await?;
        if let Some((tool,hash))=old {
            return if tool==params.tool && hash==fingerprint {Ok(())} else {Err(CalmError::Conflict("provider call identity reused with different arguments".into()))};
        }
        sqlx::query("INSERT INTO planner_recovery_calls(thread_id,turn_id,call_id,session_id,track_id,tool,arguments_sha256,received_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)")
            .bind(params.thread_id).bind(params.turn_id).bind(params.call_id).bind(bound.session_id).bind(bound.track_id).bind(params.tool).bind(fingerprint).bind(now_ms()).execute(&mut **tx).await?;
        Ok(())
    })).await
}
