//! `GET /api/events` (WebSocket upgrade): topic subscriptions, optional cursor replay (`since`), and the
//! `_replay_complete` / `_snapshot_required` control frames. The broadcast subscription is established
//! before any replay so nothing is dropped at the replay→live boundary.

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

/// Wire shape of a client → server subscription message; a parse replaces the subscription set, `since` triggers replay.
#[derive(Debug, Deserialize)]
struct SubMessage {
    sub: Vec<String>,
    #[serde(default)]
    since: Option<i64>,
}

async fn handle(socket: WebSocket, state: AppState) {
    let (mut tx, mut rx) = socket.split();
    // Subscribe first, synchronously, with no awaited work before the select loop: the broadcast buffer is the
    // only thing preventing drops at the replay→live boundary, and for live-only clients the only delivery vehicle.
    let mut bus = state.events.subscribe();
    let mut subs: HashSet<String> = HashSet::new();
    // Largest replayed id while a replay is in flight; live events at or below it are duplicates. `0` = no replay.
    let mut last_replayed_id: i64 = 0;

    loop {
        tokio::select! {
            client = rx.next() => match client {
                Some(Ok(Message::Text(t))) => {
                    match serde_json::from_str::<SubMessage>(t.as_str()) {
                        Ok(msg) => {
                            subs = msg.sub.into_iter().collect();
                            tracing::debug!(count = subs.len(), since = ?msg.since, "ws /api/events: subs replaced");

                            if let Some(since) = msg.since {
                                match run_replay(
                                    &mut tx,
                                    state.repo.as_ref(),
                                    &subs,
                                    since,
                                    state.ws_replay_cap,
                                ).await {
                                    ReplayOutcome::Streamed(tip) => {
                                        last_replayed_id = tip;
                                    }
                                    ReplayOutcome::SnapshotRequired => {
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

            env = bus.recv() => match env {
                Ok(env) => {
                    if env.id != 0 && env.id <= last_replayed_id {
                        continue;
                    }
                    // Drop overlay events whose persisted `schemaVersion` exceeds this binary; filtered before the topic check
                    // so the warn fires regardless of who is subscribed.
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

enum ReplayOutcome {
    Streamed(i64),
    SnapshotRequired,
    ClientClosed,
}

/// Ceiling on rows a single replay may stream: a warm cursor over budget gets `_snapshot_required`; a cold
/// (`since == 0`) one has its anchor promoted to the log tip instead, since re-snapshotting it would loop forever.
const WS_REPLAY_MAX_EVENTS_ENV: &str = "NEIGE_WS_REPLAY_MAX_EVENTS";
const DEFAULT_WS_REPLAY_MAX_EVENTS: i64 = 10_000;

/// Resolved once per `AppState` construction, not per connection, so tests inject a cap without env mutation.
pub(crate) fn ws_replay_max_events_from_env() -> i64 {
    match std::env::var(WS_REPLAY_MAX_EVENTS_ENV) {
        Ok(raw) => match raw.trim().parse::<i64>() {
            Ok(n) if n > 0 => n,
            _ => DEFAULT_WS_REPLAY_MAX_EVENTS,
        },
        Err(_) => DEFAULT_WS_REPLAY_MAX_EVENTS,
    }
}

/// Stream the replay window for `since`, then send `_replay_complete`; the broadcast subscription already
/// exists, so concurrent live writes are buffered for the dedup pass.
async fn run_replay<S>(
    tx: &mut S,
    repo: &dyn RouteRepo,
    subs: &HashSet<String>,
    since: i64,
    cap: i64,
) -> ReplayOutcome
where
    S: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
    // Retention check: the head of the log is gone (`since < earliest - 1`), or the pruner deleted interior rows
    // above `since` (`since < watermark`). `since == 0` is always honored.
    let mut earliest_id: Option<i64> = None;
    if since > 0 {
        let earliest = match repo.events_earliest_id().await {
            Ok(earliest) => earliest,
            Err(e) => {
                tracing::error!(error = %e, "ws /api/events: events_earliest_id failed");
                // Fall through — better to attempt the replay than strand the client on a transient DB hiccup.
                None
            }
        };
        earliest_id = earliest;
        let watermark = match repo.events_prune_watermark().await {
            Ok(watermark) => watermark,
            Err(e) => {
                tracing::error!(error = %e, "ws /api/events: events_prune_watermark failed");
                0
            }
        };
        let head_pruned = matches!(earliest, Some(earliest) if since < earliest - 1);
        if head_pruned || since < watermark {
            let frame = snapshot_required_frame(earliest.unwrap_or(watermark));
            let _ = tx.send(Message::Text(frame.into())).await;
            return ReplayOutcome::SnapshotRequired;
        }
    }

    // Over-cap detection runs on the RAW row count: `events_since` silently drops unmappable rows, so its
    // length can sit under the cap while raw rows remain, and stamping the tip would ack rows never sent.
    // The probe's raw MAX(id) bounds the terminator stamp for the same reason.
    let mut anchor = since;
    let mut anchor_promoted = false;
    let raw_window_max = loop {
        let (raw_pending, raw_window_max) = match repo
            .events_raw_window_since(anchor, cap.saturating_add(1))
            .await
        {
            Ok(probe) => probe,
            Err(e) => {
                tracing::error!(error = %e, anchor, "ws /api/events: replay-window raw probe failed");
                // Degraded path: send the terminator at the anchor so the client stops waiting.
                let frame = replay_complete_frame(anchor);
                let _ = tx.send(Message::Text(frame.into())).await;
                return ReplayOutcome::Streamed(anchor);
            }
        };
        match replay_cap_route(anchor, raw_pending, cap, anchor_promoted) {
            CapRoute::Stream => break raw_window_max,
            CapRoute::Snapshot => {
                let earliest = earliest_id.unwrap_or(anchor);
                tracing::warn!(
                    since,
                    anchor,
                    cap,
                    "ws /api/events: replay window exceeds cap; forcing re-snapshot"
                );
                let frame = snapshot_required_frame(earliest);
                let _ = tx.send(Message::Text(frame.into())).await;
                return ReplayOutcome::SnapshotRequired;
            }
            CapRoute::PromoteAnchor => {
                // Read the tip here, at promotion time, so live-only and warm clients never pay this read. A read error
                // promotes to 0, which the next pass routes to `_snapshot_required` rather than spinning.
                let tip = match repo.events_latest_id().await {
                    Ok(t) => t.unwrap_or(0),
                    Err(e) => {
                        tracing::error!(error = %e, "ws /api/events: promotion tip read failed");
                        0
                    }
                };
                tracing::warn!(
                    cap,
                    promoted_anchor = tip,
                    "ws /api/events: cold replay window exceeds cap; promoting anchor to the request-time tip"
                );
                anchor = tip;
                anchor_promoted = true;
            }
        }
    };
    let rows = match repo.events_since(anchor, cap).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, anchor, "ws /api/events: events_since query failed");
            // Send the terminator at the anchor so the client stops waiting.
            let frame = replay_complete_frame(anchor);
            let _ = tx.send(Message::Text(frame.into())).await;
            return ReplayOutcome::Streamed(anchor);
        }
    };

    // Post-fetch watermark read, after `events_since` and before the first frame: a prune can commit between the
    // pre-fetch guard and the fetch; and the ack cursor may safely extend to the watermark (pruned ids are dead forever).
    let prune_watermark = match repo.events_prune_watermark().await {
        Ok(watermark) => watermark,
        Err(e) => {
            tracing::error!(
                error = %e,
                "ws /api/events: events_prune_watermark recheck failed"
            );
            // Inert floor on error — never strand a client on a transient DB hiccup.
            0
        }
    };
    if watermark_invalidates_window(since, prune_watermark) {
        tracing::warn!(
            since,
            watermark = prune_watermark,
            "ws /api/events: prune watermark advanced mid-replay; forcing re-snapshot"
        );
        let frame = snapshot_required_frame(earliest_id.unwrap_or(prune_watermark));
        let _ = tx.send(Message::Text(frame.into())).await;
        return ReplayOutcome::SnapshotRequired;
    }

    let mut last_id = anchor;
    for (id, event_version, scope, ev) in rows {
        // Drop overlay rows whose `schemaVersion` exceeds this binary, but still advance the cursor so the next
        // reconnect resumes past them.
        if should_skip_event_for_overlay_version(&ev) {
            last_id = id;
            continue;
        }
        // Topic filter applies to replayed frames too.
        if !event::topics(&ev).iter().any(|t| subs.contains(t)) {
            // Skipped, but still advance the cursor.
            last_id = id;
            continue;
        }
        // `events_since` does not return the `actor` column and the wire format omits it, so a `User` actor is
        // synthesized; this branch never feeds the recorder.
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

    // Terminator stamped with the server's log tip (so a client whose cursor is ahead of a reset log can detect it),
    // capped at `accounted_end` so rows committed during the replay stay above the dedup cursor and are delivered live.
    let server_tip = match repo.events_latest_id().await {
        Ok(Some(tip)) => tip,
        Ok(None) => 0,
        Err(e) => {
            tracing::error!(error = %e, "ws /api/events: events_latest_id failed");
            last_id
        }
    };
    // Re-read the watermark alongside `server_tip` so the stamp's floor and tip come from the same instant; a tail
    // prune between the two would otherwise fake a log-reset signal. On error fall back to the earlier (never-larger) value.
    let stamp_watermark = match repo.events_prune_watermark().await {
        Ok(watermark) => watermark,
        Err(e) => {
            tracing::error!(
                error = %e,
                "ws /api/events: events_prune_watermark stamp-floor read failed"
            );
            prune_watermark
        }
    };
    // Highest raw id this replay covered, including rows the deserialization pass dropped, floored at the anchor.
    let accounted_end = last_id.max(raw_window_max.unwrap_or(anchor));
    let stamp = replay_complete_stamp(server_tip, accounted_end, stamp_watermark);
    let frame = replay_complete_frame(stamp);
    if tx.send(Message::Text(frame.into())).await.is_err() {
        return ReplayOutcome::ClientClosed;
    }
    ReplayOutcome::Streamed(stamp)
}

/// Pure over-cap routing decision for one probe pass.
#[derive(Debug, PartialEq, Eq)]
enum CapRoute {
    Stream,
    /// Over cap on the cold first pass: promote the anchor to the log tip and re-probe.
    PromoteAnchor,
    /// Over cap with a positive anchor: the client must re-snapshot; no cursor is stamped.
    Snapshot,
}

fn replay_cap_route(anchor: i64, raw_pending: i64, cap: i64, anchor_promoted: bool) -> CapRoute {
    if raw_pending <= cap {
        CapRoute::Stream
    } else if anchor > 0 || anchor_promoted {
        // `anchor_promoted` covers a promotion that landed at 0 (failed tip read or empty log): re-promoting would spin.
        CapRoute::Snapshot
    } else {
        CapRoute::PromoteAnchor
    }
}

/// A warm window is invalid when the watermark advanced past `since` after the pre-fetch guard; cold windows are never invalidated.
fn watermark_invalidates_window(since: i64, watermark: i64) -> bool {
    since > 0 && since < watermark
}

/// The id `_replay_complete` carries: `min(server_tip, accounted_end).max(prune_watermark)`. The cap keeps rows
/// committed during the replay above the dedup cursor; a tip below `accounted_end` is the log-reset signal and
/// passes through; the watermark floor stops a tail prune from stranding the client in a `_snapshot_required` loop.
fn replay_complete_stamp(server_tip: i64, accounted_end: i64, prune_watermark: i64) -> i64 {
    server_tip.min(accounted_end).max(prune_watermark)
}

/// Hand-crafted so the control frame stays off the typed `Event` enum (ts-rs export); carries
/// `SYNC_EVENT_VERSION` so clients can treat `eventVersion` as load-bearing on every frame.
fn replay_complete_frame(stamp_id: i64) -> String {
    serde_json::json!({
        "_id": stamp_id,
        "eventVersion": SYNC_EVENT_VERSION,
        "ev": "_replay_complete",
    })
    .to_string()
}

/// Server-only control frame.
fn snapshot_required_frame(earliest: i64) -> String {
    serde_json::json!({
        "_id": earliest,
        "eventVersion": SYNC_EVENT_VERSION,
        "ev": "_snapshot_required",
        "data": { "earliest_id": earliest },
    })
    .to_string()
}

/// Splice `_id`, `eventVersion` and `scope` alongside the `Event` enum's own `{ev, data}` serialization, so
/// the ts-rs type for `Event` stays authoritative.
fn render_envelope(env: &BroadcastEnvelope) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(&env.event)?;
    if let serde_json::Value::Object(ref mut map) = value {
        map.insert("_id".to_string(), serde_json::Value::from(env.id));
        map.insert(
            "eventVersion".to_string(),
            serde_json::Value::from(env.event_version),
        );
        map.insert("scope".to_string(), serde_json::to_value(&env.scope)?);
    }
    serde_json::to_string(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use crate::model::{Area, AreaKind};

    fn sample_area() -> Area {
        Area {
            id: "c-1".into(),
            name: "n".into(),
            color: "#fff".into(),
            sort: 0.0,
            kind: AreaKind::User,
            default_template_id: None,
            default_cwd: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    // Decision matrix over the two pure seams; cells the seams cannot express are covered by the ws_replay integration suite.

    #[test]
    fn delivery_invariant_decision_matrix() {
        const CAP: i64 = 6;

        // Warm path (anchor > 0).
        assert_eq!(replay_cap_route(5, 4, CAP, false), CapRoute::Stream);
        assert_eq!(replay_cap_route(5, CAP + 1, CAP, false), CapRoute::Snapshot);

        // Commit before the probe: acked as streamed.
        let (id, accounted_end, tip) = (8, 10, 10);
        assert!(replay_complete_stamp(tip, accounted_end, 0) >= id);

        // Commit between probe and tip-read: buffered, so the stamp must stay below it.
        assert_eq!(replay_complete_stamp(12, 10, 0), 10);

        // Commit after tip-read: trivially above the cursor.
        assert!(13 > replay_complete_stamp(12, 12, 0));

        assert_eq!(replay_complete_stamp(10, 10, 0), 10);

        // Cold path (since == 0).
        assert_eq!(replay_cap_route(0, 4, CAP, false), CapRoute::Stream);
        // Over cap → promote, never a snapshot on the first pass (the client would loop at since=0).
        assert_eq!(
            replay_cap_route(0, CAP + 1, CAP, false),
            CapRoute::PromoteAnchor
        );

        // Commit before the promotion tip read: acked wholesale under the invalidate contract.
        let (backlog_id, promoted_anchor) = (9_000, 15_000);
        assert!(
            backlog_id <= promoted_anchor,
            "wholesale-acked by the promotion"
        );

        // Commit after the promotion tip read: streamed as a replay frame.
        assert_eq!(
            replay_cap_route(promoted_anchor, 1, CAP, true),
            CapRoute::Stream
        );
        let gap_accounted = 15_001; // gap read streamed the row
        assert!(replay_complete_stamp(15_001, gap_accounted, 0) >= 15_001);

        // Commit between gap probe and tip-read: buffered, stamp stays below it.
        assert_eq!(replay_complete_stamp(15_002, 15_001, 0), 15_001);

        // Promoted window itself over cap: escalate to snapshot rather than re-promoting (which could spin).
        assert_eq!(
            replay_cap_route(promoted_anchor, CAP + 1, CAP, true),
            CapRoute::Snapshot
        );
        assert_eq!(replay_cap_route(0, CAP + 1, CAP, true), CapRoute::Snapshot);

        // Live-only clients never enter `run_replay`; their contract is that `handle` subscribes synchronously with no awaited work first.

        // Reset detection: a tip below the accounted end passes through unclamped.
        assert_eq!(replay_complete_stamp(2, 5, 0), 2);
        assert_eq!(replay_complete_stamp(2, 10, 0), 2);
    }

    #[test]
    fn replay_stamp_caps_at_accounted_end_not_tip() {
        assert_eq!(replay_complete_stamp(12, 10, 0), 10);
    }

    #[test]
    fn replay_stamp_floors_at_prune_watermark() {
        // Tail prune: watermark above the live tip; the ack must reach the watermark or the reconnect re-snapshots forever.
        assert_eq!(replay_complete_stamp(3, 3, 4), 4);
        // Warm anchor at exactly the watermark: the floor suppresses a false reset signal.
        assert_eq!(replay_complete_stamp(3, 10, 10), 10);
        assert_eq!(replay_complete_stamp(12, 10, 4), 10);
        assert_eq!(replay_complete_stamp(2, 5, 0), 2);
    }

    #[test]
    fn replay_stamp_survives_tail_prune_between_tip_and_floor_reads() {
        // Tail prune between the tip read and the watermark read: the fresh floor holds the stamp at 10.
        assert_eq!(replay_complete_stamp(8, 10, 10), 10);
        // A true reset in the same shape still fires: the watermark is 0 post-reset.
        assert_eq!(replay_complete_stamp(8, 10, 0), 8);
    }

    #[test]
    fn watermark_recheck_invalidates_only_warm_windows_below_it() {
        assert!(watermark_invalidates_window(3, 4));
        assert!(watermark_invalidates_window(1, i64::MAX));
        assert!(!watermark_invalidates_window(4, 4));
        assert!(!watermark_invalidates_window(9, 4));
        // Cold windows are never invalidated; snapshot_required would loop them.
        assert!(!watermark_invalidates_window(0, i64::MAX));
    }

    #[test]
    fn replay_cap_route_never_snapshots_a_cold_first_pass() {
        // An unpromoted since=0 client must never be told to re-snapshot (infinite loop).
        for pending in [7, 100, i64::MAX] {
            assert_eq!(
                replay_cap_route(0, pending, 6, false),
                CapRoute::PromoteAnchor
            );
        }
    }

    #[test]
    fn render_envelope_has_id_and_keeps_event_shape() {
        let env = BroadcastEnvelope {
            id: 42,
            event_version: SYNC_EVENT_VERSION,
            actor: ActorId::User,
            scope: EventScope::Area { area: "c-1".into() },
            event: Event::AreaUpdated(sample_area()),
        };
        let s = render_envelope(&env).expect("render");
        // Key ordering on the wire is implementation-defined; parse by name.
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["_id"], 42);
        assert_eq!(v["eventVersion"], SYNC_EVENT_VERSION);
        assert_eq!(v["ev"], "area.updated");
        assert_eq!(v["data"]["id"], "c-1");
        assert_eq!(v["data"]["name"], "n");
        assert_eq!(v["scope"]["kind"], "Area");
        assert_eq!(v["scope"]["id"]["area"], "c-1");
    }

    #[test]
    fn render_envelope_keeps_zero_id() {
        // `id = 0` is the sentinel for "no persisted row yet" and must surface as `_id: 0`.
        let env = BroadcastEnvelope {
            id: 0,
            event_version: SYNC_EVENT_VERSION,
            actor: ActorId::Kernel,
            scope: EventScope::System,
            event: Event::AreaUpdated(sample_area()),
        };
        let s = render_envelope(&env).expect("render");
        assert!(s.contains(r#""_id":0"#), "got: {s}");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["scope"]["kind"], "System");
    }

    #[test]
    fn render_envelope_carries_event_version() {
        // The replay path must surface whatever `event_version` the row carried, not collapse to the constant.
        let env = BroadcastEnvelope {
            id: 7,
            event_version: 99,
            actor: ActorId::User,
            scope: EventScope::System,
            event: Event::AreaUpdated(sample_area()),
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
        // Backward compat: clients that omit `since` must parse.
        let m: SubMessage = serde_json::from_str(r#"{"sub":["*"]}"#).expect("parse legacy");
        assert!(m.since.is_none());

        let m: SubMessage =
            serde_json::from_str(r#"{"sub":["track:w-1"], "since": 17}"#).expect("parse new");
        assert_eq!(m.since, Some(17));
        assert_eq!(m.sub, vec!["track:w-1".to_string()]);
    }
}
