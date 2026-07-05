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
//!   over-cap replay normally never gets this frame — the client would
//!   reconnect at `since = 0` and loop; instead the replay anchor is
//!   promoted to the connection's pre-subscription tip snapshot (the
//!   backlog below it is skipped) and the remaining tail streams like an
//!   ordinary warm window. Only if even that promoted window is over cap
//!   (a post-connect flood) does the cold client get `_snapshot_required`
//!   and bounce once more.
//!
//! These frames stay out of the `Event` enum so they don't pollute the
//! ts-rs export — the client's `wireEventSchema` zod union doesn't
//! recognize them, so the client must extract `_replay_complete` /
//! `_snapshot_required` from the envelope **before** schema validation
//! runs.
//!
//! ## Delivery invariant (issue #854 / PR #867 review rounds 2–6)
//!
//! The replay window, the ack cursor, and the broadcast-subscription
//! buffer are assembled at different instants; rounds 2–6 were all
//! facets of orderings that let those three views disagree. The single
//! invariant every replay execution must satisfy:
//!
//! > For every `{sub, since}` replay that terminates in
//! > `_replay_complete` with cursor `C` (the frame's `_id`, installed as
//! > the connection's dedup cursor):
//! >
//! > 1. **Nothing acked-unsent.** Every event with `id <= C` was either
//! >    (a) streamed as a replay frame in this execution, (b) at or below
//! >    the replay *anchor* (already at the client, or wholesale-acked by
//! >    the cold-skip contract — see below), or (c) intentionally
//! >    dropped-with-cursor-advance by a documented filter (topic
//! >    mismatch, unsupported overlay `schemaVersion`, malformed /
//! >    unknown-kind row).
//! > 2. **Nothing unbuffered-unacked.** Every event with `id > C` was
//! >    committed *after* this connection's broadcast subscription was
//! >    established, so it is buffered/forwarded by the live branch and
//! >    survives the `env.id <= C` dedupe.
//! >
//! > The anchor is the client's `since`, EXCEPT on the over-cap cold
//! > skip, where it is promoted to the log tip read AT PROMOTION TIME
//! > (the request-time snapshot, round 6). The promotion is the skip's
//! > contract: rows at/below the promoted anchor are covered by the
//! > client's defensive full invalidate (REST re-reads), not by frames.
//! > `_snapshot_required` routes stamp no cursor and close the
//! > connection, satisfying the invariant vacuously. Clients that never
//! > send `since` never stamp a cursor either: for them clause 2
//! > degenerates to "everything committed after accept is buffered",
//! > which the accept-time ordering below guarantees.
//!
//! Ordering at accept (rounds 5–6): the subscription is established
//! FIRST, synchronously, and is the ONLY accept-time step — no awaited
//! work of any kind precedes the select loop. A live-only client (no
//! `since`) has no replay to recover events with, so an awaited DB read
//! before the subscribe would be an unbuffered loss window (round 5),
//! and one placed anywhere before its frames are processed would stall
//! its subscription behind SQLite and risk broadcast `Lagged` for a
//! snapshot only the replay path consumes (round 6). The tip snapshot
//! is therefore read lazily, inside `run_replay`, only when a cold
//! over-cap window actually promotes — live-only and warm clients pay
//! zero DB reads for it.
//!
//! Why the request-time promotion cannot ack a deliverable-but-unsent
//! row — the acked set decomposes into three kinds, all within the
//! contract:
//!
//! * rows committed before the since-bearing frame began processing,
//!   under a topic set that DID NOT match them (in particular the empty
//!   set of a cold first-frame client, whose `sub` and `since` arrive
//!   in the same message): never deliverable by the live path at all —
//!   only a replay frame could have carried them, and the skip's
//!   invalidate contract is exactly the replacement for those frames;
//! * rows committed while an EARLIER subscription matched them (the
//!   re-anchor case — a live-only `{sub}` frame followed later by a
//!   `{sub, since}` frame): already delivered by the live branch before
//!   the replay began, so acking them prevents a duplicate rather than
//!   causing a loss;
//! * the in-flight sliver: rows committing during the replay request's
//!   own bounded queries (probe + promotion read, single-digit
//!   milliseconds) that land at/below the returned tip. Buffered but
//!   acked — accepted as part of the skip's backlog by construction;
//!   every earlier placement of the snapshot had the same-magnitude
//!   sliver around its own read.
//!
//! Rows above the promoted anchor need no carve-out: the `(anchor, tip]`
//! gap streams through the ordinary bounded path and everything later
//! flows live.
//!
//! Why the streaming path satisfies it constructively: ids are
//! append-only monotonic. The raw window probe runs *after* the
//! subscription exists, so when the window fits the cap it covers every
//! row above the anchor that predates the subscription (those rows are
//! not buffered — they MUST be in the window, and are). The bounded read
//! then consumes that whole window, and the cursor is
//! `min(events_latest_id(), accounted_end)` (see
//! [`replay_complete_stamp`]): the `accounted_end` cap keeps rows that
//! committed after the probe (which ARE buffered) above `C`, and the
//! `min` passes a regressed tip (issue #290 `/dev/reset` detection)
//! through unclamped. The cold skip reuses this exact machinery — anchor
//! promotion, then the gap streams as an ordinary window.
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
    // Always subscribe up-front — UNCONDITIONALLY FIRST, and as the ONLY
    // accept-time step: no awaited work of any kind precedes the select
    // loop (PR #867 rounds 5–6). Any live event emitted between now and
    // the first `{sub, since}` is buffered in the broadcast channel
    // rather than dropped. The design's "subscribe-first" pattern (§2.2)
    // is the *only* mechanism that prevents drops at the replay→live
    // boundary, and for a documented live-only client (no `since`, no
    // replay) the buffer is the ONLY delivery vehicle: an awaited DB
    // read placed anywhere before its frames are processed would (a)
    // open an unbuffered window whose events such a client can never
    // recover (round 5) and (b) stall its subscription processing behind
    // SQLite — risking broadcast-buffer `Lagged` under load — for a
    // snapshot only the replay path consumes (round 6). The over-cap
    // cold skip therefore reads its tip snapshot lazily, at promotion
    // time inside `run_replay`; live-only clients trigger zero DB work.
    // `subscribe()` is synchronous, so no event can slip in between
    // socket accept and the receiver existing.
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
///     replay anchor is PROMOTED to the log tip read at promotion time
///     (request-time snapshot, round 6): the backlog at or below it is
///     acked wholesale (the client's terminator handler runs a defensive
///     batch invalidate and its REST reads are fresh, so no state is
///     lost) and the remaining `(anchor, tip]` gap streams like an
///     ordinary warm window — see the module's "Delivery invariant"
///     section. Sending `_snapshot_required` here instead would loop
///     forever — the client's response is "clear cursor, reconnect at
///     since=0". Only a promoted window that is ITSELF over cap (rows
///     still flooding in between the promotion read and the re-probe,
///     or a failed promotion read) escalates to `_snapshot_required`.
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
/// `_replay_complete` synthetic envelope. Returns the new dedup cursor
/// (the server's `events.id` tip capped at the window this replay
/// accounted for — see [`replay_complete_stamp`]), or signals
/// `_snapshot_required` if `since` predates the retention horizon or the
/// pending window exceeds the replay cap `cap` (env-derived on the
/// `AppState`; see [`WS_REPLAY_MAX_EVENTS_ENV`] for the over-cap routing
/// table). The over-cap cold skip reads its tip snapshot lazily, at
/// promotion time inside this function (request-time snapshot, round 6)
/// — the anchor the skip promotes to (module doc, "Delivery invariant").
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
) -> ReplayOutcome
where
    S: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
    // Retention check, two independent triggers. `since == 0` is always
    // honored (the client deliberately wants "everything") even when the
    // table is empty.
    //
    //   1. `since < earliest - 1` — the head of the log is gone (rows
    //      between `since + 1` and `earliest - 1` no longer exist).
    //      `since == earliest - 1` is the happy case: the next id we'd
    //      send is exactly `earliest`, no gap.
    //   2. `since < watermark` — the events pruner (#854 slice 2) deletes
    //      INTERIOR rows too, and structural events are permanent, so
    //      `MIN(id)` never advances past the first structural row. The
    //      durable retention watermark is the highest id ever pruned; a
    //      cursor below it may have pruned rows anywhere in
    //      `(since, watermark]`, so a contiguous replay can't be promised.
    //
    // Either way the client must throw away its cache and refetch. The
    // watermark is re-read after the window is materialized, before any
    // frame streams — see the recheck below the `events_since` read.
    let mut earliest_id: Option<i64> = None;
    if since > 0 {
        let earliest = match repo.events_earliest_id().await {
            Ok(earliest) => earliest,
            Err(e) => {
                tracing::error!(error = %e, "ws /api/events: events_earliest_id failed");
                // Fall through — better to attempt the replay than to
                // strand the client over a transient DB hiccup.
                None
            }
        };
        earliest_id = earliest;
        let watermark = match repo.events_prune_watermark().await {
            Ok(watermark) => watermark,
            Err(e) => {
                tracing::error!(error = %e, "ws /api/events: events_prune_watermark failed");
                // Same fall-through rationale as `events_earliest_id`.
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
    //
    // Over-cap routing is the pure `replay_cap_route` decision (see the
    // module's "Delivery invariant" section):
    //
    //   * warm (`anchor > 0`) over-cap → `_snapshot_required`, no cursor.
    //   * cold (`anchor == 0`) over-cap → the anchor is PROMOTED to the
    //     log tip read at promotion time (the request-time snapshot,
    //     round 6) and the probe re-runs. Rows at/below the promoted
    //     anchor are acked wholesale under the skip contract (client
    //     refetches over REST after the defensive invalidate) — the
    //     invariant section derives why none of them was a
    //     deliverable-but-unsent row. The remaining `(anchor, tip]` gap
    //     then streams through this same bounded path as an ordinary
    //     warm window, so nothing above the final cursor is unbuffered
    //     and nothing below it is unsent.
    //   * a promoted anchor that is STILL over cap (rows flooding in
    //     between the promotion read and the re-probe, or a failed
    //     promotion read that degraded to 0) → `_snapshot_required`;
    //     the skip cannot help and the client must bounce cold once
    //     more.
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
                // Degraded path, mirrors the events_since error branch
                // below: send the terminator at the anchor so the client
                // stops waiting. A promoted anchor still satisfies the
                // invariant here — everything at/below it is covered by
                // the terminator-triggered defensive invalidate,
                // everything above it stays un-acked.
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
                // Request-time tip snapshot (round 6): read HERE, at
                // promotion time, and nowhere earlier — live-only and
                // warm clients never pay this read, and the accept
                // sequence stays free of awaited DB work (see `handle`).
                // Everything at/below this tip is acked wholesale under
                // the skip contract; the module's "Delivery invariant"
                // section derives why that ack can never swallow a
                // deliverable-but-unsent row. A read error promotes to 0,
                // which the next pass routes to `_snapshot_required`
                // (bounce, stamp nothing) rather than spinning.
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
            // Send the control frame with the highest id we have (the
            // anchor) so the client at least sees a terminator and stops
            // waiting. The next live broadcast will keep things moving.
            let frame = replay_complete_frame(anchor);
            let _ = tx.send(Message::Text(frame.into())).await;
            return ReplayOutcome::Streamed(anchor);
        }
    };

    let mut last_id = anchor;
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
    // delivers it. Capping keeps concurrently committed rows strictly
    // above the cursor while leaving the #290 regression signal intact
    // (a regressed tip is *below* the cap, so `min` passes it through).
    // See `replay_complete_stamp` and the module's "Delivery invariant"
    // section.
    //
    // Falling back to `last_id` (which equals the anchor when zero rows
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
    // `accounted_end` is the highest raw id this replay covered: the raw
    // end of the probed window (which the bounded read fully consumed —
    // `raw_pending <= cap` — including trailing rows the deserialization
    // pass dropped) joined with the highest row the read itself produced,
    // floored at the anchor. On a promoted (cold-skip) anchor this
    // extends the ack over the wholesale-skipped backlog at/below the
    // request-time tip exactly as the invariant's carve-out allows.
    let accounted_end = last_id.max(raw_window_max.unwrap_or(anchor));
    let stamp = replay_complete_stamp(server_tip, accounted_end);
    let frame = replay_complete_frame(stamp);
    if tx.send(Message::Text(frame.into())).await.is_err() {
        return ReplayOutcome::ClientClosed;
    }
    ReplayOutcome::Streamed(stamp)
}

/// Pure over-cap routing decision for one probe pass over the window
/// `(anchor, anchor + …]` (see the module's "Delivery invariant" section
/// and the routing table on [`WS_REPLAY_MAX_EVENTS_ENV`]).
#[derive(Debug, PartialEq, Eq)]
enum CapRoute {
    /// Window fits the budget — stream it from the current anchor.
    Stream,
    /// Over cap on the cold first pass (`anchor == 0`): promote the
    /// anchor to the log tip read at promotion time (request-time
    /// snapshot) and re-probe. The skipped backlog is acked under the
    /// invalidate contract; the remaining gap streams like a warm
    /// window.
    PromoteAnchor,
    /// Over cap with a positive anchor (a warm cursor, or an
    /// already-promoted cold anchor still facing a flood): the client
    /// must re-snapshot. No cursor is stamped on this route, so the
    /// delivery invariant holds vacuously — the connection closes and
    /// the client reconnects cold.
    Snapshot,
}

fn replay_cap_route(anchor: i64, raw_pending: i64, cap: i64, anchor_promoted: bool) -> CapRoute {
    if raw_pending <= cap {
        CapRoute::Stream
    } else if anchor > 0 || anchor_promoted {
        // `anchor_promoted` matters when the promotion landed at 0 (the
        // promotion tip read failed, or the log was empty at read time
        // with a flood committing right after): re-promoting would spin.
        CapRoute::Snapshot
    } else {
        CapRoute::PromoteAnchor
    }
}

/// Decide the id `_replay_complete` carries (also the connection's dedup
/// cursor): `min(server_tip, accounted_end)`. Pure so the decision table
/// is unit-testable.
///
///   * A tip ABOVE what this replay accounted for means rows committed
///     between the raw window probe and the `events_latest_id()` read
///     (PR #867 round-2 review). Stamping the tip would let the dedup
///     pass drop those rows' buffered broadcasts as
///     `env.id <= last_replayed_id`, acking events that were never
///     delivered — live-only consumers (hook/phase listeners) would miss
///     them permanently. Capping keeps them strictly above the cursor so
///     the live-forward branch delivers them.
///   * A tip BELOW `accounted_end` is the #290 log-regression signal
///     (`/dev/reset` restarted ids) and must pass through unclamped so
///     the client can detect it — on a promoted (cold-skip) anchor too
///     (a reset between the promotion read and the final tip read
///     shrinks the tip below the promoted anchor).
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
    // Delivery-invariant decision matrix (PR #867 rounds 2–4; see the
    // module doc). Two pure seams carry the whole decision:
    //
    //   * `replay_cap_route(anchor, raw_pending, cap, promoted)` — which
    //     path a probe pass takes (stream / promote-anchor / snapshot);
    //   * `replay_complete_stamp(server_tip, accounted_end)` — where the
    //     ack cursor lands relative to a concurrently committed row.
    //
    // The seams exist exactly so the commit-timing races are pinnable
    // without a mid-`run_replay` write hook. The matrix below enumerates
    // commit timing × path; cells the pure seams can't express (actual
    // frame delivery) are covered by the ws_replay integration suite —
    // each cell names its integration counterpart where one exists.
    // -----------------------------------------------------------------

    #[test]
    fn delivery_invariant_decision_matrix() {
        const CAP: i64 = 6;

        // ---- WARM path (anchor = client's since > 0) -----------------
        // Window fits → stream. [integration:
        // subscribe_with_since_mid_replays_only_newer]
        assert_eq!(replay_cap_route(5, 4, CAP, false), CapRoute::Stream);
        // Over cap → snapshot; no cursor is stamped (invariant vacuous).
        // [integration: stale_cursor_over_cap_gets_snapshot_required]
        assert_eq!(replay_cap_route(5, CAP + 1, CAP, false), CapRoute::Snapshot);

        // warm, commit BEFORE the probe: the row is inside the window
        // (probe covers everything above the anchor when under cap), so
        // accounted_end >= id and the stamp acks it as streamed.
        // [integration: subscribe_with_since_zero_replays_all]
        let (id, accounted_end, tip) = (8, 10, 10);
        assert!(replay_complete_stamp(tip, accounted_end) >= id);

        // warm, commit BETWEEN probe and tip-read: id (11..=12) is above
        // the accounted window (10) but below the tip (12). It IS
        // buffered (the subscription predates the probe), so the stamp
        // must stay below it → delivered live, exactly once. Round 2.
        // [integration: replay_then_live_no_drop_no_dupe]
        assert_eq!(replay_complete_stamp(12, 10), 10);

        // warm, commit AFTER tip-read: id > tip >= stamp — trivially
        // above the cursor, delivered live. [integration:
        // cold_replay_over_cap_skips_to_tip live tail]
        assert!(13 > replay_complete_stamp(12, 12));

        // warm, fully caught up: tip == accounted end, ack everything.
        assert_eq!(replay_complete_stamp(10, 10), 10);

        // ---- COLD path (since == 0) ----------------------------------
        // Under cap → plain stream, identical to warm with anchor 0.
        assert_eq!(replay_cap_route(0, 4, CAP, false), CapRoute::Stream);
        // Over cap → promote the anchor to the request-time tip (the
        // skip). Never a snapshot on the first pass: the client would
        // loop at since=0.
        assert_eq!(
            replay_cap_route(0, CAP + 1, CAP, false),
            CapRoute::PromoteAnchor
        );

        // cold-skip, commit BEFORE the promotion tip read (round 6:
        // request-time snapshot). This is the 15k backlog PLUS any row
        // committed up to the read — id <= promoted anchor → acked
        // wholesale under the invalidate contract. The module doc's
        // decomposition shows why no such row was deliverable-but-unsent:
        // either no matching topic set existed while its broadcast was
        // consumed (a cold first-frame client's set is empty until the
        // {sub, since} message — the row was never deliverable live), or
        // an earlier subscription already delivered it live (re-anchor
        // case — the ack prevents a duplicate). Round-5 note: the former
        // "between snapshot and subscribe" cell is gone — the snapshot
        // no longer exists at accept time at all. [integration:
        // cold_replay_over_cap_skips_to_tip (never-deliverable),
        // cold_skip_folds_pre_sub_frame_commit_into_the_acked_backlog
        // (never-deliverable), cold_skip_acks_live_delivered_row_without_duplicate
        // (already-delivered)]
        let (backlog_id, promoted_anchor) = (9_000, 15_000);
        assert!(
            backlog_id <= promoted_anchor,
            "wholesale-acked by the promotion"
        );

        // cold-skip, commit AFTER the promotion tip read: id 15_001 is
        // above the promoted anchor — the gap probe from the anchor
        // covers it (gap under cap → Stream) and it is STREAMED as a
        // replay frame; its buffered broadcast dedupes below the stamp.
        assert_eq!(
            replay_cap_route(promoted_anchor, 1, CAP, true),
            CapRoute::Stream
        );
        let gap_accounted = 15_001; // gap read streamed the row
        assert!(replay_complete_stamp(15_001, gap_accounted) >= 15_001);

        // cold-skip, commit BETWEEN gap probe and tip-read (the in-flight
        // sliver's outer edge): above the accounted gap (15_001), below
        // the tip (15_002); buffered → stamp stays below it, delivered
        // live. Round 2 logic on the promoted window.
        assert_eq!(replay_complete_stamp(15_002, 15_001), 15_001);

        // cold-skip, promoted window ITSELF over cap (rows still
        // flooding in between the promotion read and the re-probe, or a
        // failed promotion read that degraded to anchor 0): escalate to
        // snapshot rather than re-promoting (which could spin).
        assert_eq!(
            replay_cap_route(promoted_anchor, CAP + 1, CAP, true),
            CapRoute::Snapshot
        );
        assert_eq!(replay_cap_route(0, CAP + 1, CAP, true), CapRoute::Snapshot);

        // ---- LIVE-ONLY column (no `since`, rounds 5–6) ---------------
        // A documented live-only client never enters `run_replay`: no
        // probe, no promotion, no cursor stamp (`last_replayed_id` stays
        // 0, so `replay_cap_route` / `replay_complete_stamp` are simply
        // never consulted — nothing for the pure seams to assert). Its
        // entire delivery contract is clause 2 of the invariant with
        // C = 0: everything committed after accept must be buffered,
        // which holds because `handle` establishes the subscription
        // synchronously as the ONLY accept-time step. Round-6 Lagged
        // hardening is structural: the tip snapshot lives inside
        // `run_replay`'s promotion arm and NOWHERE on the accept or
        // frame-dispatch path, so a live-only client's subscription
        // processing can never stall behind an awaited DB read it does
        // not need (SQLite lock at connect + >1024 buffered broadcasts
        // → `Lagged` → close, with no replay cursor to recover). The
        // seam-level pin: `run_replay` is the sole caller of the
        // promotion read, and `handle` contains no awaited work before
        // the select loop — asserted by code shape (see the `handle`
        // accept comment) and exercised by [integration:
        // live_only_client_receives_first_post_connect_commit,
        // subscribe_without_since_only_live].

        // ---- #290 reset detection (both paths) -----------------------
        // A tip BELOW the accounted end is the log-regression signal and
        // passes through unclamped — warm anchor (accounted 5) and
        // promoted anchor (accounted 10) alike. [integration:
        // replay_complete_id_reflects_server_tip_after_reset]
        assert_eq!(replay_complete_stamp(2, 5), 2);
        assert_eq!(replay_complete_stamp(2, 10), 2);
    }

    #[test]
    fn replay_stamp_caps_at_accounted_end_not_tip() {
        // Round-2 headline case, kept as a named pin: concurrent commit
        // during replay must stay above the ack cursor.
        assert_eq!(replay_complete_stamp(12, 10), 10);
    }

    #[test]
    fn replay_cap_route_never_snapshots_a_cold_first_pass() {
        // The one absolute of the routing table: an unpromoted since=0
        // client must never be told to re-snapshot (its response is
        // "reconnect at since=0" — an infinite loop). Any over-cap cold
        // first pass promotes instead.
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
