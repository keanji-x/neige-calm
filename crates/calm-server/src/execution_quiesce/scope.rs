//! Card-owned execution inventory shared by Card, Track and Area deletion.
use super::*;

/// The caller holds the Area and every member Track admission fence.
pub(crate) async fn dispose_tracks(
    repo: &dyn RouteRepo,
    runtime: &crate::operation::OperationRuntime,
    tracks: &[crate::model::Track],
) -> Result<()> {
    let mut cards = Vec::new();
    for track in tracks {
        cards.extend(
            repo.cards_by_track(track.id.as_str())
                .await?
                .into_iter()
                .map(|card| card.id.to_string()),
        );
    }
    dispose_cards(repo, runtime, cards).await
}

/// Inventory all cards before stopping any owner; the caller holds every scope fence.
pub(crate) async fn dispose_cards(
    repo: &dyn RouteRepo,
    runtime: &crate::operation::OperationRuntime,
    cards: Vec<String>,
) -> Result<()> {
    let ids = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            use calm_truth::db::sqlite::workspace_execution as sql;
            let mut ids = Vec::new();
            for card in cards {
                let terminal = crate::db::sqlite::terminal_get_by_card_tx(tx, &card).await?;
                let alias = terminal.map(|t| sql::terminal_alias(&t.id));
                for execution in sql::card_executions(tx, &card).await? {
                    let metadata: workspace_runtime::execution::ExecutionMetadata =
                        serde_json::from_str(&sql::intent_tx(tx, execution.id.as_str()).await?)
                            .map_err(|e| {
                                CalmError::Conflict(format!(
                                    "unreadable card execution; resources retained: {e}"
                                ))
                            })?;
                    if alias.as_deref() != Some(metadata.alias.0.as_str()) {
                        ids.push(execution.id);
                    }
                }
            }
            Ok(ids)
        })
    })
    .await?;
    if ids.is_empty() {
        return Ok(());
    }
    let engine = runtime.execution().engine().ok_or_else(|| {
        CalmError::Conflict("owned execution disposal requires Engine; resources retained".into())
    })?;
    owned::dispose(engine.store(), runtime, &ids).await
}
