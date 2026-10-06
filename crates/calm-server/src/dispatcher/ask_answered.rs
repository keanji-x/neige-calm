//! Planner wake for `ask.answered` (#2209). The answer carries only the ask's id; the question
//! titles come from the persisted `ask.requested`, so live push and boot catch-up render the same
//! text. An ask that is not on this track maps to no observation.
use calm_types::observation::AnsweredQuestion;

use crate::db::RepoEventWrite;
use crate::error::Result;
use crate::event::Event;
use crate::harness::Observation;
use crate::ids::TrackId;

pub(crate) async fn observation(
    repo: &dyn RepoEventWrite,
    track_id: &TrackId,
    ask_id: i64,
    answers: &[String],
) -> Result<Option<Observation>> {
    let asked = repo
        .events_for_track(track_id.as_str(), &["ask.requested"], Some(ask_id - 1))
        .await?
        .into_iter()
        .find(|row| row.id == ask_id);
    let Some(Event::AskRequested { questions, .. }) = asked.map(|row| row.event) else {
        tracing::warn!(%track_id, ask_id, "ask.answered names no ask.requested on its track");
        return Ok(None);
    };
    Ok(Some(Observation::AskAnswered {
        track_id: track_id.clone(),
        answers: questions
            .into_iter()
            .zip(answers)
            .map(|(question, answer)| AnsweredQuestion {
                title: question.title,
                answer: answer.clone(),
            })
            .collect(),
    }))
}
