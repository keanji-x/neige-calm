//! RECORD_SESSION recorder × `BroadcastEnvelope.actor` integration test.
//!
//! Before issue #39 closed, `spawn_session_recorder` hardcoded
//! `"actor": "unknown"` on every recorded line because the bus envelope
//! didn't carry the producing actor. This test pins the post-#39 behavior:
//! the recorder captures whatever actor the producing `write_with_event` /
//! `log_pure_event` call threaded through, so replayed traces preserve real
//! attribution end-to-end.

use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, cove_create_tx};
use calm_server::db::write_with_event_typed;
use calm_server::event::{Event, EventBus};
use calm_server::model::NewCove;
use calm_server::replay::spawn_session_recorder;
use serde_json::Value;
use tempfile::NamedTempFile;

/// Boot an in-memory repo, an event bus, and a tempfile-backed recorder.
async fn boot() -> (Arc<dyn Repo>, EventBus, NamedTempFile) {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory repo"),
    );
    let bus = EventBus::new();
    let tmp = NamedTempFile::new().expect("tempfile");
    spawn_session_recorder(&bus, tmp.path().to_path_buf());
    // Recorder subscribes inside `tokio::spawn` — give it a tick to land
    // its subscription before we start emitting.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    (repo, bus, tmp)
}

/// Drive one `write_with_event_typed` cove create with the supplied actor.
async fn create_cove_as(repo: &dyn Repo, bus: &EventBus, actor: &str, name: &str) -> i64 {
    let p = NewCove {
        name: name.to_string(),
        color: "#000".into(),
        sort: None,
    };
    let (_cove, event_id) = write_with_event_typed(repo, actor, None, bus, move |tx| {
        Box::pin(async move {
            let c = cove_create_tx(tx, p).await?;
            Ok((c.clone(), Event::CoveUpdated(c)))
        })
    })
    .await
    .expect("write_with_event ok");
    event_id
}

/// Read all NDJSON lines off the recorded session file, parsed as JSON.
fn read_recorded(tmp: &NamedTempFile) -> Vec<Value> {
    // Reopen via std::fs so we observe the recorder's flushed bytes.
    let file = std::fs::File::open(tmp.path()).expect("reopen session file");
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(&l).expect("recorded line is JSON"))
        .collect()
}

#[tokio::test]
async fn recorder_captures_real_actor_per_envelope() {
    let (repo, bus, tmp) = boot().await;

    // Three writes with three distinct actors that match the design doc's
    // grammar — exactly the shape RECORD_SESSION needs to preserve for
    // `replay --assert` to be useful as a bug-report artifact.
    let _id_user = create_cove_as(&*repo, &bus, "user", "u").await;
    let _id_ai = create_cove_as(&*repo, &bus, "ai:codex", "a").await;

    // Pure event with `"kernel"` actor — same `BroadcastEnvelope` shape via
    // `log_pure_event`. Carries no entity write.
    let _kernel_id = repo
        .log_pure_event(
            "kernel",
            None,
            &bus,
            Event::PluginState {
                id: "todo".into(),
                state: "Running".into(),
                last_error: None,
            },
        )
        .await
        .expect("log_pure_event ok");

    // Give the recorder a beat to drain the bus and flush all three lines.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let lines = read_recorded(&tmp);
    assert_eq!(
        lines.len(),
        3,
        "expected three recorded lines, got {lines:?}"
    );

    let actors: Vec<&str> = lines
        .iter()
        .map(|l| l["actor"].as_str().expect("actor is a string"))
        .collect();
    assert_eq!(actors, vec!["user", "ai:codex", "kernel"]);

    // And no line should have the pre-#39 placeholder.
    for l in &lines {
        assert_ne!(
            l["actor"].as_str(),
            Some("unknown"),
            "recorded line still carries pre-#39 placeholder: {l}"
        );
        // Sanity: payload + kind are still recorded.
        assert!(l["kind"].is_string(), "kind missing: {l}");
        assert!(!l["payload"].is_null(), "payload missing: {l}");
    }
}

#[tokio::test]
async fn envelope_carries_actor_alongside_event() {
    // Unit-level pin: the bus envelope itself carries `actor` so any
    // future subscriber (not just the recorder) can read it directly.
    let (repo, bus, _tmp) = boot().await;
    let mut sub = bus.subscribe();
    let event_id = create_cove_as(&*repo, &bus, "ai:codex", "c").await;
    let env = sub.recv().await.expect("envelope delivered");
    assert_eq!(env.id, event_id);
    assert_eq!(env.actor, "ai:codex");
    match env.event {
        Event::CoveUpdated(_) => {}
        other => panic!("expected CoveUpdated, got {other:?}"),
    }
}
