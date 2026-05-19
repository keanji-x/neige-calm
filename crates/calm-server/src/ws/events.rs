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
//! ### Server → client (text frame, JSON)
//!
//! Each event is the `Event` enum serialized:
//!
//! ```json
//! { "ev": "wave.updated", "data": { "id":"w-001", ... } }
//! ```
//!
//! Forwarded only if `event::topics(ev)` intersects the client's `sub` set.
//!
//! ### Implementation hints
//!
//!   * `state.events.subscribe()` gives you a `broadcast::Receiver<Event>`.
//!   * On `Lagged(n)`: log + close. Client should reconnect + refetch.
//!   * Keep the subscription set in a local `HashSet<String>` per connection.

use crate::event;
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
            ev = bus.recv() => match ev {
                Ok(ev) => {
                    if event::topics(&ev).iter().any(|t| subs.contains(t)) {
                        let payload = match serde_json::to_string(&ev) {
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
