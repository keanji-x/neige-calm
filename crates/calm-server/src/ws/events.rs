//! `GET /api/events` (WebSocket upgrade). **Owned by Track C.**
//!
//! ## Protocol
//!
//! ### Client → server (text frame, JSON)
//!
//! ```json
//! { "sub": ["wave:w-001", "cove:c-001", "plugin:*"] }
//! ```
//!
//! Replaces the subscription set. Send `{"sub": ["*"]}` for firehose
//! (debug only). An empty array means "subscribe to nothing" — the server
//! keeps the connection open but forwards no events.
//!
//! Scope D will add an optional `since: <lastEventId>` field that triggers
//! replay from the `events` table. Scope A only persists events; live
//! broadcast continues to be the sole forwarding path.
//!
//! ### Server → client (text frame, JSON)
//!
//! Each event is the `Event` enum serialized with a leading `_id` field
//! per design doc §2.4:
//!
//! ```json
//! { "_id": 1729, "ev": "wave.updated", "data": { "id":"w-001", ... } }
//! ```
//!
//! Forwarded only if `event::topics(ev)` intersects the client's `sub` set.
//!
//! ### Implementation hints
//!
//!   * `state.events.subscribe()` gives you a
//!     `broadcast::Receiver<BroadcastEnvelope>` carrying the assigned
//!     `events.id` alongside the typed `Event`.
//!   * On `Lagged(n)`: log + close. Client should reconnect + refetch.
//!   * Keep the subscription set in a local `HashSet<String>` per connection.

use crate::event;
use crate::event::BroadcastEnvelope;
use crate::state::AppState;
use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::HashSet;
use tokio::sync::broadcast;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/events", get(upgrade))
}

async fn upgrade(ws: WebSocketUpgrade, State(s): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle(socket, s))
}

/// Wire shape of a client → server subscription message.
///
/// We deserialize with `serde_json::from_str` and silently ignore malformed
/// frames. A successful parse **replaces** (not extends) the connection's
/// subscription set.
#[derive(Debug, Deserialize)]
struct SubMessage {
    sub: Vec<String>,
}

async fn handle(socket: WebSocket, state: AppState) {
    let (mut tx, mut rx) = socket.split();
    let mut bus = state.events.subscribe();
    let mut subs: HashSet<String> = HashSet::new();

    loop {
        tokio::select! {
            // From client.
            client = rx.next() => match client {
                Some(Ok(Message::Text(t))) => {
                    match serde_json::from_str::<SubMessage>(t.as_str()) {
                        Ok(msg) => {
                            subs = msg.sub.into_iter().collect();
                            tracing::debug!(count = subs.len(), "ws /api/events: subs replaced");
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "ws /api/events: malformed sub frame, ignoring");
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(e)) => {
                    tracing::debug!(error = %e, "ws /api/events: client recv error, closing");
                    break;
                }
                // Ignore Ping/Pong (axum handles Pong automatically) and Binary.
                _ => {}
            },

            // From broadcast bus.
            env = bus.recv() => match env {
                Ok(env) => {
                    if event::topics(&env.event).iter().any(|t| subs.contains(t)) {
                        let payload = match render_envelope(&env) {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::error!(error = %e, "ws /api/events: event serialize failed");
                                continue;
                            }
                        };
                        if tx.send(Message::Text(payload.into())).await.is_err() {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "ws /api/events: client lagged, closing");
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}

/// Serialize a `BroadcastEnvelope` into the wire form
/// `{"_id": <id>, "ev": <tag>, "data": <payload>}`.
///
/// The `Event` enum already serializes to `{"ev": ..., "data": ...}` via
/// its `#[serde(tag, content)]` attributes; we splice `_id` in alongside.
/// Doing it this way (rather than a sibling `Serialize` impl on
/// `BroadcastEnvelope`) keeps the ts-rs generated TS type for `Event`
/// authoritative — the envelope shape is a transport concern, not a
/// domain one. See design doc §2.4 for the rationale on `_id` living
/// outside the `Event` namespace.
///
/// Key ordering on the wire is alphabetical (serde_json default); the
/// frontend's zod schemas parse by name, so the order doesn't matter
/// semantically.
fn render_envelope(env: &BroadcastEnvelope) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(&env.event)?;
    if let serde_json::Value::Object(ref mut map) = value {
        map.insert("_id".to_string(), serde_json::Value::from(env.id));
    }
    serde_json::to_string(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use crate::model::Cove;

    fn sample_cove() -> Cove {
        Cove {
            id: "c-1".into(),
            name: "n".into(),
            color: "#fff".into(),
            sort: 0.0,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn render_envelope_has_id_and_keeps_event_shape() {
        let env = BroadcastEnvelope {
            id: 42,
            event: Event::CoveUpdated(sample_cove()),
        };
        let s = render_envelope(&env).expect("render");
        // Key ordering on the wire is implementation-defined (serde_json
        // sorts alphabetically by default); the contract we care about
        // is that `_id`, `ev`, and `data` are all present at the top level
        // with the right values. Frontend `zod` parsing is by key name,
        // not position, so this is what matters.
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["_id"], 42);
        assert_eq!(v["ev"], "cove.updated");
        assert_eq!(v["data"]["id"], "c-1");
        assert_eq!(v["data"]["name"], "n");
    }

    #[test]
    fn render_envelope_keeps_zero_id() {
        // Out-of-scope (Scope A) producers still emit `id = 0` until they
        // convert. The wire envelope must surface that as `_id: 0` rather
        // than dropping the field — `0` is the agreed sentinel for "no
        // persisted row yet" (see `BroadcastEnvelope` docs).
        let env = BroadcastEnvelope {
            id: 0,
            event: Event::CoveUpdated(sample_cove()),
        };
        let s = render_envelope(&env).expect("render");
        assert!(s.contains(r#""_id":0"#), "got: {s}");
    }
}
