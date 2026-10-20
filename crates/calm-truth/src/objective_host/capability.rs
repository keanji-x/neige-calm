//! The single capability precondition for every objective ingress.
use super::*;
use calm_types::track_capability::TrackCapability;

pub async fn enabled_tx(tx: &mut Transaction<'_, Sqlite>, track: &TrackId) -> Result<bool> {
    crate::db::sqlite::track_capabilities::has_tx(tx, track, TrackCapability::Objectives).await
}

pub async fn require_tx(tx: &mut Transaction<'_, Sqlite>, track: &TrackId) -> Result<()> {
    if !enabled_tx(tx, track).await? {
        return Err(TruthError::Forbidden(
            "objectives capability is not declared for this track".into(),
        ));
    }
    Ok(())
}
