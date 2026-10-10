//! #2493 S1: which task attempt a worker session serves. One owner for two questions over one SQL
//! view, `worker_session_binding` (migration 0161):
//!
//! - **History**: the binding fact `tasks.worker_session_id`, read whether or not the session
//!   still lives (activity, run views, card deletion). [`SessionBinding`] rows.
//! - **Authority**: may this session act for an attempt *now* — the binding and the session's
//!   liveness. [`WorkerBinding`], from [`worker_binding_tx`].
//!
//! [`bind_attempt_tx`] is the one writer: each worker-spawn operation calls it in the transaction
//! that creates the session. A session serves at most one attempt
//! (`tasks_worker_session_once`).

use sqlx::SqliteConnection;

use crate::error::{CalmError, Result};
use crate::model::TaskStatus;

/// One row of `worker_session_binding`: a worker session and the attempt bound to it.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct SessionBinding {
    pub session_id: String,
    pub card_id: Option<String>,
    /// `WorkerSessionState::is_active_authority` of the session's state.
    pub session_active: bool,
    pub attempt_id: Option<String>,
    pub attempt_status: Option<TaskStatus>,
}

/// What a session may do for an attempt now. Every reader names the states it accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerBinding {
    /// The session is active and its attempt is `dispatched` or `running`.
    Live {
        attempt_id: String,
        session_id: String,
        status: TaskStatus,
    },
    /// The session is active and its last attempt ended (`verifying`, `done`, `failed`,
    /// `canceled`).
    Parked {
        last_attempt_id: String,
        session_id: String,
    },
    /// The session is active and was never bound to an attempt (a Planner-opened terminal).
    Unbound { session_id: String },
    /// No active session: it ended, was deleted, or never existed.
    NoSession,
}

/// Whose worker [`worker_binding_tx`] resolves.
#[derive(Clone, Copy, Debug)]
pub enum WorkerOf<'a> {
    Session(&'a str),
    /// The session bound to the attempt.
    Attempt(&'a str),
}

const VIEW_COLUMNS: &str = "session_id, card_id, session_active, attempt_id, attempt_status";

/// Bind `attempt_id` to the session its spawn operation just created, and stamp the card with
/// it in the same statement. Only a `dispatched` attempt with no session binds; anything else,
/// or a session already serving an attempt, is a `Conflict` and the caller's transaction rolls
/// back.
pub async fn bind_attempt_tx(
    conn: &mut SqliteConnection,
    attempt_id: &str,
    session_id: &str,
    card_id: &str,
) -> Result<()> {
    let bound = sqlx::query(
        "UPDATE tasks SET worker_session_id = ?1, worker_card_id = ?2 \
         WHERE id = ?3 AND status = 'dispatched' AND worker_session_id IS NULL",
    )
    .bind(session_id)
    .bind(card_id)
    .bind(attempt_id)
    .execute(&mut *conn)
    .await;
    match bound {
        Ok(done) if done.rows_affected() == 1 => Ok(()),
        Ok(_) => Err(CalmError::Conflict(format!(
            "attempt {attempt_id} is not a dispatched attempt without a worker; it cannot bind \
             worker session {session_id}"
        ))),
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            Err(CalmError::Conflict(format!(
                "worker session {session_id} already serves another attempt; it cannot bind \
                 attempt {attempt_id}"
            )))
        }
        Err(other) => Err(other.into()),
    }
}

/// History by session: the session's row, its attempt whatever either's state. `None` when no
/// such session exists.
pub async fn session_binding_tx(
    conn: &mut SqliteConnection,
    session_id: &str,
) -> Result<Option<SessionBinding>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {VIEW_COLUMNS} FROM worker_session_binding WHERE session_id = ?1"
    ))
    .bind(session_id)
    .fetch_optional(&mut *conn)
    .await?)
}

/// History by attempt: the session the attempt is bound to. `None` when the attempt never bound
/// or its session row is gone.
pub async fn attempt_binding_tx(
    conn: &mut SqliteConnection,
    attempt_id: &str,
) -> Result<Option<SessionBinding>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {VIEW_COLUMNS} FROM worker_session_binding WHERE attempt_id = ?1"
    ))
    .bind(attempt_id)
    .fetch_optional(&mut *conn)
    .await?)
}

/// History by card: the bound session of the card, whatever its state. A card's sessions serve
/// at most one attempt (a `claude-restart` of a bound card is refused), so a second bound session
/// is a `Conflict`.
pub async fn card_binding_tx(
    conn: &mut SqliteConnection,
    card_id: &str,
) -> Result<Option<SessionBinding>> {
    let mut rows: Vec<SessionBinding> = sqlx::query_as(&format!(
        "SELECT {VIEW_COLUMNS} FROM worker_session_binding \
         WHERE card_id = ?1 AND attempt_id IS NOT NULL LIMIT 2"
    ))
    .bind(card_id)
    .fetch_all(&mut *conn)
    .await?;
    if rows.len() > 1 {
        return Err(CalmError::Conflict(format!(
            "worker card {card_id} has more than one bound worker session"
        )));
    }
    Ok(rows.pop())
}

/// History by card, every bound session: a card deletion settles whatever its sessions ran.
pub async fn card_bindings_tx(
    conn: &mut SqliteConnection,
    card_id: &str,
) -> Result<Vec<SessionBinding>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {VIEW_COLUMNS} FROM worker_session_binding \
         WHERE card_id = ?1 AND attempt_id IS NOT NULL ORDER BY attempt_id"
    ))
    .bind(card_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// The run views' card of a run (`run key` = the attempt id) is the card whose session is bound to
/// the attempt. A pre-scheduler worker ran no attempt (its key names no `tasks` row), so nothing is
/// bound: its card payload's `idempotency_key` stays its run key, as the runs views read it before
/// bindings existed. The legacy arm reads no key that names an attempt.
const RUN_CARDS_SQL: &str = "SELECT b.card_id AS card_id, b.attempt_id AS run_key \
       FROM worker_session_binding b JOIN tasks t ON t.id = b.attempt_id \
      WHERE t.track_id = ?1 AND b.card_id IS NOT NULL \
     UNION \
     SELECT c.id AS card_id, json_extract(c.payload, '$.idempotency_key') AS run_key \
       FROM cards c \
      WHERE c.track_id = ?1 AND c.role = 'worker' \
        AND json_extract(c.payload, '$.idempotency_key') IS NOT NULL \
        AND NOT EXISTS (SELECT 1 FROM tasks t \
                         WHERE t.id = json_extract(c.payload, '$.idempotency_key'))";

/// Every `(card_id, run key)` of the track's runs ([`RUN_CARDS_SQL`]), the run ↔ card relation of
/// the track's history views.
pub async fn track_card_bindings_tx(
    conn: &mut SqliteConnection,
    track_id: &str,
) -> Result<Vec<(String, String)>> {
    Ok(sqlx::query_as(&format!(
        "SELECT card_id, run_key FROM ({RUN_CARDS_SQL}) ORDER BY run_key, card_id"
    ))
    .bind(track_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// The cards of one run of the track ([`RUN_CARDS_SQL`]).
pub async fn run_card_ids_tx(
    conn: &mut SqliteConnection,
    track_id: &str,
    run_key: &str,
) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT card_id FROM ({RUN_CARDS_SQL}) WHERE run_key = ?2 ORDER BY card_id"
    ))
    .bind(track_id)
    .bind(run_key)
    .fetch_all(&mut *conn)
    .await?)
}

/// The run key of one card of the track ([`RUN_CARDS_SQL`]); `None` for a card that ran none.
pub async fn run_key_of_card_tx(
    conn: &mut SqliteConnection,
    track_id: &str,
    card_id: &str,
) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT run_key FROM ({RUN_CARDS_SQL}) WHERE card_id = ?2 ORDER BY run_key LIMIT 1"
    ))
    .bind(track_id)
    .bind(card_id)
    .fetch_optional(&mut *conn)
    .await?)
}

/// History for a session bound to no attempt: whether another session of its card is bound (the
/// card ran an attempt, so a later unbound session of it is not a plain terminal).
pub async fn unbound_session_belongs_to_a_task_tx(
    conn: &mut SqliteConnection,
    session_id: &str,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM worker_session_binding b \
                        JOIN worker_sessions ws ON ws.card_id = b.card_id \
                        WHERE ws.id = ?1 AND b.attempt_id IS NOT NULL)",
    )
    .bind(session_id)
    .fetch_one(&mut *conn)
    .await?)
}

/// The view rows that carry authority: an active session's. One predicate for every selector.
const AUTHORITY: &str = "session_active";

/// Authority: what `of`'s active session may act for now.
pub async fn worker_binding_tx(
    conn: &mut SqliteConnection,
    of: WorkerOf<'_>,
) -> Result<WorkerBinding> {
    let (selector, key) = match of {
        WorkerOf::Session(id) => ("session_id = ?1", id),
        WorkerOf::Attempt(id) => (
            "session_id = (SELECT worker_session_id FROM tasks WHERE id = ?1)",
            id,
        ),
    };
    let row: Option<SessionBinding> = sqlx::query_as(&format!(
        "SELECT {VIEW_COLUMNS} FROM worker_session_binding WHERE {selector} AND {AUTHORITY}"
    ))
    .bind(key)
    .fetch_optional(&mut *conn)
    .await?;
    row.map_or(Ok(WorkerBinding::NoSession), authority_of)
}

/// The authority an active session's view row grants.
fn authority_of(row: SessionBinding) -> Result<WorkerBinding> {
    let session_id = row.session_id;
    let (Some(attempt_id), Some(status)) = (row.attempt_id, row.attempt_status) else {
        return Ok(WorkerBinding::Unbound { session_id });
    };
    match status {
        TaskStatus::Dispatched | TaskStatus::Running => Ok(WorkerBinding::Live {
            attempt_id,
            session_id,
            status,
        }),
        TaskStatus::Verifying | TaskStatus::Done | TaskStatus::Failed | TaskStatus::Canceled => {
            Ok(WorkerBinding::Parked {
                last_attempt_id: attempt_id,
                session_id,
            })
        }
        TaskStatus::Pending => Err(CalmError::Internal(format!(
            "attempt {attempt_id} is bound to worker session {session_id} while pending"
        ))),
    }
}
