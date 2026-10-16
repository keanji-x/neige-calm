//! The user's approval of one approval-gated forge action (#2617), through an explicit, tagged ask
//! action. The kernel supplies the choices, and only a click on Approve grants: typed text never
//! does. The grant covers only the call whose forge idempotency key and semantic payload hash the
//! ask carries. The answer has no effect of its own: it wakes the Planner, which repeats the
//! identical call, and the forge submit reads the answer here.
use super::StoredAsk;
use crate::error::{CalmError, Result};
use crate::event::{AskAction, AskAnswer, AskDelivery, AskQuestion, Event, EventScope};
use crate::ids::{CardId, TrackId};
use crate::model::CardRole;
use sqlx::{Row, Sqlite, Transaction};

const OPTIONS: [&str; 2] = ["Approve", "Decline"];

/// What the user said about one approval-gated forge call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeApproval {
    /// A click on Approve: the call may run.
    Approved,
    /// The ask still waits for the user's answer.
    Awaiting { ask_id: i64 },
    /// Decline, or typed text: the call never runs. A new request needs a new idempotency key.
    Declined { ask_id: i64 },
}

/// The approval read in `tx`, or the `ask.requested` to write when the call has none yet.
#[derive(Debug)]
pub enum ForgeApprovalRead {
    Recorded(ForgeApproval),
    /// No ask carries this call yet: write this event, as the Planner card's own, in the
    /// caller's batch; its event id is the new ask's id.
    Ask(EventScope, Box<Event>),
}

/// The approval of the forge call `(idempotency_key, payload_hash)` that `card` submits on
/// `track`, read in `tx`. Only the track's Planner card submits an approval-gated call. The ask is
/// looked up on this track, raised by this card, for exactly this key and payload hash; while it
/// is open a repeated call gets the same ask back. `title` is asked only when there is no ask yet.
pub async fn forge_approval_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    card: &CardId,
    idempotency_key: &str,
    payload_hash: &str,
    title: &str,
) -> Result<ForgeApprovalRead> {
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM cards WHERE id = ?1 AND track_id = ?2")
            .bind(card.as_str())
            .bind(track.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    if role.as_deref() != Some(CardRole::Planner.as_db_str()) {
        return Err(CalmError::Forbidden(
            "an approval-gated forge action is submitted only by its track's Planner".into(),
        ));
    }
    let asked = sqlx::query(
        "SELECT r.id, r.payload, \
                (SELECT c.payload FROM events c \
                  WHERE c.scope_track = r.scope_track AND c.kind = 'ask.answered' \
                    AND json_extract(c.payload, '$.ask_id') = r.id) AS answer \
           FROM events r \
          WHERE r.scope_track = ?1 AND r.kind = 'ask.requested' \
            AND json_extract(r.payload, '$.action.kind') = 'forge_action' \
            AND json_extract(r.payload, '$.action.idempotency_key') = ?2 \
            AND json_extract(r.payload, '$.action.payload_hash') = ?3 \
            AND json_extract(r.actor, '$.kind') = 'AiPlanner' \
            AND json_extract(r.actor, '$.id') = ?4 \
          ORDER BY r.id LIMIT 1",
    )
    .bind(track.as_str())
    .bind(idempotency_key)
    .bind(payload_hash)
    .bind(card.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = asked else {
        return super::requested_tx(
            tx,
            card,
            vec![AskQuestion {
                title: title.to_string(),
                options: OPTIONS.iter().map(|value| (*value).into()).collect(),
            }],
            AskDelivery::Wake,
            None,
            Some(AskAction::ForgeAction {
                idempotency_key: idempotency_key.to_string(),
                payload_hash: payload_hash.to_string(),
            }),
        )
        .await
        .map(|(scope, event)| ForgeApprovalRead::Ask(scope, Box::new(event)));
    };
    let ask_id: i64 = row.get("id");
    let Event::AskRequested {
        questions,
        delivery,
        ..
    } = Event::from_kind_and_payload(
        "ask.requested",
        serde_json::from_str(row.get::<&str, _>("payload"))?,
    )?
    else {
        return Err(CalmError::Internal(format!(
            "ask {ask_id}: the stored row is not an ask.requested"
        )));
    };
    if !is_contract(&questions, delivery) {
        return Err(CalmError::Internal(format!(
            "ask {ask_id}: invalid forge approval question contract"
        )));
    }
    let Some(answer) = row.get::<Option<&str>, _>("answer") else {
        return Ok(ForgeApprovalRead::Recorded(ForgeApproval::Awaiting {
            ask_id,
        }));
    };
    let Event::AskAnswered { answers, .. } =
        Event::from_kind_and_payload("ask.answered", serde_json::from_str(answer)?)?
    else {
        return Err(CalmError::Internal(format!(
            "ask {ask_id}: the stored answer is not an ask.answered"
        )));
    };
    Ok(ForgeApprovalRead::Recorded(if grants(&answers) {
        ForgeApproval::Approved
    } else {
        ForgeApproval::Declined { ask_id }
    }))
}

/// Only a click on the first option, Approve, grants.
fn grants(answers: &[AskAnswer]) -> bool {
    matches!(answers, [AskAnswer::Option(0)])
}

fn is_contract(questions: &[AskQuestion], delivery: AskDelivery) -> bool {
    delivery == AskDelivery::Wake && matches!(questions, [question] if question.options == OPTIONS)
}

/// The answer has no effect of its own; a forge approval ask whose question is not the kernel's
/// contract is refused, as a 400 like the other tagged asks.
pub(super) fn answer_events_tx(ask: &StoredAsk) -> Result<Vec<Event>> {
    if !matches!(ask.action, Some(AskAction::ForgeAction { .. })) {
        return Ok(Vec::new());
    }
    if !is_contract(&ask.questions, ask.delivery) {
        return Err(CalmError::BadRequest(
            "invalid forge approval question contract".into(),
        ));
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(options: &[&str]) -> AskQuestion {
        AskQuestion {
            title: "Re-run?".into(),
            options: options.iter().map(|option| (*option).into()).collect(),
        }
    }

    #[test]
    fn forge_approval_grants_only_a_click_on_approve() {
        assert!(grants(&[AskAnswer::Option(0)]));
        for answers in [
            vec![AskAnswer::Option(1)],
            vec![AskAnswer::Text("Approve".into())],
            vec![AskAnswer::Option(0), AskAnswer::Option(0)],
            Vec::new(),
        ] {
            assert!(!grants(&answers), "{answers:?}");
        }
    }

    #[test]
    fn forge_approval_contract_is_one_wake_question_with_the_kernel_options() {
        assert!(is_contract(&[question(&OPTIONS)], AskDelivery::Wake));
        assert!(!is_contract(&[question(&OPTIONS)], AskDelivery::Hold));
        assert!(!is_contract(
            &[question(&["Decline", "Approve"])],
            AskDelivery::Wake
        ));
        assert!(!is_contract(
            &[question(&OPTIONS), question(&OPTIONS)],
            AskDelivery::Wake
        ));
    }

    #[test]
    fn forge_approval_answer_has_no_events_and_refuses_a_foreign_contract() {
        let ask = |questions| StoredAsk {
            action: Some(AskAction::ForgeAction {
                idempotency_key: "k".into(),
                payload_hash: "h".into(),
            }),
            questions,
            delivery: AskDelivery::Wake,
            actor: crate::ids::ActorId::AiPlanner(CardId::from("planner")),
        };
        assert!(
            answer_events_tx(&ask(vec![question(&OPTIONS)]))
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            answer_events_tx(&ask(vec![question(&["Yes", "No"])])),
            Err(CalmError::BadRequest(_))
        ));
    }
}
