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
    if !wakes_on_failure(
        payload
            .event_spec
            .as_ref()
            .map(|descriptor| descriptor.event_kind.as_str()),
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

fn wakes_on_failure(kind: Option<&str>) -> bool {
    kind.is_some_and(calm_types::forge_action::notifies_planner)
}

#[test]
fn failure_wake_covers_every_forge_success_wake_kind() {
    let data = json!({
        "track_id":"track", "card_id":"card", "subject":{"pr_number":42},
        "overlapping_prs":[], "pr_number":42, "head_sha":"head", "merge_sha":"merge",
        "base_sha":"base", "artifact_path":"/tmp/evidence", "conclusion":"success",
        "issue_number":42, "issue_url":"https://example.invalid/42", "path":"/tmp/worktree",
        "commit_sha":"commit", "branch":"topic"
    });
    for kind in SUPPORTED_FORGE_EVENT_KINDS {
        let event = Event::from_kind_and_payload(kind, data.clone()).unwrap();
        assert_eq!(
            wakes_on_failure(Some(kind)),
            crate::dispatcher::event_warrants_planner_push_with_role(
                &event,
                &ActorId::KernelDispatcher,
                |_| None
            ),
            "Forge failure and success notification policy drifted for {kind}"
        );
    }
    assert!(!wakes_on_failure(None));
}
