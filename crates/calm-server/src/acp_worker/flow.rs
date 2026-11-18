//! ACP activity uses the existing normalized Worker flow sink and checkpoint CAS.
use super::admission::Admission;
use crate::{
    error::{CalmError, Result},
    operation::SpawnCtx,
    worker_flow::cursor::{self, CursorWriter},
};
use calm_exec::flow::FlowRowCtx;
use calm_types::{
    worker::WorkerProviderKind,
    worker_flow::{FlowEnvelope, ToolCallId, ToolResultBlock, TurnOutcome, WorkerFlowItem},
};
use provider::events::{ItemPhase, PlannerEvent, PlannerEventKind};
use serde_json::{Value, json};
use tokio::sync::mpsc;

pub(crate) async fn capture(
    ctx: SpawnCtx,
    admission: Admission,
    mut incoming: mpsc::Receiver<PlannerEvent>,
) -> Result<()> {
    let state = admission.check(true).await?;
    let worker_provider = WorkerProviderKind::from(&state.requested_selection.provider);
    let stored = cursor::get(ctx.repo.as_ref(), &admission.card, "acp_session")
        .await
        .map_err(|error| CalmError::Internal(error.to_string()))?;
    let mut index = stored.as_ref().map_or(0, |cursor| cursor.record_index);
    let sink = calm_truth::worker_flow_sink::WorkerFlowSink::new(ctx.repo.clone());
    let mut writer = CursorWriter::new(
        &admission.card,
        "acp_session",
        &admission.worker,
        stored.as_ref(),
        tokio_util::sync::CancellationToken::new(),
    );
    let row = FlowRowCtx {
        session_id: admission.worker.clone().into(),
        track_id: Some(admission.track.clone()),
        card_id: Some(admission.card.clone()),
    };
    while let Some(event) = incoming.recv().await {
        index += 1;
        let env = FlowEnvelope {
            seq: index as u64,
            turn: state.turn_index,
            session_id: admission.worker.clone().into(),
            provider: worker_provider,
            timestamp: Some(crate::model::now_ms()),
            source_uuid: admission.turn.clone(),
            provider_extra: None,
            raw_ref: None,
        };
        let items = normalize(event, env);
        if !writer
            .persist(
                &sink,
                &row,
                items,
                index,
                0,
                admission.turn.as_deref(),
                None,
            )
            .await
            .map_err(|error| CalmError::Internal(error.to_string()))?
        {
            return Err(CalmError::Conflict(
                "ACP Worker activity checkpoint changed".into(),
            ));
        }
    }
    Ok(())
}
fn normalize(event: PlannerEvent, mut env: FlowEnvelope) -> Vec<WorkerFlowItem> {
    match event.kind {
        PlannerEventKind::Item { params, phase, .. } => {
            let item = &params["item"];
            env.provider_extra = Some(params.clone());
            let id = item["id"].as_str().unwrap_or("unknown");
            match item["type"].as_str() {
                Some("agentMessage") if phase == ItemPhase::Completed => {
                    vec![WorkerFlowItem::AgentMessage {
                        env,
                        text: item["text"].as_str().unwrap_or_default().into(),
                        is_final: false,
                        phase: None,
                    }]
                }
                Some("reasoning") if phase == ItemPhase::Completed => {
                    vec![WorkerFlowItem::Reasoning {
                        env,
                        summary: strings(&item["summary"]),
                        content: strings(&item["content"]),
                        redacted: false,
                    }]
                }
                Some("dynamicToolCall") if phase == ItemPhase::Started => {
                    vec![WorkerFlowItem::ToolCall {
                        env,
                        call_id: ToolCallId::from(id),
                        name: item["tool"].as_str().unwrap_or("ACP tool").into(),
                        input: item["arguments"].clone(),
                        input_summary: None,
                    }]
                }
                Some("dynamicToolCall") if phase == ItemPhase::Completed => {
                    vec![WorkerFlowItem::ToolResult {
                        env,
                        call_id: ToolCallId::from(id),
                        ok: item["status"] != "failed",
                        output: vec![ToolResultBlock::Text {
                            text: item["result"].to_string(),
                        }],
                        output_summary: None,
                        error: None,
                    }]
                }
                _ => vec![],
            }
        }
        PlannerEventKind::TurnCompleted { turn } => {
            env.provider_extra = Some(json!({"turn":turn}));
            let outcome = match turn["status"].as_str() {
                Some("completed") => TurnOutcome::Completed,
                Some("interrupted") => TurnOutcome::Aborted {
                    reason: "ACP cancellation".into(),
                },
                _ => TurnOutcome::Failed {
                    message: turn["error"]["message"].as_str().map(str::to_owned),
                },
            };
            vec![WorkerFlowItem::TurnEnded { env, outcome }]
        }
        _ => vec![],
    }
}
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|array| {
            array
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
