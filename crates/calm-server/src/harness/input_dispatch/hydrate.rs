//! Hydrated input is transient. Only canonical revisions and the frozen plan
//! are read; no native echo, request envelope, or runtime cache can supply text.
use calm_types::conversation::{
    MessageAuthor, PrivateHistoryPayload, SubmissionRenderInput, SubmissionRenderPlan,
    SubmissionRenderSegment,
};
use sqlx::{Sqlite, Transaction};

use crate::db::sqlite::conversation::{message_tx, revision_tx};
use crate::error::{CalmError, Result};
use provider::InputItem;

pub(super) async fn hydrate(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    plan: &SubmissionRenderPlan,
) -> Result<Vec<InputItem>> {
    plan.input_refs()
        .map_err(|error| CalmError::Conflict(error.into()))?;
    let mut text = Vec::with_capacity(plan.segments().len());
    let mut images = Vec::new();
    for segment in plan.segments() {
        match segment {
            SubmissionRenderSegment::Input { input, .. } => match input {
                SubmissionRenderInput::Human { message, author } => {
                    let identity = message_tx(tx, &message.message_id).await?;
                    if identity.card_id != card_id
                        || identity.withdrawn_at_ms.is_some()
                        || identity.author
                            != (MessageAuthor::AuthenticatedHuman {
                                registered_user_id: author.registered_user_id.clone(),
                            })
                    {
                        return Err(CalmError::Conflict(
                            "submitted human reference and original author disagree".into(),
                        ));
                    }
                    let PrivateHistoryPayload::Present { value } =
                        revision_tx(tx, message).await?.payload
                    else {
                        return Err(erased());
                    };
                    // Quoting makes embedded newlines in a display label data,
                    // rather than a second author boundary in the rendered input.
                    let name = serde_json::to_string(&author.display_name)?;
                    text.push(format!("[Human {name}]\n{}", value.text));
                    images.extend(
                        value
                            .bound_attachments
                            .into_iter()
                            .map(|attachment| InputItem::local_image(attachment.path)),
                    );
                }
                SubmissionRenderInput::System {
                    payload: PrivateHistoryPayload::Present { value },
                    ..
                } => text.push(value.observation().to_turn_text()),
                SubmissionRenderInput::System { .. } => return Err(erased()),
            },
            SubmissionRenderSegment::Context {
                payload: PrivateHistoryPayload::Present { value },
            } => text.push(value.text().into()),
            SubmissionRenderSegment::Context { .. } => return Err(erased()),
        }
    }
    let mut items = vec![InputItem::text(text.join("\n"))];
    items.extend(images);
    Ok(items)
}

fn erased() -> CalmError {
    CalmError::Conflict("an erased input or context cannot be dispatched".into())
}
