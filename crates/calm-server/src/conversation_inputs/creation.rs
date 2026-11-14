//! First-input creation uses the same allocation owner for Human and LinkGuest.
//! The request credential remains transient; receipts bind its author identity.
use super::*;
use crate::share_access::scope::ShareAccessScope;
use calm_types::conversation::MessageAuthor;

#[derive(Clone)]
pub(crate) struct CreationInputSource {
    human: HumanInputSource,
    share: Option<ShareAccessScope>,
}
impl From<HumanInputSource> for CreationInputSource {
    fn from(human: HumanInputSource) -> Self {
        Self { human, share: None }
    }
}
impl CreationInputSource {
    pub(crate) fn new(human: HumanInputSource, share: Option<ShareAccessScope>) -> Self {
        Self { human, share }
    }
    fn check_current(&self) -> Result<()> {
        if let Some(scope) = &self.share {
            scope.check_current()?;
        }
        self.human.current_principal().map(drop)
    }
    pub(crate) fn require_historical(&self) -> Result<()> {
        if self.share.is_some() {
            return Err(CalmError::IdempotencyKeyReused(
                "key names historical Human creation".into(),
            ));
        }
        self.human.current_human_principal()?;
        Ok(())
    }
    pub(crate) fn human(&self) -> &HumanInputSource {
        &self.human
    }
    pub(crate) fn source_id(&self) -> Result<String> {
        Ok(self.human.current_principal()?.user_id)
    }
    pub(crate) async fn require_author_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        message: &str,
    ) -> Result<()> {
        let id = calm_types::conversation::ConversationMessageId::parse(message)
            .map_err(|e| CalmError::Internal(e.to_string()))?;
        let stored = conversation::message_tx(tx, &id).await?;
        let expected = match &self.share {
            Some(scope) => MessageAuthor::LinkGuest {
                share_link_id: scope.read.link.delegation.id.clone(),
            },
            None => MessageAuthor::AuthenticatedHuman {
                registered_user_id: self.human.current_human_principal()?.user_id,
            },
        };
        if stored.author != expected {
            return Err(CalmError::IdempotencyKeyReused(
                "key names another input author".into(),
            ));
        }
        self.check_current()
    }

    pub(crate) async fn authorize_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        area: &str,
        track: Option<&str>,
    ) -> Result<crate::model::Area> {
        if let Some(scope) = &self.share {
            scope.authorize_ingress_tx(tx, area, track).await?;
        } else {
            self.human.current_human_principal()?;
        }
        let area = crate::organization_access::require_area_access_tx(
            tx,
            area,
            &self.human.current_principal()?,
            AreaAccess::Collaborate,
        )
        .await?;
        self.check_current()?;
        Ok(area)
    }
    pub(crate) async fn accept_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        carrier: &mut CurrentInputCarrier,
        payload: HumanPayload,
    ) -> Result<AcceptedPendingInput> {
        if let Some(scope) = &self.share {
            let source = scope.input_source()?;
            source.authorize_tx(tx, carrier.card_id()).await?;
            Ok(conversation::accept_pending_message_tx(
                tx,
                carrier,
                &MessageAuthor::LinkGuest {
                    share_link_id: scope.read.link.delegation.id.clone(),
                },
                payload,
            )
            .await?)
        } else {
            super::accept_human_tx(tx, &self.human.current_human_principal()?, carrier, payload)
                .await
        }
    }
    pub(crate) async fn wake(
        &self,
        state: crate::state::RouteState,
        card: crate::ids::CardId,
    ) -> Result<()> {
        let source = match &self.share {
            Some(scope) => InputSource::Share(scope.input_source()?),
            None => InputSource::from(self.human.clone()),
        };
        crate::operation::planner_start_fence::wake_canonical_input(state, card, source).await;
        Ok(())
    }
}
