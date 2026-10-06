//! One way to ask the user (#2209): the Planner writes `ask.requested` with one or more questions,
//! and only the user writes `ask.answered`, which wakes the Planner. Every writer goes through this
//! module, inside the caller's immediate transaction, so a question is validated and a track is
//! resolved one way.

use sqlx::{Row, Sqlite, Transaction};

use crate::error::{CalmError, Result};
use crate::event::{AskQuestion, Event, EventScope};
use crate::ids::{CardId, TrackId};

/// At most this many questions in one ask.
pub const MAX_QUESTIONS: usize = 8;
/// At most this many options on one question.
pub const MAX_OPTIONS: usize = 8;
/// Upper bound on a title or an answer, in characters (not bytes).
pub const MAX_TEXT_CHARS: usize = 2000;
/// Upper bound on one option, in characters.
pub const MAX_OPTION_CHARS: usize = 200;

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

/// The `ask.requested` a Planner card raises, ready for the caller's event batch under the card's
/// own Planner identity (the role gate refuses any other author). The track is the card's own,
/// read in `tx`; it is never taken from the caller. A closed track is not refused: a translated
/// provider question has nobody to refuse, and the projection keeps it either way.
pub async fn ask_requested_tx(
    tx: &mut Transaction<'_, Sqlite>,
    planner_card: &CardId,
    questions: Vec<AskQuestion>,
    source_item_id: Option<String>,
) -> Result<(EventScope, Event)> {
    let questions = validate_questions(questions)?;
    let track_id: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id = ?1")
        .bind(planner_card.as_str())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {planner_card}")))?;
    let track = crate::db::sqlite::track_get_tx(tx, &TrackId::from(track_id)).await?;
    Ok((
        EventScope::Track {
            track: track.id.clone(),
            area: track.area_id,
        },
        Event::AskRequested {
            track_id: track.id,
            questions,
            source_item_id,
        },
    ))
}

/// The `ask.answered` for `ask_id` on `track`, ready for the caller's event batch as the user's
/// (the role gate refuses any other author): the ask exists on this track, is not answered yet,
/// and gets exactly one non-empty answer per question. Answers are not limited to the options;
/// the Planner reads the answer and decides.
pub async fn ask_answered_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    ask_id: i64,
    answers: Vec<String>,
) -> Result<(EventScope, Event)> {
    let track_row = crate::db::sqlite::track_get_tx(tx, track).await?;
    let row = sqlx::query(
        "SELECT scope_track, payload FROM events WHERE id = ?1 AND kind = 'ask.requested'",
    )
    .bind(ask_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row.filter(|row| {
        row.get::<Option<String>, _>("scope_track").as_deref() == Some(track.as_str())
    }) else {
        return Err(CalmError::NotFound(format!(
            "ask {ask_id} on track {track}"
        )));
    };
    let payload: serde_json::Value = serde_json::from_str(row.get::<&str, _>("payload"))?;
    let Event::AskRequested { questions, .. } =
        Event::from_kind_and_payload("ask.requested", payload)?
    else {
        return Err(CalmError::Internal(format!(
            "ask {ask_id}: the stored row is not an ask.requested"
        )));
    };
    let answered: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM events WHERE scope_track = ?1 AND kind = 'ask.answered' \
            AND json_extract(payload, '$.ask_id') = ?2)",
    )
    .bind(track.as_str())
    .bind(ask_id)
    .fetch_one(&mut **tx)
    .await?;
    if answered {
        return Err(CalmError::Conflict(format!(
            "ask {ask_id} is already answered"
        )));
    }
    if answers.len() != questions.len() {
        return Err(CalmError::BadRequest(format!(
            "answers: ask {ask_id} has {} questions, got {} answers",
            questions.len(),
            answers.len()
        )));
    }
    let answers = answers
        .iter()
        .enumerate()
        .map(|(i, answer)| bounded(&format!("answers[{i}]"), answer, MAX_TEXT_CHARS))
        .collect::<Result<Vec<_>>>()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn q(title: &str, options: &[&str]) -> AskQuestion {
        AskQuestion {
            title: title.into(),
            options: options.iter().map(|o| (*o).to_string()).collect(),
        }
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
}
