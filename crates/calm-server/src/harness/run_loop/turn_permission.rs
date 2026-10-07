//! The permission mode a turn runs under (#2348), read from the card as it stands when the turn
//! is issued, through the one reader of a stored mode.

use super::Inner;
use crate::harness::issuance::IssuanceRefusal;
use crate::planner_permission_mode::{self, PlannerPermissionMode};

/// A Planner conversation runs under its card's stored mode; every other conversation (PlainChat,
/// Assistant) never asks. A Planner whose stored mode cannot be read is not issued, as one whose
/// model selection cannot be read is not: neither `ask` nor `never` is assumed for it.
pub(super) async fn resolve(inner: &Inner) -> Result<PlannerPermissionMode, IssuanceRefusal> {
    let card_id = inner.card_id.as_str();
    let read = async {
        Ok::<_, crate::error::CalmError>((
            inner.repo.card_get(card_id).await?,
            inner.repo.card_role_get(card_id).await?,
        ))
    }
    .await;
    let (card, role) = match read {
        Err(e) => {
            return Err(IssuanceRefusal::retryable(format!(
                "could not re-read the card to resolve its permission mode: {e}"
            )));
        }
        Ok((None, _)) => {
            return Err(IssuanceRefusal::retryable(
                "the card this conversation belongs to no longer exists".into(),
            ));
        }
        Ok((Some(card), role)) => (card, role),
    };
    let has_mode =
        role.is_some_and(|role| planner_permission_mode::card_has_permission_mode(&card, role));
    if !has_mode {
        return Ok(PlannerPermissionMode::Never);
    }
    planner_permission_mode::read(&card.payload).map_err(|malformed| {
        IssuanceRefusal::needs_a_choice(
            malformed.to_string(),
            "This conversation's saved permission mode cannot be read. Choose a permission mode \
             to replace it. Your message is still queued."
                .into(),
        )
    })
}
