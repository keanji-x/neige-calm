//! The kernel's approval gate for an approval-gated forge action (#2617). A plugin marks a call
//! with `approval`; the forge submit then runs it only after the user clicked Approve on the ask
//! bound to its exact idempotency key and semantic payload hash. The plugin renders the title, the
//! kernel owns the choices, the lookup and who may ask.
use crate::ask::{ForgeApproval, ForgeApprovalRead, forge_approval_tx};
use crate::db::{RepoEventWrite, write_in_tx_typed, write_with_actor_events_typed};
use crate::error::CalmError;
use crate::event::EventBus;
use crate::ids::{ActorId, CardId, TrackId};
use crate::mcp_server::framing::{RpcError, calm_error};
use crate::mcp_server::registry::AppContext;
use crate::mcp_server::transport::{ForgeSubmit, PluginForgePayload};
use crate::operation::OperationRuntime;
use crate::operation::forge_action_adapter::FORGE_ACTION_KIND;
use crate::state::WriteContext;

/// What a forge submit needs to raise and read approval asks. A submitter without one, such as
/// the kernel's own delivery, cannot submit an approval-gated payload.
pub(crate) struct ApprovalAsks<'a> {
    repo: &'a dyn RepoEventWrite,
    events: &'a EventBus,
    write: &'a WriteContext,
}

impl<'a> ApprovalAsks<'a> {
    pub(crate) fn of(ctx: &'a AppContext) -> Self {
        Self {
            repo: ctx.repo.as_ref(),
            events: &ctx.events,
            write: &ctx.write,
        }
    }
}

/// The gate on the one forge submit entry: `Some` when the call is approval-gated and must not run
/// now. An operation already holding the key is a replay and runs nothing new; otherwise a gated
/// call runs only with the user's recorded Approve for this exact key and payload hash.
pub(super) async fn hold(
    runtime: &OperationRuntime,
    approval_asks: Option<&ApprovalAsks<'_>>,
    payload: &PluginForgePayload,
    track: &str,
    card: &str,
    idempotency_key: &str,
    payload_hash: &str,
) -> Result<Option<ForgeSubmit>, RpcError> {
    let Some(request) = payload.approval.as_ref() else {
        return Ok(None);
    };
    if runtime
        .find_by_kind_and_idempotency(FORGE_ACTION_KIND, idempotency_key)
        .await
        .map_err(|e| RpcError::internal(format!("forge approval lookup: {e}")))?
        .is_some()
    {
        return Ok(None);
    }
    let asks = approval_asks.ok_or_else(|| {
        RpcError::invalid_params("this submitter cannot run an approval-gated forge action")
    })?;
    Ok(
        match approval(
            asks,
            track,
            card,
            idempotency_key,
            payload_hash,
            &request.title,
        )
        .await?
        {
            ForgeApproval::Approved => None,
            ForgeApproval::Awaiting { ask_id } => Some(ForgeSubmit::AwaitingApproval { ask_id }),
            ForgeApproval::Declined { ask_id } => Some(ForgeSubmit::Declined { ask_id }),
        },
    )
}

/// The user's recorded approval of the call `(idempotency_key, payload_hash)` that `card` submits
/// on `track`. With no ask for it yet, the ask is raised now, as the Planner card's own, and the
/// call awaits it.
async fn approval(
    asks: &ApprovalAsks<'_>,
    track: &str,
    card: &str,
    idempotency_key: &str,
    payload_hash: &str,
    title: &str,
) -> Result<ForgeApproval, RpcError> {
    let call = Call {
        track: TrackId::from(track.to_string()),
        card: CardId::from(card.to_string()),
        idempotency_key: idempotency_key.to_string(),
        payload_hash: payload_hash.to_string(),
        title: title.to_string(),
    };
    let recorded = write_in_tx_typed(asks.repo, {
        let call = call.clone();
        move |tx| {
            Box::pin(async move {
                Ok(match call.read_tx(tx).await? {
                    ForgeApprovalRead::Recorded(approval) => Some(approval),
                    ForgeApprovalRead::Ask(..) => None,
                })
            })
        }
    })
    .await
    .map_err(calm_error)?;
    if let Some(approval) = recorded {
        return Ok(approval);
    }
    let actor = ActorId::AiPlanner(call.card.clone());
    let (_, ids) =
        write_with_actor_events_typed::<(), _>(asks.repo, None, asks.events, asks.write, {
            move |tx| {
                Box::pin(async move {
                    match call.read_tx(tx).await? {
                        ForgeApprovalRead::Ask(scope, event) => {
                            Ok(((), vec![(actor, scope, *event)]))
                        }
                        // A concurrent identical call raised it between the read and this write.
                        ForgeApprovalRead::Recorded(_) => Err(CalmError::Conflict(
                            "the approval ask was raised concurrently; call again".into(),
                        )),
                    }
                })
            }
        })
        .await
        .map_err(calm_error)?;
    match ids.as_slice() {
        [ask_id] => Ok(ForgeApproval::Awaiting { ask_id: *ask_id }),
        _ => Err(RpcError::internal(format!(
            "expected one approval ask id, got {ids:?}"
        ))),
    }
}

#[derive(Clone)]
struct Call {
    track: TrackId,
    card: CardId,
    idempotency_key: String,
    payload_hash: String,
    title: String,
}

impl Call {
    async fn read_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> crate::error::Result<ForgeApprovalRead> {
        forge_approval_tx(
            tx,
            &self.track,
            &self.card,
            &self.idempotency_key,
            &self.payload_hash,
            &self.title,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::super::{PluginForgePayload, semantic_payload_hash, validate_plugin_forge_payload};
    use calm_types::forge_action::ForgeApprovalRequest;

    fn payload(approval: Option<&str>) -> PluginForgePayload {
        PluginForgePayload {
            argv: vec!["gh".into(), "pr".into(), "merge".into(), "42".into()],
            idem_key: "gh.pr.merge:owner/repo:42".into(),
            event_spec: None,
            subject: None,
            context: serde_json::Map::new(),
            probe: None,
            parked: false,
            replay_any_version: false,
            approval: approval.map(|title| ForgeApprovalRequest {
                title: title.into(),
            }),
        }
    }

    /// #2652: replaying by key alone would let one approved call stand for another payload.
    #[test]
    fn replay_any_version_refuses_approval_payloads() {
        let mut gated = payload(Some("Merge?"));
        assert!(validate_plugin_forge_payload(&gated).is_ok());
        gated.replay_any_version = true;
        let error = validate_plugin_forge_payload(&gated).unwrap_err();
        assert!(error.message.contains("cannot replay"), "{error:?}");
        let mut read = payload(None);
        read.replay_any_version = true;
        assert!(validate_plugin_forge_payload(&read).is_ok());
    }

    #[test]
    fn forge_approval_title_is_bounded_like_an_ask_title() {
        for title in [
            String::new(),
            " ".into(),
            "x".repeat(crate::ask::MAX_TEXT_CHARS + 1),
        ] {
            assert!(validate_plugin_forge_payload(&payload(Some(&title))).is_err());
        }
        let at_limit = "x".repeat(crate::ask::MAX_TEXT_CHARS);
        assert!(validate_plugin_forge_payload(&payload(Some(&at_limit))).is_ok());
    }

    /// An ungated call hashes exactly as before the field existed; a gated one covers its title.
    #[test]
    fn forge_approval_leaves_ungated_hashes_unchanged() {
        assert_eq!(
            semantic_payload_hash(&payload(None)).unwrap(),
            "626c80f965f6e608d578279848a455fa43d9933f5a39d66640b65f82a815fd50"
        );
        let gated = semantic_payload_hash(&payload(Some("Merge?"))).unwrap();
        assert_ne!(gated, semantic_payload_hash(&payload(None)).unwrap());
        assert_ne!(
            gated,
            semantic_payload_hash(&payload(Some("Merge now?"))).unwrap()
        );
    }
}
