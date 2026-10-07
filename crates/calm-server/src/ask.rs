//! One way to ask the user (#2209): the Planner writes `ask.requested` with one or more questions,
//! and only the user writes `ask.answered`. Every writer goes through this module, inside the
//! caller's immediate transaction, so a question is validated and a track is resolved one way.
//!
//! An ask is delivered one of two ways (#2348). A `wake` ask's answer wakes a new Planner turn.
//! A `hold` ask is a provider request the running turn is paused on: only the harness raises one,
//! its answer goes back to that request, and its own Planner session withdraws it
//! (`ask.withdrawn`) once the request is gone. Each writer has its own entry, so the MCP tool's
//! entry has no way to ask anything but `wake`.

use std::collections::HashSet;

use sqlx::{Row, Sqlite, Transaction};

use crate::error::{CalmError, Result};
use crate::event::{AskAnswer, AskDelivery, AskQuestion, Event, EventScope};
use crate::ids::{ActorId, CardId, TrackId};
use calm_types::worker::WorkerSessionId;

/// At most this many questions in one ask.
pub const MAX_QUESTIONS: usize = 8;
/// At most this many options on one question.
pub const MAX_OPTIONS: usize = 8;
/// Upper bound on a title or an answer, in characters (not bytes).
pub const MAX_TEXT_CHARS: usize = 2000;
/// Upper bound on one option, in characters.
pub const MAX_OPTION_CHARS: usize = 200;

/// A question title a provider adapter builds from the provider's own fields (#2348), cut to
/// [`MAX_TEXT_CHARS`] so a long command is shown in part rather than refused for its length.
pub fn clip_title(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_TEXT_CHARS {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(MAX_TEXT_CHARS - 1).collect();
    clipped.truncate(clipped.trim_end().len());
    clipped.push('…');
    clipped
}

/// Whether the `ask.requested` row aliased `r` is still open: nothing answered or withdrew it, and
/// a `hold` ask's Planner session still has live authority. The session clause is a liveness
/// filter, not a close: a session that recovers from `failed` shows its asks again until the
/// harness sweep withdraws them. The state list is
/// [`calm_types::worker::WorkerSessionState::is_active_authority`]'s. The one spelling of "open"
/// for the projection, the answer transaction and the withdraw transaction; a macro so each
/// statement embeds it with `concat!`.
macro_rules! ask_open_sql {
    () => {
        "NOT EXISTS (SELECT 1 FROM events c \
           WHERE c.scope_track = r.scope_track AND c.kind IN ('ask.answered', 'ask.withdrawn') \
             AND json_extract(c.payload, '$.ask_id') = r.id) \
       AND (json_extract(r.payload, '$.delivery') = 'wake' \
            OR EXISTS (SELECT 1 FROM worker_sessions ws \
                        WHERE json_extract(r.actor, '$.kind') = 'AiPlannerSession' \
                          AND ws.id = json_extract(r.actor, '$.id') \
                          AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')))"
    };
}
pub(crate) use ask_open_sql;

/// [`ask_open_sql!`] as a value.
pub const ASK_OPEN_SQL: &str = ask_open_sql!();

fn bounded(what: &str, text: &str, max: usize) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(CalmError::BadRequest(format!("{what} must not be empty")));
    }
    let chars = text.chars().count();
    if chars > max {
        return Err(CalmError::BadRequest(format!(
            "{what} is {chars} characters; the limit is {max}"
        )));
    }
    Ok(text.to_string())
}

/// The questions trimmed, or why they are refused: 1 to [`MAX_QUESTIONS`] questions, each with a
/// title and at most [`MAX_OPTIONS`] non-empty options.
pub fn validate_questions(questions: Vec<AskQuestion>) -> Result<Vec<AskQuestion>> {
    if questions.is_empty() || questions.len() > MAX_QUESTIONS {
        return Err(CalmError::BadRequest(format!(
            "questions: ask 1 to {MAX_QUESTIONS} questions, not {}",
            questions.len()
        )));
    }
    questions
        .into_iter()
        .enumerate()
        .map(|(i, question)| {
            if question.options.len() > MAX_OPTIONS {
                return Err(CalmError::BadRequest(format!(
                    "questions[{i}].options: at most {MAX_OPTIONS} options"
                )));
            }
            Ok(AskQuestion {
                title: bounded(
                    &format!("questions[{i}].title"),
                    &question.title,
                    MAX_TEXT_CHARS,
                )?,
                options: question
                    .options
                    .iter()
                    .enumerate()
                    .map(|(j, option)| {
                        bounded(
                            &format!("questions[{i}].options[{j}]"),
                            option,
                            MAX_OPTION_CHARS,
                        )
                    })
                    .collect::<Result<_>>()?,
            })
        })
        .collect()
}

/// The answers trimmed, or why they are refused: exactly one per question; an option names one
/// of its question's options; typed text is non-empty and bounded. A `hold` ask takes options
/// only: its answer goes to a provider request that knows nothing but its options.
pub fn validate_answers(
    ask_id: i64,
    questions: &[AskQuestion],
    delivery: AskDelivery,
    answers: Vec<AskAnswer>,
) -> Result<Vec<AskAnswer>> {
    if answers.len() != questions.len() {
        return Err(CalmError::BadRequest(format!(
            "answers: ask {ask_id} has {} questions, got {} answers",
            questions.len(),
            answers.len()
        )));
    }
    answers
        .into_iter()
        .zip(questions)
        .enumerate()
        .map(|(i, (answer, question))| match answer {
            AskAnswer::Option(index) if index < question.options.len() => {
                Ok(AskAnswer::Option(index))
            }
            AskAnswer::Option(index) => Err(CalmError::BadRequest(format!(
                "answers[{i}]: option {index} is not one of the question's {} options",
                question.options.len()
            ))),
            AskAnswer::Text(_) if delivery == AskDelivery::Hold => {
                Err(CalmError::BadRequest(format!(
                    "answers[{i}]: ask {ask_id} waits on a paused request; answer with one of its options"
                )))
            }
            AskAnswer::Text(text) => Ok(AskAnswer::Text(bounded(
                &format!("answers[{i}]"),
                &text,
                MAX_TEXT_CHARS,
            )?)),
        })
        .collect()
}

/// The `wake` ask a Planner card raises with `neige_user_ask`, ready for the caller's event batch
/// under the card's own Planner identity (the role gate refuses any other author). There is no
/// delivery to choose: only the harness raises a `hold` ask ([`hold_ask_requested_tx`]).
pub async fn ask_requested_tx(
    tx: &mut Transaction<'_, Sqlite>,
    planner_card: &CardId,
    questions: Vec<AskQuestion>,
) -> Result<(EventScope, Event)> {
    requested_tx(tx, planner_card, questions, AskDelivery::Wake, None).await
}

/// The `hold` ask the harness raises for one provider request its running turn is paused on
/// (#2348): exactly one question, answered by one of at least one option. Never deduplicated by
/// a provider item: every paused request is its own ask.
pub(crate) async fn hold_ask_requested_tx(
    tx: &mut Transaction<'_, Sqlite>,
    planner_card: &CardId,
    questions: Vec<AskQuestion>,
) -> Result<(EventScope, Event)> {
    match questions.as_slice() {
        [question] if !question.options.is_empty() => {}
        _ => {
            return Err(CalmError::BadRequest(
                "a paused request asks exactly one question with at least one option".into(),
            ));
        }
    }
    requested_tx(tx, planner_card, questions, AskDelivery::Hold, None).await
}

/// The track is the card's own, read in `tx`; it is never taken from the caller. A closed track
/// is not refused: a translated provider question has nobody to refuse, and the projection keeps
/// it either way.
async fn requested_tx(
    tx: &mut Transaction<'_, Sqlite>,
    planner_card: &CardId,
    questions: Vec<AskQuestion>,
    delivery: AskDelivery,
    source_item_id: Option<String>,
) -> Result<(EventScope, Event)> {
    let questions = validate_questions(questions)?;
    let track_id = card_track_tx(tx, planner_card).await?;
    let track = crate::db::sqlite::track_get_tx(tx, &track_id).await?;
    Ok((
        EventScope::Track {
            track: track.id.clone(),
            area: track.area_id,
        },
        Event::AskRequested {
            track_id: track.id,
            questions,
            delivery,
            source_item_id,
        },
    ))
}

/// A question the provider's own model put to the user (#2209 U2), translated into the Planner's
/// `wake` ask, once per provider item: `None` when the card's track already has the
/// `ask.requested` from `source_item_id`, so a duplicate frame, a replay or a restart asks nothing
/// twice. The check and the caller's insert share `tx`.
pub async fn provider_ask_requested_tx(
    tx: &mut Transaction<'_, Sqlite>,
    planner_card: &CardId,
    questions: Vec<AskQuestion>,
    source_item_id: String,
) -> Result<Option<(EventScope, Event)>> {
    let track_id = card_track_tx(tx, planner_card).await?;
    let asked: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM events WHERE scope_track = ?1 AND kind = 'ask.requested' \
            AND json_extract(payload, '$.source_item_id') = ?2)",
    )
    .bind(track_id.as_str())
    .bind(&source_item_id)
    .fetch_one(&mut **tx)
    .await?;
    if asked {
        return Ok(None);
    }
    requested_tx(
        tx,
        planner_card,
        questions,
        AskDelivery::Wake,
        Some(source_item_id),
    )
    .await
    .map(Some)
}

/// The track `card` lives on, read in `tx`.
async fn card_track_tx(tx: &mut Transaction<'_, Sqlite>, card: &CardId) -> Result<TrackId> {
    let track_id: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id = ?1")
        .bind(card.as_str())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card}")))?;
    Ok(TrackId::from(track_id))
}

/// One persisted `ask.requested`, as the answer path reads it.
#[derive(Debug, Clone)]
pub struct StoredAsk {
    pub questions: Vec<AskQuestion>,
    pub delivery: AskDelivery,
    /// Who raised it; a `hold` ask's is its harness's Planner session.
    pub actor: ActorId,
}

impl StoredAsk {
    /// The Planner session whose harness holds this ask's provider request; `None` for an ask
    /// that is not a session's `hold` ask.
    pub fn holding_session(&self) -> Option<&WorkerSessionId> {
        match (&self.delivery, &self.actor) {
            (AskDelivery::Hold, ActorId::AiPlannerSession(session)) => Some(session),
            _ => None,
        }
    }
}

fn stored_ask_from_row(ask_id: i64, row: &sqlx::sqlite::SqliteRow) -> Result<StoredAsk> {
    let payload: serde_json::Value = serde_json::from_str(row.get::<&str, _>("payload"))?;
    let actor: ActorId = serde_json::from_str(row.get::<&str, _>("actor"))?;
    let Event::AskRequested {
        questions,
        delivery,
        ..
    } = Event::from_kind_and_payload("ask.requested", payload)?
    else {
        return Err(CalmError::Internal(format!(
            "ask {ask_id}: the stored row is not an ask.requested"
        )));
    };
    Ok(StoredAsk {
        questions,
        delivery,
        actor,
    })
}

/// The `ask.requested` `ask_id` on `track`, read in `tx`, open or not; not found unless it is
/// on this track.
pub async fn stored_ask_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    ask_id: i64,
) -> Result<StoredAsk> {
    let row = sqlx::query(
        "SELECT payload, actor FROM events \
          WHERE id = ?1 AND kind = 'ask.requested' AND scope_track = ?2",
    )
    .bind(ask_id)
    .bind(track.as_str())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| CalmError::NotFound(format!("ask {ask_id} on track {track}")))?;
    stored_ask_from_row(ask_id, &row)
}

/// The `ask.requested` `ask_id` on `track`, open or not, through the track event reader; not
/// found unless it is on this track. A read before the answer transaction, which reads it again.
pub async fn stored_ask(
    repo: &dyn crate::db::RepoEventWrite,
    track: &TrackId,
    ask_id: i64,
) -> Result<StoredAsk> {
    let row = repo
        .events_for_track(track.as_str(), &["ask.requested"], Some(ask_id - 1))
        .await?
        .into_iter()
        .find(|row| row.id == ask_id)
        .ok_or_else(|| CalmError::NotFound(format!("ask {ask_id} on track {track}")))?;
    match row.event {
        Event::AskRequested {
            questions,
            delivery,
            ..
        } => Ok(StoredAsk {
            questions,
            delivery,
            actor: row.actor,
        }),
        _ => Err(CalmError::Internal(format!(
            "ask {ask_id}: the stored row is not an ask.requested"
        ))),
    }
}

/// Whether the `ask.requested` `ask_id` is open by [`ASK_OPEN_SQL`], read in `tx`.
async fn is_open_tx(tx: &mut Transaction<'_, Sqlite>, ask_id: i64) -> Result<bool> {
    Ok(sqlx::query_scalar(concat!(
        "SELECT EXISTS (SELECT 1 FROM events r \
          WHERE r.id = ?1 AND r.kind = 'ask.requested' AND ",
        ask_open_sql!(),
        ")"
    ))
    .bind(ask_id)
    .fetch_one(&mut **tx)
    .await?)
}

/// The `ask.answered` for `ask_id` on `track`, ready for the caller's event batch as the user's
/// (the role gate refuses any other author): the ask exists on this
/// track, is still open, and gets one valid answer per question ([`validate_answers`]). A wake
/// answer is not limited to the options; the Planner reads it and decides.
pub async fn ask_answered_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    ask_id: i64,
    answers: Vec<AskAnswer>,
) -> Result<(EventScope, Event)> {
    let track_row = crate::db::sqlite::track_get_tx(tx, track).await?;
    let ask = stored_ask_tx(tx, track, ask_id).await?;
    let answers = validate_answers(ask_id, &ask.questions, ask.delivery, answers)?;
    if !is_open_tx(tx, ask_id).await? {
        return Err(CalmError::Conflict(format!(
            "ask {ask_id} is no longer open: it is answered, or its paused request is gone"
        )));
    }
    Ok((
        EventScope::Track {
            track: track_row.id.clone(),
            area: track_row.area_id,
        },
        Event::AskAnswered {
            ask_id,
            track_id: track_row.id,
            answers,
        },
    ))
}

/// The open `hold` asks `session` raised on `track`, oldest first.
async fn open_hold_asks<'e, E: sqlx::SqliteExecutor<'e>>(
    executor: E,
    track: &TrackId,
    session: &WorkerSessionId,
) -> Result<Vec<i64>> {
    Ok(sqlx::query_scalar(concat!(
        "SELECT r.id FROM events r \
          WHERE r.scope_track = ?1 AND r.kind = 'ask.requested' \
            AND json_extract(r.payload, '$.delivery') = 'hold' \
            AND json_extract(r.actor, '$.kind') = 'AiPlannerSession' \
            AND json_extract(r.actor, '$.id') = ?2 AND ",
        ask_open_sql!(),
        " ORDER BY r.id"
    ))
    .bind(track.as_str())
    .bind(session.as_str())
    .fetch_all(executor)
    .await?)
}

/// Whether [`withdraw_hold_asks_tx`] has anything to withdraw: one autocommit read, so a sweep
/// with nothing to close takes no write lock.
pub(crate) async fn has_hold_asks_to_withdraw(
    pool: &sqlx::SqlitePool,
    track: &TrackId,
    session: &WorkerSessionId,
    keep: &HashSet<i64>,
) -> Result<bool> {
    Ok(open_hold_asks(pool, track, session)
        .await?
        .iter()
        .any(|ask_id| !keep.contains(ask_id)))
}

/// One `ask.withdrawn` per open `hold` ask `session` raised on `track` that `keep` does not name,
/// ready for the caller's event batch as `session`'s own: the harness's sweep, which closes every
/// ask whose provider request its table no longer holds. Empty when there is nothing to close.
/// The author is the session, so the role gate refuses a session that lost live authority; the
/// caller then only lets the requests go.
pub(crate) async fn withdraw_hold_asks_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    session: &WorkerSessionId,
    keep: &HashSet<i64>,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let open = open_hold_asks(&mut **tx, track, session).await?;
    if open.iter().all(|ask_id| keep.contains(ask_id)) {
        return Ok(Vec::new());
    }
    let track_row = crate::db::sqlite::track_get_tx(tx, track).await?;
    let scope = EventScope::Track {
        track: track_row.id.clone(),
        area: track_row.area_id,
    };
    Ok(open
        .into_iter()
        .filter(|ask_id| !keep.contains(ask_id))
        .map(|ask_id| {
            (
                ActorId::AiPlannerSession(session.clone()),
                scope.clone(),
                Event::AskWithdrawn {
                    ask_id,
                    track_id: track_row.id.clone(),
                },
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(title: &str, options: &[&str]) -> AskQuestion {
        AskQuestion {
            title: title.into(),
            options: options.iter().map(|o| (*o).to_string()).collect(),
        }
    }

    /// The open clause's state list is exactly the live-authority states.
    #[test]
    fn open_sql_session_states_are_the_live_authority_states() {
        use calm_types::worker::WorkerSessionState as S;
        for state in [
            S::Starting,
            S::Running,
            S::Idle,
            S::TurnPending,
            S::Exited,
            S::Failed,
            S::Superseded,
        ] {
            assert_eq!(
                ASK_OPEN_SQL.contains(&format!("'{}'", state.as_db_str())),
                state.is_active_authority(),
                "{state:?}"
            );
        }
    }

    #[test]
    fn answers_name_an_option_or_carry_text_and_hold_takes_options_only() {
        let questions = vec![q("Merge?", &["Merge", "Hold"]), q("Branch?", &[])];
        assert_eq!(
            validate_answers(
                1,
                &questions,
                AskDelivery::Wake,
                vec![AskAnswer::Option(1), AskAnswer::Text("  main ".into())]
            )
            .unwrap(),
            vec![AskAnswer::Option(1), AskAnswer::Text("main".into())]
        );
        for answers in [
            vec![AskAnswer::Option(2), AskAnswer::Text("main".into())],
            vec![AskAnswer::Option(0), AskAnswer::Option(0)],
            vec![AskAnswer::Option(0), AskAnswer::Text("  ".into())],
            vec![AskAnswer::Option(0)],
        ] {
            assert!(
                validate_answers(1, &questions, AskDelivery::Wake, answers.clone()).is_err(),
                "{answers:?}"
            );
        }
        let hold = vec![q("Run `cargo test`?", &["Allow", "Deny"])];
        assert_eq!(
            validate_answers(1, &hold, AskDelivery::Hold, vec![AskAnswer::Option(0)]).unwrap(),
            vec![AskAnswer::Option(0)]
        );
        assert!(matches!(
            validate_answers(
                1,
                &hold,
                AskDelivery::Hold,
                vec![AskAnswer::Text("Allow".into())]
            ),
            Err(CalmError::BadRequest(_))
        ));
    }

    #[test]
    fn questions_are_trimmed_and_bounded() {
        assert_eq!(
            validate_questions(vec![q("  Merge?  ", &[" Merge ", "Hold"])]).unwrap(),
            vec![q("Merge?", &["Merge", "Hold"])]
        );
        assert!(validate_questions(Vec::new()).is_err());
        assert!(validate_questions(vec![q("x", &[]); MAX_QUESTIONS + 1]).is_err());
        assert!(validate_questions(vec![q("   ", &[])]).is_err());
        assert!(validate_questions(vec![q("x", &["ok", " "])]).is_err());
        assert!(validate_questions(vec![q("x", &["o"; MAX_OPTIONS + 1])]).is_err());
        let at_limit = "é".repeat(MAX_TEXT_CHARS);
        assert!(
            validate_questions(vec![q(&at_limit, &[])]).is_ok(),
            "the limit counts characters, not bytes"
        );
        assert!(validate_questions(vec![q(&"x".repeat(MAX_TEXT_CHARS + 1), &[])]).is_err());
        assert!(validate_questions(vec![q("x", &[&"o".repeat(MAX_OPTION_CHARS + 1)])]).is_err());
    }

    #[test]
    fn a_clipped_title_always_fits_the_question_bound() {
        assert_eq!(clip_title("  run ls  "), "run ls");
        let at_limit = "é".repeat(MAX_TEXT_CHARS);
        assert_eq!(clip_title(&at_limit), at_limit);
        let long = format!("{} tail", "x".repeat(MAX_TEXT_CHARS));
        let clipped = clip_title(&long);
        assert_eq!(clipped.chars().count(), MAX_TEXT_CHARS);
        assert!(clipped.ends_with('…'));
        assert!(validate_questions(vec![q(&clipped, &["Allow"])]).is_ok());
    }
}
