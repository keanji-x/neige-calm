//! #2493: an unresolved write fences the next one on the caller's connection, whichever path
//! (`message` or typed `input`) made it. A write held past its delivery budget, or whose request
//! was cancelled after it was queued, lands exactly once when admission resumes, and nothing new
//! is written until its outcome is known.
use super::task_terminal::stop;
use super::terminal_support::Harness;
use super::worker_message::{
    Fixture, assert_refusal, expected, message, running, wait_for_reads, written,
};
use calm_server::db::prelude::*;
use calm_server::mcp_server::registry::ToolCallIdentity;
use calm_server::model::CardRole;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::terminal_interaction::Target;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::OwnedMutexGuard;

const UNRESOLVED: &str = "prior input outcome unknown";

type Held = Arc<Mutex<Option<OwnedMutexGuard<()>>>>;

/// Hold the renderer's write admission once the next message client has attached.
fn hold_next_message_write(h: &Harness) -> (Held, tokio::sync::oneshot::Receiver<()>) {
    let held: Held = Arc::new(Mutex::new(None));
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let registry = h.state.terminal_renderer.clone();
    let slot = held.clone();
    h.interaction()
        .set_message_write_seam(Box::new(move |terminal: String| {
            Box::pin(async move {
                let entry = registry.get(&terminal).unwrap();
                *slot.lock().unwrap() = Some(entry.handle.input_barrier.hold_for_test().await);
                let _ = entered.send(());
            })
        }));
    (held, entered_rx)
}

/// After the first write lands, the fence clears and the next message is written.
async fn release_then_next_lands(h: &Harness, f: &Fixture, held: &Held, first: &[u8]) {
    drop(held.lock().unwrap().take());
    assert_eq!(wait_for_reads(&f.log, 1).await, vec![first.to_vec()]);
    written(&message(h, json!({"attempt_id":f.worker.task}), "after", "after").await);
    assert_eq!(
        wait_for_reads(&f.log, 2).await,
        vec![first.to_vec(), expected(&f.worker.task, "after")],
        "the held write landed exactly once"
    );
}

#[tokio::test]
async fn message_held_past_its_budget_lands_once_and_fences_the_next() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    // Control for the typed input below, claimed before admission is held.
    h.ok(
        "neige_terminal_control",
        json!({"attempt_id":f.worker.task,"action":"claim"}),
    )
    .await;
    let (held, _entered) = hold_next_message_write(&h);
    let reply = message(&h, json!({"attempt_id":f.worker.task}), "held", "held").await;
    assert!(reply.get("error").is_none(), "{reply}");
    assert_eq!(reply["result"]["structuredContent"]["outcome"], "unknown");
    let next = message(&h, json!({"attempt_id":f.worker.task}), "next", "next").await;
    assert_refusal(&next, -32403, None, UNRESOLVED);
    // Typed input on a fresh observation is fenced by the same unresolved message.
    let typed = |key: &'static str| {
        let h = &h;
        let task = f.worker.task.clone();
        async move {
            let view = h
                .ok(
                    "neige_terminal_read",
                    json!({"attempt_id":task,"wait_ms":50}),
                )
                .await;
            h.call(
                "neige_terminal_input",
                json!({"attempt_id":task,"observation_id":view["observation_id"],
                    "idempotency_key":key,"action":{"type":"text","text":"keys"}}),
            )
            .await
        }
    };
    assert_refusal(&typed("fenced").await, -32403, None, UNRESOLVED);
    // The same key still replays its receipt.
    let replay = message(&h, json!({"attempt_id":f.worker.task}), "held", "held").await;
    assert_eq!(replay["result"]["structuredContent"]["outcome"], "unknown");
    release_then_next_lands(&h, &f, &held, &expected(&f.worker.task, "held")).await;
    // Settled: typed input writes again, and only now.
    let written_keys = typed("keys").await;
    assert_eq!(
        written_keys["result"]["structuredContent"]["outcome"], "written",
        "{written_keys}"
    );
    assert_eq!(
        wait_for_reads(&f.log, 3).await,
        vec![
            expected(&f.worker.task, "held"),
            expected(&f.worker.task, "after"),
            b"keys".to_vec()
        ]
    );
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn message_cancelled_after_enqueue_fences_the_next() {
    let h = Harness::start().await;
    let f = running(&h, "codex", true, false).await;
    let planner = h
        .sql
        .cards_by_track(&h.track)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "codex")
        .unwrap();
    let identity = ToolCallIdentity {
        card_id: planner.id.to_string(),
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        session_id: h.session_id.clone(),
        track_id: Some(h.track.clone()),
        area_id: h.area_id.clone(),
        thread_id: "card-bound".into(),
    };
    let (held, entered) = hold_next_message_write(&h);
    let service = h.interaction();
    let target = Target::Attempt(f.worker.task.clone());
    let action = json!({"type":"message","text":"cancelled"});
    let call = service.message(&identity, &target, "cancelled", action, None);
    let driver = async {
        entered.await.unwrap();
        // The input reaches the writer, which waits for admission.
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    tokio::select! {
        result = call => panic!("the held message must not settle: {result:?}"),
        () = driver => {}
    }
    let next = message(&h, json!({"attempt_id":f.worker.task}), "next", "next").await;
    assert_refusal(&next, -32403, None, UNRESOLVED);
    release_then_next_lands(&h, &f, &held, &expected(&f.worker.task, "cancelled")).await;
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn message_after_unknown_input_is_refused() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    h.ok(
        "neige_terminal_control",
        json!({"attempt_id":f.worker.task,"action":"claim"}),
    )
    .await;
    let view = h
        .ok(
            "neige_terminal_read",
            json!({"attempt_id":f.worker.task,"wait_ms":50}),
        )
        .await;
    let entry = h.state.terminal_renderer.get(&f.worker.terminal).unwrap();
    let held: Held = Arc::new(Mutex::new(Some(
        entry.handle.input_barrier.hold_for_test().await,
    )));
    let typed = h
        .input(
            &f.worker.terminal,
            &view,
            "keys",
            json!({"type":"text","text":"keys"}),
        )
        .await;
    assert_eq!(typed["outcome"], "unknown", "{typed}");
    let next = message(&h, json!({"attempt_id":f.worker.task}), "next", "next").await;
    assert_refusal(&next, -32403, None, UNRESOLVED);
    release_then_next_lands(&h, &f, &held, b"keys").await;
    stop(&h, &f.worker).await;
}
