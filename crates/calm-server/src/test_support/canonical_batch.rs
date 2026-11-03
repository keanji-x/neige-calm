#![cfg(test)]
//! Observe canonical plans and native requests without constructing either.
use crate::db::sqlite::{SqlxRepo, conversation};
use calm_types::conversation::{
    AcceptedInputRef, ConversationMessageId, InputSubmissionId, MessageAuthor, MessageRevisionRef,
    PrivateHistoryPayload, SubmissionRenderInput, SubmissionRenderPlan, SubmissionRenderSegment,
};
use serde_json::Value;

pub(crate) fn human_ref(entry_id: &str) -> AcceptedInputRef {
    AcceptedInputRef::Human {
        message: MessageRevisionRef {
            message_id: ConversationMessageId::parse(entry_id).unwrap(),
            revision: std::num::NonZeroU32::MIN,
        },
    }
}

#[cfg(test)]
pub(crate) async fn assert_submission(
    repo: &SqlxRepo,
    card: &str,
    submission: &InputSubmissionId,
    expected: &[(AcceptedInputRef, &str)],
    author: (&str, &str),
) -> SubmissionRenderPlan {
    let mut tx = repo.pool().begin().await.unwrap();
    // The owner also checks the ordered persisted submission-input relations.
    let plan = conversation::submission_plan_tx(&mut tx, submission)
        .await
        .unwrap()
        .plan;
    assert_eq!(
        plan.input_refs().unwrap(),
        expected
            .iter()
            .map(|(input, _)| input.clone())
            .collect::<Vec<_>>()
    );
    let humans: Vec<_> = plan
        .segments()
        .iter()
        .filter_map(|segment| match segment {
            SubmissionRenderSegment::Input {
                input: SubmissionRenderInput::Human { message, author },
                ..
            } => Some((message, author)),
            SubmissionRenderSegment::Input { .. } => panic!("expected only accepted Human inputs"),
            SubmissionRenderSegment::Context { .. } => None,
        })
        .collect();
    assert_eq!(humans.len(), expected.len());
    for ((message, frozen), (_, body)) in humans.into_iter().zip(expected) {
        assert_eq!(frozen.registered_user_id, author.0);
        assert_eq!(frozen.display_name, author.1);
        let identity = conversation::message_tx(&mut tx, &message.message_id)
            .await
            .unwrap();
        assert_eq!(identity.card_id, card);
        assert_eq!(
            identity.author,
            MessageAuthor::AuthenticatedHuman {
                registered_user_id: author.0.into()
            }
        );
        let PrivateHistoryPayload::Present { value } = conversation::revision_tx(&mut tx, message)
            .await
            .unwrap()
            .payload
        else {
            panic!("accepted revision must retain its body");
        };
        assert_eq!(value.text, *body);
        assert!(value.bound_attachments.is_empty());
    }
    tx.rollback().await.unwrap();
    eprintln!("observed stored submission {submission:?}: {plan:?}");
    plan
}

/// Read only the real wire text blocks (Codex input, ACP prompt or Claude content).
pub(crate) fn native_text(blocks: &Value) -> String {
    blocks
        .as_array()
        .unwrap()
        .iter()
        .map(|block| {
            assert_eq!(block["type"], "text");
            block["text"].as_str().unwrap()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn assert_native_text(text: &str, bodies: &[&str], display_name: &str) {
    let mut previous = None;
    for body in bodies {
        assert_eq!(text.matches(body).count(), 1, "{text}");
        let position = text.find(body).unwrap();
        if let Some(previous) = previous {
            assert!(previous < position, "{text}");
        }
        previous = Some(position);
    }
    assert_eq!(
        text.matches(&format!(
            "[Human {}]",
            serde_json::to_string(display_name).unwrap()
        ))
        .count(),
        bodies.len(),
        "{text}"
    );
    assert_eq!(text.matches("[Human ").count(), bodies.len(), "{text}");
    eprintln!("observed native text: {text:?}");
}

/// The existing route fixture's transcript query, shared with retained activation.
pub(crate) async fn transcript_count(
    repo: &SqlxRepo,
    card: &str,
    worker: Option<&str>,
    method: Option<&str>,
    item: Option<&str>,
) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM harness_items WHERE card_id=?1
         AND (?2 IS NULL OR worker_session_id=?2)
         AND (?3 IS NULL OR method=?3) AND (?4 IS NULL OR item_uuid=?4)",
    )
    .bind(card)
    .bind(worker)
    .bind(method)
    .bind(item)
    .fetch_one(repo.pool())
    .await
    .unwrap()
}
