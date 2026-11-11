//! Current deployment approval qualifies only the original LinkGuest references.
//! Revocation is an ingress fence; accepted work revalidates ownership and scope.
use super::*;
use crate::auth::LegacyPrivateExecution;
use calm_types::conversation::{MessageAuthor, PrivateHistoryPayload};
use calm_types::share_links::ShareDelegation;

#[derive(Clone)]
pub(crate) struct ShareInputSource {
    execution: LegacyPrivateExecution,
    delegation: ShareDelegation,
}
impl ShareInputSource {
    pub(crate) fn new(execution: LegacyPrivateExecution, delegation: ShareDelegation) -> Self {
        Self {
            execution,
            delegation,
        }
    }
    pub(crate) fn check_current(&self) -> Result<()> {
        if self.execution.current_owner()? != self.delegation.approver_user_id {
            return Err(CalmError::Forbidden("share issuer changed".into()));
        }
        Ok(())
    }
    pub(crate) fn delegation(&self) -> &ShareDelegation {
        &self.delegation
    }

    pub(crate) async fn authorize_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        card: &str,
    ) -> Result<String> {
        crate::share_access::authorize_accepted_tx(tx, &self.execution, &self.delegation, card)
            .await
    }

    pub(crate) async fn matches_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        carrier: &CurrentInputCarrier,
        message: &MessageRevisionRef,
    ) -> Result<bool> {
        let stored = conversation::message_tx(tx, &message.message_id).await?;
        Ok(stored.card_id == carrier.card_id()
            && stored.current == *message
            && stored.withdrawn_at_ms.is_none()
            && stored.author
                == (MessageAuthor::LinkGuest {
                    share_link_id: self.delegation.id.clone(),
                })
            && matches!(
                conversation::revision_tx(tx, message).await?.payload,
                PrivateHistoryPayload::Present { .. }
            ))
    }
}
