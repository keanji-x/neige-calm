//! The existing historical start owner can retain one current HTTP source.
//! Persisted operations retain their released format; no Actor becomes a source.
use super::*;
use crate::conversation_inputs::HistoricalHumanSource;

impl PlannerHarnessStartAdapter {
    pub(crate) fn historical_human_invocation(&self, source: HistoricalHumanSource) -> Self {
        let mut invocation = self.clone();
        invocation.invocation = StartInvocation::HistoricalHuman(source);
        invocation
    }

    pub(super) fn historical_human_source(&self) -> Option<&HistoricalHumanSource> {
        match &self.invocation {
            StartInvocation::HistoricalHuman(source) => Some(source),
            _ => None,
        }
    }

    pub(super) fn check_historical_issuer(&self) -> Result<()> {
        match self.historical_human_source() {
            Some(source) => source.check_current(),
            None => Ok(()), // Registered historical kernel invocation.
        }
    }

    pub(super) async fn admit_historical(&self, card_id: &str) -> Result<()> {
        if let Some(source) = self.historical_human_source() {
            source.require_card(self.repo.as_ref(), card_id).await
        } else {
            crate::conversation_inputs::require_historical_storage(self.repo.as_ref(), card_id)
                .await
        }
    }

    pub(super) async fn admit_historical_tx(&self, tx: &mut Tx<'_>, card_id: &str) -> Result<()> {
        if let Some(source) = self.historical_human_source() {
            source.require_card_tx(tx, card_id).await
        } else {
            crate::conversation_inputs::require_historical_storage_tx(tx, card_id).await
        }
    }
}
