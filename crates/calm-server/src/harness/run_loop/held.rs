//! The run loop's side of the held-request table (#2348): it is the only consumer of the
//! adapter's channel, the only writer of `hold` asks and `ask.withdrawn`, and the only place the
//! turn watchdog learns that the turn is waiting on the user.
//!
//! One mechanism closes asks, the sweep: every open `hold` ask of this session whose request the
//! table does not hold is withdrawn. It runs when the harness is built (boot, a respawn after a
//! registry miss, system-error recovery), when a turn starts and when a turn ends, and after a
//! `Gone` or `ConnectionLost` took entries out. The responders taken out are dropped only after
//! the sweep, so a refusal never reaches the provider before the ask is withdrawn. A session
//! without live authority writes nothing; its responders are still let go.

use std::collections::HashSet;
use std::time::Instant;

use calm_types::worker::WorkerSessionId;

use super::Inner;
use crate::db::write_with_actor_events_typed;
use crate::event::AskQuestion;
use crate::harness::held_requests::{ConnectionId, HeldRequestMessage, HeldResponder, RequestKey};
use crate::harness::state::HarnessState;
use crate::ids::ActorId;
use crate::state::WriteContext;

fn session(inner: &Inner) -> WorkerSessionId {
    WorkerSessionId::from(inner.worker_session_id.as_str())
}

fn write_context(inner: &Inner) -> WriteContext {
    WriteContext::new(
        inner.card_role_cache.clone(),
        inner.track_area_cache.clone(),
    )
}

pub(super) async fn on_message(inner: &Inner, message: HeldRequestMessage) {
    match message {
        HeldRequestMessage::Open {
            request_key,
            connection,
            questions,
            responder,
        } => open(inner, request_key, connection, questions, responder).await,
        HeldRequestMessage::Gone { request_key } => {
            let taken = inner.held_requests.take_request(&request_key);
            if !taken.is_empty() {
                sweep(inner).await;
            }
            drop(taken);
        }
        HeldRequestMessage::ConnectionLost { connection } => {
            let taken = inner.held_requests.take_connection(&connection);
            if !taken.is_empty() {
                sweep(inner).await;
            }
            drop(taken);
        }
    }
}

/// Ask the user, then hold the request under the ask's id. A refused write (the session lost its
/// authority, the questions are not one question with options, the database failed) asks nothing
/// and drops the responder, which refuses the request.
async fn open(
    inner: &Inner,
    request_key: RequestKey,
    connection: ConnectionId,
    questions: Vec<AskQuestion>,
    responder: Box<dyn HeldResponder>,
) {
    let card = inner.card_id.clone();
    let actor = ActorId::AiPlannerSession(session(inner));
    let written = write_with_actor_events_typed::<(), _>(
        inner.repo.as_ref(),
        None,
        &inner.events,
        &write_context(inner),
        move |tx| {
            Box::pin(async move {
                let (scope, event) =
                    crate::ask::hold_ask_requested_tx(tx, &card, questions).await?;
                Ok(((), vec![(actor, scope, event)]))
            })
        },
    )
    .await;
    match written {
        Ok((_, ids)) if ids.len() == 1 => {
            inner
                .held_requests
                .insert(ids[0], request_key, connection, responder);
        }
        Ok((_, ids)) => tracing::error!(
            worker_session_id = %inner.worker_session_id,
            ?ids,
            "planner harness: a paused request wrote an unexpected number of asks; refused"
        ),
        Err(error) => tracing::warn!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            %error,
            "planner harness: could not ask the user about a paused request; refused"
        ),
    }
}

/// Withdraw every open `hold` ask of this session whose request the table does not hold.
pub(super) async fn sweep(inner: &Inner) {
    let keep: HashSet<i64> = inner.held_requests.ask_ids();
    let session = session(inner);
    let Some(pool) = inner.repo.sqlite_pool() else {
        return;
    };
    match crate::ask::has_hold_asks_to_withdraw(&pool, &inner.track_id, &session, &keep).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            tracing::warn!(
                worker_session_id = %inner.worker_session_id,
                %error,
                "planner harness: cannot read the open paused-request asks; not swept"
            );
            return;
        }
    }
    let track = inner.track_id.clone();
    let written = write_with_actor_events_typed::<(), _>(
        inner.repo.as_ref(),
        None,
        &inner.events,
        &write_context(inner),
        move |tx| {
            Box::pin(async move {
                let withdrawn =
                    crate::ask::withdraw_hold_asks_tx(tx, &track, &session, &keep).await?;
                Ok(((), withdrawn))
            })
        },
    )
    .await;
    if let Err(error) = written {
        tracing::info!(
            worker_session_id = %inner.worker_session_id,
            %error,
            "planner harness: paused-request asks not withdrawn"
        );
    }
}

/// A turn ended: none of its requests is waited on any more.
pub(super) async fn end_turn(inner: &Inner) {
    let taken = inner.held_requests.take_all();
    sweep(inner).await;
    drop(taken);
}

/// Let every held request go without writing: the run loop is stopping.
pub(super) fn release_all(inner: &Inner) {
    drop(inner.held_requests.take_all());
}

/// The turn watchdog does not count time the turn spends waiting on the user: while the table
/// holds a request, each tick moves the running turn's `watchdog_from` forward by the time since
/// the last tick that saw it held. Moving that instant, rather than skipping the check, keeps the
/// waited time out of the watchdog after the answer too. The turn's `started_at` stays put: the
/// wait is still part of how long the turn has run.
pub(super) async fn pause_watchdog_while_held(inner: &Inner) {
    let now = Instant::now();
    let held = !inner.held_requests.is_empty();
    let mut held_since = inner.watchdog_held_since.lock().await;
    if let Some(since) = *held_since {
        let waited = now.saturating_duration_since(since);
        if let HarnessState::TurnRunning { watchdog_from, .. } = &mut *inner.state.lock().await {
            *watchdog_from = (*watchdog_from + waited).min(now);
        }
    }
    *held_since = held.then_some(now);
}
