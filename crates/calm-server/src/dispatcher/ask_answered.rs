//! Planner wake for `ask.answered` (#2209). The answer carries only the ask's id; the question
//! titles come from the persisted `ask.requested`, so live push and boot catch-up render the same
//! text. A clicked option is rendered as its label. Only a `wake` ask's answer wakes the Planner
//! (#2348): a `hold` ask's answer went to the paused provider request, and the turn it paused
//! goes on. An ask that is not on this track maps to no observation.
use calm_types::observation::AnsweredQuestion;

use crate::db::RepoEventWrite;
use crate::error::Result;
use crate::event::{AskAnswer, AskDelivery, Event};
use crate::harness::Observation;
use crate::ids::TrackId;

pub(crate) async fn observation(
    repo: &dyn RepoEventWrite,
    track_id: &TrackId,
    ask_id: i64,
    answers: &[AskAnswer],
) -> Result<Option<Observation>> {
    let asked = repo
        .events_for_track(track_id.as_str(), &["ask.requested"], Some(ask_id - 1))
        .await?
        .into_iter()
        .find(|row| row.id == ask_id);
    let Some(Event::AskRequested {
        questions,
        delivery,
        ..
    }) = asked.map(|row| row.event)
    else {
        tracing::warn!(%track_id, ask_id, "ask.answered names no ask.requested on its track");
        return Ok(None);
    };
    if delivery == AskDelivery::Hold {
        return Ok(None);
    }
    let mut answered = Vec::with_capacity(answers.len());
    for (question, answer) in questions.into_iter().zip(answers) {
        let answer = match answer {
            AskAnswer::Option(index) => match question.options.get(*index) {
                Some(label) => label.clone(),
                None => {
                    // The answer transaction admits only an option the question has.
                    tracing::warn!(%track_id, ask_id, index, "ask.answered names an option its question does not have");
                    return Ok(None);
                }
            },
            AskAnswer::Text(text) => text.clone(),
        };
        answered.push(AnsweredQuestion {
            title: question.title,
            answer,
        });
    }
    Ok(Some(Observation::AskAnswered {
        track_id: track_id.clone(),
        answers: answered,
    }))
}
