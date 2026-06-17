//! `GET /api/events` (WebSocket upgrade). **Owned by Track C.**
//!
//! ## Protocol
//!
//! ### Client → server (text frame, JSON)
//!
//! ```json
//! { "sub": ["wave:w-001", "cove:c-001", "plugin:*"], "since": 1729 }
//! ```
//!
//! Replaces the subscription set. Send `{"sub": ["*"]}` for firehose
//! (debug only). An empty array means "subscribe to nothing" — the server
//! keeps the connection open but forwards no events.
//!
//! `since` (Scope D, sync engine phase 2) is optional:
//!
//!   * **Absent** — behave exactly as today, live broadcast only. Old
//!     clients keep working untouched.
//!   * **0** — replay every event in the log (cold-start tests).
//!   * **N** — replay every event with `id > N` matching the topic filter,
//!     then transition to live forwarding.
//!
//! Subscription is **replace-on-message**, same as before — a fresh
//! `{sub, since}` mid-connection re-anchors the cursor and re-runs the
//! replay query.
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
//! ### Control frames (Scope D, server-only)
//!
//! Two synthetic envelopes that are **not** part of the typed `Event` enum
//! — they're transport-level signals the WS handler hand-crafts and the
//! client must handle out-of-band before running the regular zod parse:
//!
//! ```json
//! { "_id": <server_tip_id>, "ev": "_replay_complete" }
//! { "_id": <earliest_id>, "ev": "_snapshot_required", "data": { "earliest_id": <id> } }
//! ```
//!
//! * `_replay_complete` is sent once, after the historical replay window
//!   has been streamed and any dupes from the concurrent live broadcast
//!   have been drained. Lets the client drop any "reconnecting" UI banner
//!   and run a defensive `qc.invalidateQueries()` to catch optimistic
//!   state that may have drifted during the window. The `_id` is the
//!   server's actual `events.id` tip (`MAX(id)` of the live log) — NOT
//!   the highest id replayed in this window. That gives a client whose
//!   persisted cursor is *ahead* of the server tip (the dev
//!   `/dev/reset` path resets `sqlite_sequence`, so re-seeded events
//!   restart at id=1) a per-connection signal it can use to detect the
//!   reset and re-bootstrap its cache. Issue #290.
//! * `_snapshot_required` is sent when the client's `since` cursor
//!   predates the retention horizon (the smallest live `events.id`).
//!   After sending it, the server closes the connection. The client must
//!   clear its persisted query cache (`qc.clear()`) and reconnect cold.
//!
//! These frames stay out of the `Event` enum so they don't pollute the
//! ts-rs export — the client's `wireEventSchema` zod union doesn't
//! recognize them, so the client must extract `_replay_complete` /
//! `_snapshot_required` from the envelope **before** schema validation
//! runs.
//!
//! ### Implementation hints
//!
//!   * `state.events.subscribe()` gives you a
//!     `broadcast::Receiver<BroadcastEnvelope>` carrying the assigned
//!     `events.id` alongside the typed `Event`. Subscribe BEFORE running
//!     the `events_since` query — the design's "subscribe-first" pattern
//!     (§2.2) is what guards against drops at the replay/live boundary.
//!   * On `Lagged(n)` during replay: close with `_snapshot_required` so
//!     the client falls back to a cold refetch.
//!   * Keep the subscription set in a local `HashSet<String>` per connection.

use crate::db::RouteRepo;
use crate::event;
#[cfg(test)]
use crate::event::EventScope;
use crate::event::{BroadcastEnvelope, SYNC_EVENT_VERSION};
use crate::ids::ActorId;
use crate::session_projection_lookup::project_runtime_into_event_payload;
use crate::state::AppState;
use crate::validation::should_skip_event_for_overlay_version;
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
///
/// `since` is the cursor protocol's only optional field (Scope D). Absence
/// preserves the pre-Phase-2 behavior; presence triggers replay.
#[derive(Debug, Deserialize)]
struct SubMessage {
    sub: Vec<String>,
    #[serde(default)]
    since: Option<i64>,
}

async fn handle(socket: WebSocket, state: AppState) {
    let (mut tx, mut rx) = socket.split();
    // Always subscribe up-front so any live event emitted between now and
    // the first `{sub, since}` is buffered in the broadcast channel rather
    // than dropped. The design's "subscribe-first" pattern (§2.2) is the
    // *only* mechanism that prevents drops at the replay→live boundary,
    // so it has to come before any SQL query against the events table.
    let mut bus = state.events.subscribe();
    let mut subs: HashSet<String> = HashSet::new();
    // Tracks the largest replayed event id while a replay is in flight.
    // Any live event with `id <= last_replayed_id` is a duplicate (already
    // included in the replay set) and gets dropped. `0` is the sentinel
    // for "no replay in progress" — production event ids start at 1.
    let mut last_replayed_id: i64 = 0;

    loop {
        tokio::select! {
            // From client.
            client = rx.next() => match client {
                Some(Ok(Message::Text(t))) => {
                    match serde_json::from_str::<SubMessage>(t.as_str()) {
                        Ok(msg) => {
                            subs = msg.sub.into_iter().collect();
                            tracing::debug!(count = subs.len(), since = ?msg.since, "ws /api/events: subs replaced");

                            if let Some(since) = msg.since {
                                // Replay path. Returns the `last_replayed_id`
                                // tip if the replay completed successfully,
                                // or `None` if we sent `_snapshot_required`
                                // and need to close. Anything happening on
                                // the bus during replay is buffered for us;
                                // we'll dedupe after.
                                match run_replay(
                                    &mut tx,
                                    state.repo.as_ref(),
                                    &subs,
                                    since,
                                ).await {
                                    ReplayOutcome::Streamed(tip) => {
                                        last_replayed_id = tip;
                                    }
                                    ReplayOutcome::SnapshotRequired => {
                                        // Sent the control frame already.
                                        // Close politely so the client
                                        // reconnects cleanly.
                                        break;
                                    }
                                    ReplayOutcome::ClientClosed => break,
                                }
                            } else {
                                last_replayed_id = 0;
                            }
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
                    // Dedupe vs the just-finished replay: any live broadcast
                    // whose id is in the replay set has already been sent.
                    if env.id != 0 && env.id <= last_replayed_id {
                        continue;
                    }
                    // Tier A read-side guard, broadcast surface (issue #198
                    // concern 4, PR #214 follow-up): drop kernel-owned
                    // overlay events whose persisted `schemaVersion` exceeds
                    // what this binary supports. `should_skip_event_for_overlay_version`
                    // already emits a structured warn for the drop. Filtered
                    // BEFORE the topic check so the warn fires regardless of
                    // who's subscribed — if the row exists at all, we want
                    // operators to see it.
                    if should_skip_event_for_overlay_version(&env.event) {
                        continue;
                    }
                    if event::topics(&env.event).iter().any(|t| subs.contains(t)) {
                        let mut env = env;
                        if let Err(e) =
                            project_runtime_into_event_payload(state.repo.as_ref(), &mut env.event)
                                .await
                        {
                            tracing::warn!(
                                error = %e,
                                "ws /api/events: runtime projection failed; dropping frame"
                            );
                            continue;
                        }
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

/// Outcome from a replay batch — distinguishes "replay completed, here's
/// the tip cursor", "we asked the client to do a cold refetch", and "the
/// client closed under us mid-stream". The outer handler uses this to
/// decide whether to update its dedup cursor, close cleanly, or keep going.
enum ReplayOutcome {
    Streamed(i64),
    SnapshotRequired,
    ClientClosed,
}

/// Stream the replay window for `since` to the client, then send the
/// `_replay_complete` synthetic envelope. Returns the server's actual
/// `events.id` tip (the new dedup cursor — `MAX(id)` of the live log,
/// queried after the in-window scan completes; falls back to the
/// in-window high-water mark if that query errors), or signals
/// `_snapshot_required` if `since` predates the retention horizon.
///
/// Implements the subscribe-first ordering: the broadcast subscription is
/// established *before* this function is called (in `handle`), so any
/// concurrent live write between the `events_since` query and the moment
/// the handler switches to live-forward mode is buffered for the dedup
/// pass at the top of the main loop's `bus.recv()` branch.
async fn run_replay<S>(
    tx: &mut S,
    repo: &dyn RouteRepo,
    subs: &HashSet<String>,
    since: i64,
) -> ReplayOutcome
where
    S: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
    // Retention check: if `since` predates the smallest surviving id, the
    // server can't honor a contiguous replay. Tell the client to throw
    // away its cache and refetch. `since == 0` is always honored (the
    // client deliberately wants "everything") even when the table is empty.
    if since > 0 {
        match repo.events_earliest_id().await {
            Ok(Some(earliest)) if since < earliest - 1 => {
                // since < earliest - 1 means there's at least one row
                // between `since + 1` and `earliest - 1` that's been
                // pruned. (since == earliest - 1 is the happy case: the
                // next id we'd send is exactly `earliest`, no gap.)
                let frame = snapshot_required_frame(earliest);
                let _ = tx.send(Message::Text(frame.into())).await;
                return ReplayOutcome::SnapshotRequired;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::error!(error = %e, "ws /api/events: events_earliest_id failed");
                // Fall through — better to attempt the replay than to
                // strand the client over a transient DB hiccup.
            }
        }
    }

    let rows = match repo.events_since(since, None).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, since, "ws /api/events: events_since query failed");
            // Send the control frame with the highest id we have (or
            // `since`) so the client at least sees a terminator and stops
            // waiting. The next live broadcast will keep things moving.
            let frame = replay_complete_frame(since);
            let _ = tx.send(Message::Text(frame.into())).await;
            return ReplayOutcome::Streamed(since);
        }
    };

    let mut last_id = since;
    for (id, event_version, scope, ev) in rows {
        // Tier A read-side guard, replay surface (issue #198 concern 4,
        // PR #214 follow-up): the events table can hold an
        // `Event::OverlaySet` row whose `schemaVersion` was written by a
        // newer kernel binary against the same DB. Drop it before the
        // topic check so the warn fires once per drop, then still advance
        // the cursor so the client's next reconnect resumes past this id
        // (matches the topic-filter skip semantics below — we never want
        // the same client to re-poll a row we already decided to filter).
        if should_skip_event_for_overlay_version(&ev) {
            last_id = id;
            continue;
        }
        // Topic filter applies to replayed frames too: a cursor-aware
        // client that just changed waves shouldn't suddenly see history
        // for a wave it didn't subscribe to.
        if !event::topics(&ev).iter().any(|t| subs.contains(t)) {
            // Skipped, but still advance the cursor — the client's next
            // reconnect should resume from here, not re-receive this id.
            last_id = id;
            continue;
        }
        // The replay path reconstructs an envelope from
        // `(id, event_version, scope, ev)` rows in the `events` table.
        // `events_since` does not return the `actor` column (replay is
        // read-only and the wire format omits actor — see
        // `render_envelope`), so we synthesize a `User` actor here. This
        // branch never feeds the `RECORD_SESSION` recorder. `event_version`
        // is round-tripped from the row's `event_version` column — old
        // rows backfill to `1` via the migration default. `scope` carries
        // through to the rendered envelope so future per-scope subscribers
        // (PR3+ of #136) see the same metadata fresh writes have.
        let env = BroadcastEnvelope {
            id,
            event_version,
            actor: ActorId::User,
            scope,
            event: ev,
        };
        let mut env = env;
        if let Err(e) = project_runtime_into_event_payload(repo, &mut env.event).await {
            tracing::warn!(
                error = %e,
                id,
                "ws /api/events: runtime projection failed; dropping frame"
            );
            last_id = id;
            continue;
        }
        let payload = match render_envelope(&env) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, id, "ws /api/events: replay serialize failed");
                last_id = id;
                continue;
            }
        };
        if tx.send(Message::Text(payload.into())).await.is_err() {
            return ReplayOutcome::ClientClosed;
        }
        last_id = id;
    }

    // Terminator: tells the client the historical window is fully
    // delivered. Stamped with the SERVER'S actual log tip — not the
    // highest id returned by the in-window scan — so a client whose
    // persisted cursor is *ahead* of the server's tip (e.g. the dev
    // `/dev/reset` path wipes `sqlite_sequence`, so re-seeded events
    // restart at id=1) sees `_replay_complete._id < lastEventId` and
    // can re-bootstrap. Issue #290.
    //
    // Falling back to `last_id` (which equals `since` when zero rows
    // matched) on a query error preserves the pre-#290 invariant
    // "terminator always carries a sensible id, even when the table is
    // transiently unreadable" — the client's no-regress guard treats it
    // as a no-op rather than a false reset signal.
    let server_tip = match repo.events_latest_id().await {
        Ok(Some(tip)) => tip,
        Ok(None) => 0,
        Err(e) => {
            tracing::error!(error = %e, "ws /api/events: events_latest_id failed");
            last_id
        }
    };
    let frame = replay_complete_frame(server_tip);
    if tx.send(Message::Text(frame.into())).await.is_err() {
        return ReplayOutcome::ClientClosed;
    }
    ReplayOutcome::Streamed(server_tip)
}

/// `{ "_id": <id>, "eventVersion": <n>, "ev": "_replay_complete" }`.
/// Hand-crafted to keep the control frame off the typed `Event` enum
/// (which ts-rs exports — adding underscore-prefixed variants would muddy
/// the client's discriminated union for no win).
///
/// `server_tip_id` is the server's actual `events.id` tip (`MAX(id)`),
/// not the highest id streamed in this replay window. See the module
/// docstring + `run_replay` comments for why — issue #290's reset
/// detection relies on the frame carrying the *server's* view of "how
/// far the log goes" so a stale-cursor client can compare.
///
/// Control frames are kernel-emitted and carry `SYNC_EVENT_VERSION` for
/// shape consistency with persisted-event frames — clients can treat
/// `eventVersion` as load-bearing on every frame they receive, not "only
/// on the replayed ones". They don't sit in the `events` table, so they
/// don't have a row-level version to round-trip; the constant is the
/// right source.
fn replay_complete_frame(server_tip_id: i64) -> String {
    serde_json::json!({
        "_id": server_tip_id,
        "eventVersion": SYNC_EVENT_VERSION,
        "ev": "_replay_complete",
    })
    .to_string()
}

/// `{ "_id": <earliest>, "eventVersion": <n>, "ev": "_snapshot_required",
///    "data": { "earliest_id": <id> } }`.
/// Server-only control frame; design §2.3. Carries `SYNC_EVENT_VERSION`
/// for the same consistency reason as `_replay_complete`.
fn snapshot_required_frame(earliest: i64) -> String {
    serde_json::json!({
        "_id": earliest,
        "eventVersion": SYNC_EVENT_VERSION,
        "ev": "_snapshot_required",
        "data": { "earliest_id": earliest },
    })
    .to_string()
}

/// Serialize a `BroadcastEnvelope` into the wire form
/// `{"_id": <id>, "eventVersion": <n>, "ev": <tag>, "data": <payload>}`.
///
/// The `Event` enum already serializes to `{"ev": ..., "data": ...}` via
/// its `#[serde(tag, content)]` attributes; we splice `_id` and
/// `eventVersion` in alongside. Doing it this way (rather than a sibling
/// `Serialize` impl on `BroadcastEnvelope`) keeps the ts-rs generated TS
/// type for `Event` authoritative — the envelope shape is a transport
/// concern, not a domain one. See design doc §2.4 for the rationale on
/// `_id` living outside the `Event` namespace.
///
/// `eventVersion` is camelCase to match the rest of the WS / REST wire
/// surface (`_id` is the documented exception — the leading underscore
/// signals "envelope, not payload"). The value is round-tripped from the
/// `events.event_version` column on the replay path and stamped from the
/// `SYNC_EVENT_VERSION` constant on fresh writes; clients use it to refuse
/// to replay a log they don't understand.
///
/// Key ordering on the wire is alphabetical (serde_json default); the
/// frontend's zod schemas parse by name, so the order doesn't matter
/// semantically.
fn render_envelope(env: &BroadcastEnvelope) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(&env.event)?;
    if let serde_json::Value::Object(ref mut map) = value {
        map.insert("_id".to_string(), serde_json::Value::from(env.id));
        map.insert(
            "eventVersion".to_string(),
            serde_json::Value::from(env.event_version),
        );
        // PR2 of #136 surfaces the event's home scope on the WS wire so
        // future MCP subscribers + frontend can route/filter without
        // re-parsing the event payload. Tagged `{kind, id}` shape per
        // `EventScope`'s serde attributes.
        map.insert("scope".to_string(), serde_json::to_value(&env.scope)?);
    }
    serde_json::to_string(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use crate::model::{Cove, CoveKind};

    fn sample_cove() -> Cove {
        Cove {
            id: "c-1".into(),
            name: "n".into(),
            color: "#fff".into(),
            sort: 0.0,
            kind: CoveKind::User,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn render_envelope_has_id_and_keeps_event_shape() {
        let env = BroadcastEnvelope {
            id: 42,
            event_version: SYNC_EVENT_VERSION,
            actor: ActorId::User,
            scope: EventScope::Cove { cove: "c-1".into() },
            event: Event::CoveUpdated(sample_cove()),
        };
        let s = render_envelope(&env).expect("render");
        // Key ordering on the wire is implementation-defined (serde_json
        // sorts alphabetically by default); the contract we care about
        // is that `_id`, `eventVersion`, `ev`, `data`, and `scope` are
        // all present at the top level with the right values. Frontend
        // `zod` parsing is by key name, not position, so this is what
        // matters.
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["_id"], 42);
        assert_eq!(v["eventVersion"], SYNC_EVENT_VERSION);
        assert_eq!(v["ev"], "cove.updated");
        assert_eq!(v["data"]["id"], "c-1");
        assert_eq!(v["data"]["name"], "n");
        assert_eq!(v["scope"]["kind"], "Cove");
        assert_eq!(v["scope"]["id"]["cove"], "c-1");
    }

    #[test]
    fn render_envelope_keeps_zero_id() {
        // Out-of-scope (Scope A) producers still emit `id = 0` until they
        // convert. The wire envelope must surface that as `_id: 0` rather
        // than dropping the field — `0` is the agreed sentinel for "no
        // persisted row yet" (see `BroadcastEnvelope` docs).
        let env = BroadcastEnvelope {
            id: 0,
            event_version: SYNC_EVENT_VERSION,
            actor: ActorId::Kernel,
            scope: EventScope::System,
            event: Event::CoveUpdated(sample_cove()),
        };
        let s = render_envelope(&env).expect("render");
        assert!(s.contains(r#""_id":0"#), "got: {s}");
        // System scope still surfaces on the wire as `{"kind":"System"}`.
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["scope"]["kind"], "System");
    }

    #[test]
    fn render_envelope_carries_event_version() {
        // Replay path round-trips the row's `event_version` value into the
        // envelope — assert that a non-default version survives serialization
        // unchanged. Today the kernel only ever writes `SYNC_EVENT_VERSION`,
        // but the replay path must not collapse to it; it has to surface
        // whatever the persisted row carried.
        let env = BroadcastEnvelope {
            id: 7,
            event_version: 99,
            actor: ActorId::User,
            scope: EventScope::System,
            event: Event::CoveUpdated(sample_cove()),
        };
        let s = render_envelope(&env).expect("render");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["eventVersion"], 99);
    }

    #[test]
    fn replay_complete_frame_shape() {
        let s = replay_complete_frame(1234);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["_id"], 1234);
        assert_eq!(v["eventVersion"], SYNC_EVENT_VERSION);
        assert_eq!(v["ev"], "_replay_complete");
        // No `data` field — the frame is purely a terminator.
        assert!(v.get("data").is_none());
    }

    #[test]
    fn snapshot_required_frame_shape() {
        let s = snapshot_required_frame(50000);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["_id"], 50000);
        assert_eq!(v["eventVersion"], SYNC_EVENT_VERSION);
        assert_eq!(v["ev"], "_snapshot_required");
        assert_eq!(v["data"]["earliest_id"], 50000);
    }

    #[test]
    fn sub_message_accepts_optional_since() {
        // Backward compat: pre-Scope-D clients omit `since` and must parse.
        let m: SubMessage = serde_json::from_str(r#"{"sub":["*"]}"#).expect("parse legacy");
        assert!(m.since.is_none());

        // New shape with cursor.
        let m: SubMessage =
            serde_json::from_str(r#"{"sub":["wave:w-1"], "since": 17}"#).expect("parse new");
        assert_eq!(m.since, Some(17));
        assert_eq!(m.sub, vec!["wave:w-1".to_string()]);
    }
}
