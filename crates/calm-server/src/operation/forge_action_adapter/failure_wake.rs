//! Failure is a result too. Keep Forge's notification policy in its owning adapter.
use super::*;

pub(super) async fn event_tx(
    tx: &mut Tx<'_>,
    op: &Operation,
    reason: &str,
) -> Result<Option<OwnerEvent>> {
    // Preparation can be deferred after the caller receives pending, so tx_output
    // is not required. The accepted payload is the original operation's contract.
    let payload: ForgeActionPayload = serde_json::from_value(op.payload.clone())?;
    if !matches!(
        payload
            .event_spec
            .as_ref()
            .map(|spec| spec.event_kind.as_str()),
        Some(
            "forge.scan.completed"
                | "forge.pr.opened"
                | "forge.pr.checks"
                | "forge.issue.closed"
                | "forge.pr.merged"
        )
    ) {
        // Same closed set as Forge's success wakes. Receipt-only reads/publish
        // and worker worktree events keep their existing caller-owned reporting.
        return Ok(None);
    }
    let area: Option<String> = sqlx::query_scalar("SELECT area_id FROM tracks WHERE id=?1")
        .bind(&payload.track_id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some(area) = area else {
        // Deleting the recipient intentionally ends its notification lifecycle.
        return Ok(None);
    };
    let track = TrackId::from(payload.track_id);
    let reason: String = reason.chars().take(4000).collect();
    Ok(Some(OwnerEvent {
        actor: ActorId::KernelDispatcher,
        scope: EventScope::Track {
            track: track.clone(),
            area: AreaId::from(area),
        },
        event: Event::TrackWakeRequested {
            track_id: track,
            source: FORGE_ACTION_KIND.into(),
            key: op.id.clone(),
            text: format!(
                "Forge operation {} ended unsuccessfully: {reason}. Read its recorded outcome before deciding whether to retry.",
                op.id
            ),
        },
    }))
}
