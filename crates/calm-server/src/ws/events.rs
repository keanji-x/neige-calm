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
//!   reset and re-bootstrap its cache. Issue #290. One qualifier: the
//!   tip is capped at the highest raw id the replay window accounted
//!   for, so rows committed concurrently with the replay stay above the
//!   dedup cursor and arrive via live forwarding instead of being
//!   silently skipped — see `replay_complete_stamp`.
//! * `_snapshot_required` is sent when the client's `since` cursor
//!   predates the retention horizon (the smallest live `events.id`), or
//!   when a `since > 0` replay window exceeds the replay cap
//!   (`NEIGE_WS_REPLAY_MAX_EVENTS`, issue #854). After sending it, the
//!   server closes the connection. The client must clear its persisted
//!   query cache (`qc.clear()`) and reconnect cold. A cold (`since = 0`)
//!   over-cap replay never gets this frame — the client would reconnect
//!   at `since = 0` and loop; instead the backlog is skipped and
//!   `_replay_complete` lands at the tip snapshot taken before the
//!   connection's broadcast subscription (so post-subscribe commits stay
//!   above the dedup cursor and arrive via live forwarding).
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
    // Snapshot the log tip BEFORE subscribing (PR #867 round-3 review).
    // The over-cap cold-skip path acks a replay window it never streamed;
    // its ack must not cover any broadcast the subscription below will
    // buffer, or the dedup pass drops that buffered frame and live-only
    // consumers (hook/phase listeners) miss the event entirely. Every
    // buffered broadcast is committed after the subscribe, hence has
    // `id > conn_tip` — so `conn_tip` is the highest id the cold skip may
    // safely ack. A read error degrades to `0` ("ack nothing beyond the
    // client's own cursor"), which can over-forward but never
    // ack-without-delivery.
    let conn_tip = match state.repo.events_latest_id().await {
        Ok(tip) => tip.unwrap_or(0),
        Err(e) => {
            tracing::error!(error = %e, "ws /api/events: pre-subscribe tip snapshot failed");
            0
        }
    };
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
                                    state.ws_replay_cap,
                                    conn_tip,
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

/// Ceiling on the number of rows a single replay may stream (issue #854).
/// The events table is unbounded (214k rows / 1.7 GB observed in prod), so
/// a replay past this budget doesn't stream — it re-routes:
///
///   * `since > 0` — the cursor is too far behind to backfill within
///     budget; send `_snapshot_required` so the client drops its cache,
///     refetches over REST, and reconnects cold.
///   * `since == 0` — a cold client has no cache to invalidate, so the
///     backlog is skipped entirely: send `_replay_complete` at the
///     connection's pre-subscription tip snapshot and go live-forward
///     (post-subscribe commits stay above the dedup cursor — see
///     `replay_complete_stamp`). The client's terminator handler runs a
///     defensive batch invalidate, and its REST reads are fresh, so no
///     state is lost. Sending `_snapshot_required` here instead would
///     loop forever — the client's response is "clear cursor, reconnect
///     at since=0".
const WS_REPLAY_MAX_EVENTS_ENV: &str = "NEIGE_WS_REPLAY_MAX_EVENTS";
const DEFAULT_WS_REPLAY_MAX_EVENTS: i64 = 10_000;

/// Resolve the replay cap from `NEIGE_WS_REPLAY_MAX_EVENTS`. Called once
/// per `AppState` construction (`BootState::into_app_state`) — NOT per
/// connection — so tests can inject a small cap via
/// `AppState::with_ws_replay_cap` instead of racing sibling tests on
/// process-global env mutation.
pub(crate) fn ws_replay_max_events_from_env() -> i64 {
    match std::env::var(WS_REPLAY_MAX_EVENTS_ENV) {
        Ok(raw) => match raw.trim().parse::<i64>() {
            Ok(n) if n > 0 => n,
            _ => DEFAULT_WS_REPLAY_MAX_EVENTS,
        },
        Err(_) => DEFAULT_WS_REPLAY_MAX_EVENTS,
    }
}

/// Stream the replay window for `since` to the client, then send the
/// `_replay_complete` synthetic envelope. Returns the server's actual
/// `events.id` tip (the new dedup cursor — `MAX(id)` of the live log,
/// queried after the in-window scan completes; falls back to the
/// in-window high-water mark if that query errors), or signals
/// `_snapshot_required` if `since` predates the retention horizon or the
/// pending window exceeds the replay cap `cap` (env-derived on the
/// `AppState`; see [`WS_REPLAY_MAX_EVENTS_ENV`] for the over-cap routing
/// table).
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
    cap: i64,
    conn_tip: i64,
) -> ReplayOutcome
where
    S: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
    // Retention check: if `since` predates the smallest surviving id, the
    // server can't honor a contiguous replay. Tell the client to throw
    // away its cache and refetch. `since == 0` is always honored (the
    // client deliberately wants "everything") even when the table is empty.
    let mut earliest_id: Option<i64> = None;
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
            Ok(found) => earliest_id = found,
            Err(e) => {
                tracing::error!(error = %e, "ws /api/events: events_earliest_id failed");
                // Fall through — better to attempt the replay than to
                // strand the client over a transient DB hiccup.
            }
        }
    }

    // Over-cap detection runs on the RAW row count, NOT on the length of
    // the `events_since` result (PR #867 review finding): `events_since`
    // silently drops malformed-payload / unknown-kind rows while mapping,
    // so its filtered length can sit at/below the cap while more raw rows
    // remain in the window. Deciding on the filtered length would stream
    // the surviving page and then stamp `_replay_complete` at the server
    // tip — permanently advancing the client past events that were never
    // sent. The probe is bounded (`LIMIT cap+1` id-only subquery), so
    // "over budget" stays detectable without a full-table COUNT.
    //
    // The probe also returns the raw MAX(id) of the window — the highest
    // row this replay is about to account for. That bounds the terminator
    // stamp below (PR #867 round-2 review): rows committed between this
    // probe and the later `events_latest_id()` call must NOT be covered by
    // the returned dedup cursor, or the buffered live broadcast carrying
    // them would be dropped and the client would advance past events that
    // were never sent.
    let (raw_pending, raw_window_max) = match repo
        .events_raw_window_since(since, cap.saturating_add(1))
        .await
    {
        Ok(probe) => probe,
        Err(e) => {
            tracing::error!(error = %e, since, "ws /api/events: replay-window raw probe failed");
            // Degraded path, mirrors the events_since error branch below:
            // send the terminator at `since` so the client stops waiting
            // and keeps its cursor; the next reconnect retries the replay.
            let frame = replay_complete_frame(since);
            let _ = tx.send(Message::Text(frame.into())).await;
            return ReplayOutcome::Streamed(since);
        }
    };
    if raw_pending > cap && since > 0 {
        let earliest = earliest_id.unwrap_or(since);
        tracing::warn!(
            since,
            cap,
            "ws /api/events: replay window exceeds cap; forcing re-snapshot"
        );
        let frame = snapshot_required_frame(earliest);
        let _ = tx.send(Message::Text(frame.into())).await;
        return ReplayOutcome::SnapshotRequired;
    }
    let skipped_backlog = raw_pending > cap;
    let rows = if skipped_backlog {
        // since == 0: cold client, over-cap backlog — skip straight to the
        // tip (see the routing table on WS_REPLAY_MAX_EVENTS_ENV). The
        // skip accounts for everything up to `conn_tip`, the tip snapshot
        // taken BEFORE this connection's broadcast subscription: those
        // rows are covered by the client's defensive full invalidate +
        // fresh REST reads. Rows past `conn_tip` were committed after the
        // subscribe and sit in the broadcast buffer — the terminator
        // below must NOT ack them (PR #867 round-3 review), so the
        // live-forward branch delivers them to stream-only consumers.
        tracing::warn!(
            cap,
            "ws /api/events: cold replay window exceeds cap; skipping backlog to server tip"
        );
        Vec::new()
    } else {
        match repo.events_since(since, cap).await {
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
    // delivered. Stamped with the SERVER'S log tip — not the highest id
    // returned by the in-window scan — so a client whose persisted cursor
    // is *ahead* of the server's tip (e.g. the dev `/dev/reset` path wipes
    // `sqlite_sequence`, so re-seeded events restart at id=1) sees
    // `_replay_complete._id < lastEventId` and can re-bootstrap. Issue
    // #290.
    //
    // ...capped at `accounted_end` (PR #867 round-2 review): a row can
    // commit between the raw window probe and this `events_latest_id()`
    // call. Such a row was never streamed, but it IS buffered on the
    // broadcast subscription (subscribe-first ordering) — as long as the
    // returned dedup cursor stays BELOW its id, the live-forward branch
    // delivers it. `accounted_end` is the highest raw id this replay
    // actually covered: the raw end of the probed window (which the
    // bounded read fully consumed, `raw_pending <= cap`, including any
    // trailing rows the deserialization pass dropped) joined with the
    // highest row the read itself produced. Capping keeps concurrently
    // committed rows strictly above the cursor while leaving the #290
    // regression signal intact (a regressed tip is *below* the cap, so
    // `min` passes it through). See `replay_complete_stamp`.
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
    let accounted_end = if skipped_backlog {
        // Cold skip: nothing streamed; everything up to the connection's
        // pre-subscription tip snapshot is covered by the client's
        // defensive invalidate. Anything above it is a post-subscribe
        // commit sitting in the broadcast buffer — do not ack it.
        conn_tip
    } else {
        last_id.max(raw_window_max.unwrap_or(since))
    };
    let stamp = replay_complete_stamp(server_tip, accounted_end);
    let frame = replay_complete_frame(stamp);
    if tx.send(Message::Text(frame.into())).await.is_err() {
        return ReplayOutcome::ClientClosed;
    }
    ReplayOutcome::Streamed(stamp)
}

/// Decide the id `_replay_complete` carries (also the connection's dedup
/// cursor): `min(server_tip, accounted_end)`. Pure so the decision table
/// is unit-testable.
///
///   * A tip ABOVE what this replay accounted for means rows committed
///     concurrently with the replay — between the raw window probe and
///     the `events_latest_id()` read on the streaming path (PR #867
///     round-2 review), or between the connection's broadcast subscribe
///     and that read on the over-cap cold-skip path (round-3 review,
///     where `accounted_end` is the pre-subscription tip snapshot).
///     Stamping the tip would let the dedup pass drop those rows'
///     buffered broadcasts as `env.id <= last_replayed_id`, acking events
///     that were never delivered — live-only consumers (hook/phase
///     listeners) would miss them permanently. Capping keeps them
///     strictly above the cursor so the live-forward branch delivers
///     them.
///   * A tip BELOW `accounted_end` is the #290 log-regression signal
///     (`/dev/reset` restarted ids) and must pass through unclamped so
///     the client can detect it — on the skip path too (a reset between
///     connection open and the replay shrinks the tip below `conn_tip`).
fn replay_complete_stamp(server_tip: i64, accounted_end: i64) -> i64 {
    server_tip.min(accounted_end)
}

/// `{ "_id": <id>, "eventVersion": <n>, "ev": "_replay_complete" }`.
/// Hand-crafted to keep the control frame off the typed `Event` enum
/// (which ts-rs exports — adding underscore-prefixed variants would muddy
/// the client's discriminated union for no win).
///
/// `stamp_id` is the server's `events.id` tip (`MAX(id)`) capped at the
/// highest raw id the replay accounted for — see [`replay_complete_stamp`]
/// for the decision table. Issue #290's reset detection relies on the
/// frame carrying the *server's* view of "how far the log goes" so a
/// stale-cursor client can compare; the cap only bites when rows commit
/// concurrently with the replay (PR #867 round-2 review), in which case
/// those rows arrive via the live-forward branch instead.
///
/// Control frames are kernel-emitted and carry `SYNC_EVENT_VERSION` for
/// shape consistency with persisted-event frames — clients can treat
/// `eventVersion` as load-bearing on every frame they receive, not "only
/// on the replayed ones". They don't sit in the `events` table, so they
/// don't have a row-level version to round-trip; the constant is the
/// right source.
fn replay_complete_frame(stamp_id: i64) -> String {
    serde_json::json!({
        "_id": stamp_id,
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

    // -----------------------------------------------------------------
    // `replay_complete_stamp` decision table (PR #867 round-2 + round-3
    // reviews). The pure seam exists exactly so these races are pinnable
    // without a mid-`run_replay` write hook: a row committing between
    // the window probe (streaming path) or the broadcast subscribe
    // (cold-skip path) and the `events_latest_id()` read pushes the tip
    // ABOVE what the replay accounted for, and the stamp must not cover
    // it — the buffered broadcast is that row's only delivery vehicle.
    // -----------------------------------------------------------------

    #[test]
    fn replay_stamp_caps_at_accounted_end_not_tip() {
        // Concurrent commit during replay: probe/read accounted through
        // id 10, but a row (id 11..12) landed before the tip read. Stamp
        // 10 so the buffered broadcast for 11/12 survives the dedup pass
        // (`env.id <= last_replayed_id` drops) and reaches the client.
        assert_eq!(replay_complete_stamp(12, 10), 10);
    }

    #[test]
    fn replay_stamp_uses_tip_when_fully_caught_up() {
        // No concurrent writes: the tip IS the accounted end.
        assert_eq!(replay_complete_stamp(10, 10), 10);
    }

    #[test]
    fn replay_stamp_passes_regressed_tip_through_for_reset_detection() {
        // Issue #290: after /dev/reset the log restarts at id 1, so the
        // tip sits BELOW a stale client cursor (accounted_end == since
        // when zero rows matched). The regression signal must reach the
        // client unclamped.
        assert_eq!(replay_complete_stamp(2, 5), 2);
    }

    #[test]
    fn replay_stamp_cold_skip_does_not_ack_events_buffered_after_subscribe() {
        // Round-3 review: over-cap cold skip with a row committed between
        // this connection's broadcast subscribe and the post-hoc tip read
        // (tip 15_000 > pre-subscription snapshot 14_997). The skip path
        // passes `accounted_end = conn_tip`; the stamp must stay at the
        // snapshot so ids 14_998..15_000 — which exist ONLY in the
        // broadcast buffer — survive the dedup pass and reach live-only
        // consumers instead of being acked-without-delivery.
        assert_eq!(replay_complete_stamp(15_000, 14_997), 14_997);
    }

    #[test]
    fn replay_stamp_cold_skip_at_snapshot_acks_full_backlog() {
        // No post-subscribe commits: the skip acks exactly the tip it
        // snapshotted before subscribing (the whole skipped backlog).
        assert_eq!(replay_complete_stamp(15_000, 15_000), 15_000);
    }

    #[test]
    fn replay_stamp_cold_skip_passes_mid_connection_reset_through() {
        // A /dev/reset between connection open (conn_tip 10) and the
        // replay shrinks the tip to 2 — the #290 regression signal must
        // pass through unclamped on the skip path too.
        assert_eq!(replay_complete_stamp(2, 10), 2);
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
