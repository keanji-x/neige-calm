//! The legacy lifecycle consumes one persistent contract fact. Actor, liveness
//! and an absent current pointer cannot downgrade an accepted conversation.
use crate::db::{RepoEventWrite, write_in_tx_typed};
use crate::error::{CalmError, Result};
use sqlx::SqliteConnection;

pub(crate) async fn require_historical_storage_tx(
    connection: &mut SqliteConnection,
    card_id: &str,
) -> Result<()> {
    if crate::db::sqlite::conversation::card_has_canonical_storage(connection, card_id).await? {
        return Err(CalmError::Conflict(
            "canonical conversation requires its retained current-carrier lifecycle".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn has_canonical_storage(
    repo: &dyn RepoEventWrite,
    card_id: &str,
) -> Result<bool> {
    let card_id = card_id.to_owned();
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            Ok(crate::db::sqlite::conversation::card_has_canonical_storage(tx, &card_id).await?)
        })
    })
    .await
}

pub(crate) async fn require_historical_storage(
    repo: &dyn RepoEventWrite,
    card_id: &str,
) -> Result<()> {
    if has_canonical_storage(repo, card_id).await? {
        return Err(CalmError::Conflict(
            "canonical conversation requires its retained current-carrier lifecycle".into(),
        ));
    }
    Ok(())
}
