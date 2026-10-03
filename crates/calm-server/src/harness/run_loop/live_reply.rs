//! The run loop's side of the live replies (#1923 S2): which item events open and close a live
//! reply, and the settle hook that stores what an interrupted or failed turn left open.

use std::sync::Arc;

use calm_types::harness::HarnessLiveReply;
use serde_json::{Value, json};

use super::{Inner, emit_item_added, insert_item_row, item_turn_id};
use crate::harness::live_replies::LiveReplyWriter;
use crate::harness::planner_event::ItemPhase;

/// The stored item type of a reply, the one item type that streams.
const REPLY_ITEM_TYPE: &str = "agentMessage";

/// Called once an item event's row is stored: an `agentMessage` start opens a live reply, and a
/// completion removes the item, whose text is now durable.
pub(super) fn on_item(live: &LiveReplyWriter, phase: ItemPhase, params: &Value) {
    let Some(item_id) = params.pointer("/item/id").and_then(Value::as_str) else {
        return;
    };
    match phase {
        ItemPhase::Started => {
            if params.pointer("/item/type").and_then(Value::as_str) == Some(REPLY_ITEM_TYPE)
                && let Some(turn_id) = item_turn_id(params)
            {
                live.reply_started(turn_id, item_id);
            }
        }
        ItemPhase::Completed => live.item_stored(item_id),
    }
}

/// The settle hook, run on every `TurnCompleted` branch before its outcome row is written. An
/// `interrupted` or `failed` turn's still-open replies are stored as `_partial` completions, so
/// they get lower row ids than the outcome; any other outcome discards them. Taking the replies
/// out of the live state is what makes each write happen at most once per item.
pub(super) async fn settle(
    inner: &Arc<Inner>,
    live: &LiveReplyWriter,
    turn_id: &str,
    turn: &Value,
) {
    let open = live.settle(turn_id);
    let ended_early = matches!(
        turn.get("status").and_then(Value::as_str),
        Some("interrupted" | "failed")
    );
    if open.is_empty() || !ended_early {
        return;
    }
    let Some(thread_id) = inner.thread_id.read().await.clone() else {
        tracing::warn!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            turn_id,
            "planner harness dropping partial replies: no thread is known yet"
        );
        return;
    };
    for reply in open {
        let item_id = reply.item_id.clone();
        if let Err(error) = store_partial(inner, &thread_id, turn_id, reply).await {
            tracing::warn!(
                worker_session_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                turn_id,
                item_id,
                error = %error,
                "planner harness could not store a partial reply"
            );
        }
    }
}

/// One partial reply as the `item/completed` row Codex's own completion would have written, marked
/// `_partial` as provenance: the kernel wrote it from the streamed text.
async fn store_partial(
    inner: &Arc<Inner>,
    thread_id: &str,
    turn_id: &str,
    reply: HarnessLiveReply,
) -> crate::error::Result<()> {
    let method = ItemPhase::Completed.method();
    let params = json!({
        "threadId": thread_id,
        "turnId": turn_id,
        "item": { "id": reply.item_id, "type": REPLY_ITEM_TYPE, "text": reply.text },
        "completedAtMs": crate::model::now_ms(),
        "_partial": true,
    });
    let row_id = insert_item_row(
        inner,
        thread_id,
        Some(turn_id),
        Some(&reply.item_id),
        Some(REPLY_ITEM_TYPE),
        method,
        &serde_json::to_string(&params)?,
        None,
    )
    .await?;
    emit_item_added(
        inner,
        row_id,
        Some(reply.item_id),
        Some(REPLY_ITEM_TYPE.into()),
        Some(turn_id.into()),
        method.into(),
    )
    .await
}
