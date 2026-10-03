//! The run loop's side of the live replies (#1923 S2): which item events open and close a live
//! reply, and the settle hook that stores what an interrupted or failed turn left open.

use std::sync::Arc;

use serde_json::{Value, json};

use super::{Inner, emit_item_added, insert_item_row, item_turn_id};
use crate::harness::live_replies::{LiveReplyWriter, OpenReply};
use crate::harness::planner_event::ItemPhase;
use crate::harness::state::{HarnessState, IssuingKind};

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
                live.reply_started(turn_id, item_id, &params["item"]);
            }
        }
        ItemPhase::Completed => live.item_stored(item_id),
    }
}

/// A `TurnStarted` the state machine did not accept still starts the live turn when an interrupt
/// issued before the start drained targets it: that turn streams, and its interrupted completion
/// settles it. The state machine is left as it is.
pub(super) fn on_unaccepted_start(
    live: &LiveReplyWriter,
    state: &HarnessState,
    issued_turn_id: Option<&str>,
    turn_id: &str,
) {
    if let HarnessState::Issuing {
        kind: IssuingKind::Interrupt { target_turn_id, .. },
        ..
    } = state
        && (target_turn_id == turn_id || issued_turn_id == Some(turn_id))
    {
        live.turn_started(turn_id);
    }
}

/// The settle hook, run once on every `TurnCompleted` branch before its outcome row is written.
/// An `interrupted` or `failed` turn's still-open replies are stored as `_partial` completions, so
/// they get lower row ids than the outcome; any other outcome discards them. Settling the turn is
/// what makes each write happen at most once per item. A reply leaves the live state only once its
/// row is stored: one that fails to store stays live until the next turn starts.
pub(super) async fn settle(
    inner: &Arc<Inner>,
    live: &LiveReplyWriter,
    turn_id: &str,
    turn: &Value,
) {
    let ended_early = matches!(
        turn.get("status").and_then(Value::as_str),
        Some("interrupted" | "failed")
    );
    if !ended_early {
        live.discard(turn_id);
        return;
    }
    let open = live.settle(turn_id);
    if open.is_empty() {
        return;
    }
    let Some(thread_id) = inner.thread_id.read().await.clone() else {
        tracing::warn!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            turn_id,
            "planner harness not storing partial replies: no thread is known yet"
        );
        return;
    };
    for reply in open {
        let item_id = reply.item_id.clone();
        if let Err(error) = store_partial(inner, live, &thread_id, turn_id, reply).await {
            tracing::warn!(
                worker_session_id = %inner.worker_session_id,
                card_id = %inner.card_id,
                turn_id,
                item_id,
                error = %error,
                "planner harness could not store a partial reply; it stays live"
            );
        }
    }
}

/// One partial reply as an `item/completed` row: the item its `item/started` carried, with the
/// streamed text, marked `_partial` as provenance (the kernel wrote it). Once the row is stored
/// the reply leaves the live state, and the row is announced like any item.
async fn store_partial(
    inner: &Arc<Inner>,
    live: &LiveReplyWriter,
    thread_id: &str,
    turn_id: &str,
    reply: OpenReply,
) -> crate::error::Result<()> {
    let method = ItemPhase::Completed.method();
    let mut item = reply.started;
    item["text"] = Value::String(reply.text);
    let params = json!({
        "threadId": thread_id,
        "turnId": turn_id,
        "item": item,
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
    live.item_stored(&reply.item_id);
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
