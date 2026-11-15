//! Serial ACP prompt collection. Role policies own admission, permissions and settlement.
use crate::error::{CalmError, Result};
use provider::{
    acp::{Incoming, protocol, translate::TurnTranslator},
    events::{PlannerEvent, PlannerEventKind},
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::watch;
#[derive(Clone, Copy)]
pub(crate) enum CancelRequest {
    Running,
    Requested,
    Attempted,
}
pub(crate) struct NativeTurnIdentity<'a> {
    pub native: &'a str,
}
fn wire_error(error: provider::acp::Error) -> CalmError {
    CalmError::Conflict(error.to_string())
}
/// Read a single turn's wire events and terminal response. The caller owns
/// cleanup and persistence for its execution mode.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn collect_turn(
    identity: NativeTurnIdentity<'_>,
    emit: &(dyn Fn(PlannerEvent) + Send + Sync),
    cancel_gate: &provider::send_boundary::PreSendGate<'_>,
    incoming: &mut tokio::sync::mpsc::Receiver<Incoming>,
    commands: &super::process::Commands,
    translator: &mut TurnTranslator,
    pending: tokio::sync::oneshot::Receiver<
        std::result::Result<provider::acp::PendingResponse, provider::acp::WriteFailure>,
    >,
    cancelled: &mut watch::Receiver<CancelRequest>,
    approvals: &dyn provider::acp::permission::PermissionHandler,
    retained: &mut Vec<Value>,
) -> Result<Vec<PlannerEvent>> {
    let pending = pending
        .await
        .map_err(|_| CalmError::Conflict("ACP prompt handoff was cancelled".into()))?
        .map_err(|error| wire_error(error.into_error()))?;
    let response = pending.wait(Duration::from_secs(3600));
    tokio::pin!(response);
    let mut stop_at = None;
    let mut incoming_open = true;
    loop {
        tokio::select! {
            result=&mut response=>{
                while let Ok(incoming)=incoming.try_recv() {
                    commands.observe_for(identity.native, &incoming);
                    match incoming {
                        Incoming::Notification{method,params:frame} if method=="session/update"=>{
                            for event in translator.update(&frame,crate::model::now_ms()).map_err(wire_error)? {retain_item(retained,&event);emit(event);}
                        },
                        Incoming::Request{..}=>return Err(CalmError::Conflict("ACP completed while a client request was still pending".into())),
                        _=>{},
                    }
                }
                let response=match result {
                    // The agent answered: the turn is over, so its outcome is known.
                    Err(provider::acp::Error::Remote{message,..})=>return Ok(translator.fail(&message,crate::model::now_ms())),
                    result=>result.map_err(wire_error)?,
                };
                let response:protocol::PromptResponse=protocol::decode(response).map_err(wire_error)?;
                // A turn the agent ended itself after the turn declined one of its requests ends
                // with that reason, not as one the person interrupted.
                if matches!(response.stop_reason,protocol::StopReason::Cancelled) && stop_at.is_none()
                    && let Some(reason)=approvals.declined() {
                    return Ok(translator.fail(&reason,crate::model::now_ms()));
                }
                let declined = stop_at.is_none().then(|| approvals.declined_permission()).flatten();
                return Ok::<_,CalmError>(translator.finish_after_permission(response.stop_reason,declined.as_ref(),crate::model::now_ms()));
            },
            changed=cancelled.changed(),if stop_at.is_none()=>{
                if changed.is_err() || !matches!(*cancelled.borrow(), CancelRequest::Running) {
                    stop_at=Some(tokio::time::Instant::now()+Duration::from_secs(5));
                    if !matches!(*cancelled.borrow(), CancelRequest::Attempted) {
                    tokio::time::timeout(Duration::from_secs(3),async {
                        approvals.cancel(identity.native, cancel_gate).await.map_err(wire_error)
                    }).await.map_err(|_|CalmError::Conflict("ACP cancellation writes timed out".into()))??;
                    }
                }
            },
            _=async{match stop_at{Some(at)=>tokio::time::sleep_until(at).await,None=>std::future::pending().await}}=>return Err(CalmError::Conflict("ACP cancellation outcome is unknown".into())),
            message=incoming.recv(),if incoming_open=>match message {
                Some(frame @ Incoming::Notification{..})=>{
                    commands.observe_for(identity.native, &frame);
                    let Incoming::Notification {method,params} = frame else { unreachable!() };
                    if method != "session/update" { continue; }
                    for event in translator.update(&params,crate::model::now_ms()).map_err(wire_error)? {retain_item(retained,&event);emit(event);}
                },
                Some(Incoming::Request{id,method,params})=>approvals.request(id,&method,params).await.map_err(wire_error)?,
                // The RPC owner publishes its exact result before dropping this sender.
                // EOF can race with an already queued success or protocol failure.
                None=>incoming_open=false,
            }
        }
    }
}
pub(crate) fn retain_item(retained: &mut Vec<Value>, event: &PlannerEvent) {
    if let PlannerEventKind::Item { params, phase, .. } = &event.kind
        && let Some(id) = params["item"]["id"].as_str()
    {
        let frame = json!({"method":phase.method(),"params":params});
        if let Some(prior) = retained
            .iter_mut()
            .find(|frame| frame["params"]["item"]["id"] == id)
        {
            *prior = frame;
        } else {
            retained.push(frame);
        }
    }
}
