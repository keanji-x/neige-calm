//! Verify the worker's checkout before launching Claude (#1830 S2 D3): the track's checkout its
//! prepare froze, still at that base on that branch. Nothing is created.
use super::*;
use crate::operation::workspace_lease::worker::verify_worker_checkout;

pub(super) async fn verify(
    adapter: &ClaudeWorkerAdapter,
    ctx: &SpawnCtx,
    output: &TxOutput,
) -> Result<()> {
    verify_worker_checkout(output, "claude-worker")?;
    let path = output.output_string("cwd", "claude-worker")?;
    let card_id = output.output_string("card_id", "claude-worker")?;
    let track_id = output.output_string("track_id", "claude-worker")?;
    let scope = card_scope(
        ctx.repo.as_ref(),
        CardId::from(card_id.clone()),
        TrackId::from(track_id.clone()),
    )
    .await?;
    let write = WriteContext::new(
        adapter.card_role_cache.clone(),
        adapter.track_area_cache.clone(),
    );
    let recorded_result = write_with_events_typed(
        ctx.repo.as_ref(),
        ActorId::KernelDispatcher,
        None,
        &ctx.events,
        &write,
        move |tx| {
            Box::pin(async move {
                // Retries may verify the same checkout more than once. The
                // ready event, unlike the check, is committed once.
                let recorded: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM events WHERE kind = 'worktree.provisioned' \
                 AND json_extract(payload, '$.card_id') = ?1)",
                )
                .bind(&card_id)
                .fetch_one(&mut **tx)
                .await?;
                let events = if recorded {
                    return Err(CalmError::Conflict(
                        "claude workspace already recorded".into(),
                    ));
                } else {
                    vec![(
                        scope,
                        Event::WorktreeProvisioned {
                            track_id: TrackId::from(track_id),
                            card_id: CardId::from(card_id),
                            path,
                        },
                    )]
                };
                Ok(((), events))
            })
        },
    )
    .await;
    match recorded_result {
        Ok(_) => Ok(()),
        Err(CalmError::Conflict(reason)) if reason == "claude workspace already recorded" => Ok(()),
        Err(error) => Err(error),
    }
}
