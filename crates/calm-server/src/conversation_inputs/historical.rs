//! Transient admission for public operations on the released historical contract.
//! It is never serialized or assigned to old queue entries. Once accepted, V1
//! work retains its original operator/kernel execution semantics.
//!
//! Exit: stop selecting new historical work once canonical creation supports
//! the released templates, recipes, forks, attached workspaces and providers,
//! and its operations cover ordinary sends, mutations, attachments and internal
//! feature input. Existing V1 rows/keys still need lifetime servicing or an
//! explicit lossless migration preserving original attribution and receipts.
use super::*;
use crate::db::{RepoEventWrite, write_in_tx_typed};

#[derive(Clone, Debug)]
pub(crate) struct HistoricalHumanSource(HumanInputSource);

impl HistoricalHumanSource {
    pub(crate) fn check_current(&self) -> Result<()> {
        self.0.current_principal().map(drop)
    }

    pub(crate) fn new(source: HumanInputSource) -> Self {
        Self(source)
    }

    pub(crate) async fn require_area_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        area_id: &str,
    ) -> Result<()> {
        super::execution::authorize_legacy_human_area_tx(tx, &self.0, area_id).await
    }

    pub(crate) async fn require_card_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        card_id: &str,
    ) -> Result<()> {
        self.0.current_principal()?;
        super::require_historical_storage_tx(tx, card_id).await?;
        let area = conversation::conversation_area_tx(tx, card_id).await?;
        self.require_area_tx(tx, area.id.as_str()).await
    }

    pub(crate) async fn require_track_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        track_id: &str,
    ) -> Result<()> {
        self.0.current_principal()?;
        let track =
            crate::db::sqlite::track_get_tx(tx, &crate::ids::TrackId::from(track_id)).await?;
        self.require_area_tx(tx, track.area_id.as_str()).await
    }

    pub(crate) async fn require_area(&self, repo: &dyn RepoEventWrite, id: &str) -> Result<()> {
        let this = self.clone();
        let id = id.to_owned();
        write_in_tx_typed(repo, move |tx| {
            Box::pin(async move { this.require_area_tx(tx, &id).await })
        })
        .await
    }

    pub(crate) async fn require_card(&self, repo: &dyn RepoEventWrite, id: &str) -> Result<()> {
        let this = self.clone();
        let id = id.to_owned();
        write_in_tx_typed(repo, move |tx| {
            Box::pin(async move { this.require_card_tx(tx, &id).await })
        })
        .await
    }

    pub(crate) async fn require_track(&self, repo: &dyn RepoEventWrite, id: &str) -> Result<()> {
        let this = self.clone();
        let id = id.to_owned();
        write_in_tx_typed(repo, move |tx| {
            Box::pin(async move { this.require_track_tx(tx, &id).await })
        })
        .await
    }
}
