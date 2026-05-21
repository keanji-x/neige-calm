//! Replay-loader infrastructure shared by the `replay` binary and the
//! `tests/replay_fixtures.rs` integration test.
//!
//! Per design doc §6.3, fixtures are JSON traces under
//! `crates/calm-server/tests/fixtures/events/<name>.events.json`. Two
//! consumers care about loading + replaying them:
//!
//!  1. The integration test in `tests/replay_fixtures.rs` — boots a
//!     bare WS-only router, raw-inserts events, drains the WS replay
//!     window, and asserts state against `expected`.
//!
//!  2. The `cargo run --bin replay` binary — boots the **full** app
//!     router (REST + WS) so a developer can poke the resulting state
//!     from a browser (`--serve`), or compares state against the
//!     fixture's `expected` block and exits with a status code
//!     (`--assert`).
//!
//! Rather than duplicate the load/boot/seed dance, this module hosts
//! the shared types + helpers and exposes them off `calm_server::replay`.
//! The test boots a minimal WS-only router on top of the seeded repo;
//! the binary boots the full router on top of the same seeded repo.
//! Both share the same fixture parser and seeding path.
//!
//! ## Fixture format
//!
//! ```json
//! {
//!   "name": "...",
//!   "description": "...",
//!   "events": [{ "kind": "...", "actor": "...", "payload": {...} }, ...],
//!   "expected": {
//!     "last_event_kind": "overlay.set",
//!     "layout_positions": { "<card_id>": { x, y, w, h }, ... }
//!   }
//! }
//! ```
//!
//! Events are seeded via `Repo::log_pure_event` — the public ingest
//! path used everywhere else in the kernel for events with no
//! associated entity write. That keeps the commit-then-emit invariant
//! intact and gives the seeded rows real `events.id`s in append order.

use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use crate::db::Repo;
use crate::db::sqlite::SqlxRepo;
use crate::event::{Event, EventBus};
use crate::plugin_host::{PluginHost, PluginRegistry};
use crate::state::{AppState, CodexClient, DaemonClient};

// ---------------------------------------------------------------------------
// Fixture shape (mirrors `tests/replay_fixtures.rs`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Fixture {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub events: Vec<FixtureEvent>,
    #[serde(default)]
    pub expected: FixtureExpected,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureEvent {
    pub kind: String,
    pub actor: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FixtureExpected {
    /// If present, assert the *last* event kind in the persisted log
    /// matches this value (post-seed).
    #[serde(default)]
    pub last_event_kind: Option<String>,
    /// If non-empty, assert the post-replay `view/layout` overlay's
    /// `positions` map matches this exactly (same cardinality, same
    /// per-card x/y/w/h).
    #[serde(default)]
    pub layout_positions: serde_json::Map<String, serde_json::Value>,
}

/// Read + parse a fixture from disk. Returns a descriptive error on
/// missing file or malformed JSON — the binary surfaces these straight
/// to stderr.
///
/// Accepts **two** on-disk shapes so RECORD_SESSION output is directly
/// replayable (round-trip invariant — design doc §6.3):
///
///   1. Curated fixture object: `{name, description, events, expected}` —
///      the format `tests/fixtures/events/*.events.json` ships.
///   2. NDJSON session recording: one `{"kind","actor","payload"}` object
///      per line — the shape `spawn_session_recorder` writes.
///
/// Detection: read the first non-blank line and check whether it parses
/// as a `FixtureEvent` (i.e. has a top-level `kind` field). If so, the
/// whole file is treated as NDJSON and a synthetic `Fixture` is
/// constructed with `name` derived from the filename, an empty
/// `expected` block, and the events in append order. Otherwise we fall
/// back to parsing the entire file as a single `Fixture` JSON object.
///
/// This is a pure shape sniff; nothing reads ahead beyond the first line
/// before committing to a branch.
pub fn load_fixture_from_path(path: &Path) -> Result<Fixture, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("read fixture {}: {}", path.display(), e))?;

    // Sniff: first non-blank line. If it parses as a `FixtureEvent`,
    // we're looking at NDJSON. Curated fixtures start `{` followed by
    // `"name"` / whitespace; that line on its own does not parse as
    // FixtureEvent (no `kind` field), so the sniff is unambiguous.
    let first_line = text.lines().find(|l| !l.trim().is_empty());
    if let Some(line) = first_line {
        if serde_json::from_str::<FixtureEvent>(line).is_ok() {
            // NDJSON branch — every non-blank line is a FixtureEvent.
            let mut events = Vec::new();
            for (lineno, raw) in text.lines().enumerate() {
                if raw.trim().is_empty() {
                    continue;
                }
                let ev: FixtureEvent = serde_json::from_str(raw).map_err(|e| {
                    format!(
                        "parse fixture {} (line {}): {}",
                        path.display(),
                        lineno + 1,
                        e
                    )
                })?;
                events.push(ev);
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("recorded-session")
                .to_string();
            return Ok(Fixture {
                name,
                description: "recorded session (NDJSON)".to_string(),
                events,
                expected: FixtureExpected::default(),
            });
        }
    }

    // Object branch — single JSON fixture object.
    serde_json::from_str(&text).map_err(|e| format!("parse fixture {}: {}", path.display(), e))
}

// ---------------------------------------------------------------------------
// In-memory boot
// ---------------------------------------------------------------------------

/// Boot an in-memory `SqlxRepo` + `EventBus` + minimal `AppState` with
/// stub external clients. No background tasks are spawned (no FSM, no
/// orphan-terminal sweeper) — the replay loader plays back a known event
/// log, and the kernel-internal projectors that would react to that log
/// would step on the seeded events.
///
/// Returns the components separately so the test harness (which only
/// wires up the WS router) and the binary (which mounts the full router)
/// can each build their own `axum::Router::with_state`.
pub async fn boot_in_memory() -> anyhow::Result<(Arc<SqlxRepo>, EventBus, AppState)> {
    let events = EventBus::new();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await?);
    let plugin = Arc::new(PluginHost::new(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
    ));
    let state = AppState {
        repo: repo.clone(),
        events: events.clone(),
        daemon: Arc::new(DaemonClient::new_stub()),
        plugin,
        codex: Arc::new(CodexClient::new_stub()),
    };
    Ok((repo, events, state))
}

/// Raw-insert every fixture event into the repo via `Repo::log_pure_event`.
/// Returns the assigned `events.id`s in append order.
///
/// `log_pure_event` is the same public path used by codex hook ingest +
/// plugin-state emission — it persists the event row and broadcasts the
/// envelope inside the commit-then-emit window. From the consumer's
/// perspective (WS subscribers, replay queries) the result is
/// indistinguishable from a "real" event from a write handler.
pub async fn seed_events(
    repo: &SqlxRepo,
    bus: &EventBus,
    fixture: &Fixture,
) -> anyhow::Result<Vec<i64>> {
    let mut out = Vec::with_capacity(fixture.events.len());
    for ev in &fixture.events {
        let event = Event::from_kind_and_payload(&ev.kind, ev.payload.clone())
            .map_err(|e| anyhow::anyhow!("reconstruct event {}: {}", ev.kind, e))?;
        let id = repo.log_pure_event(&ev.actor, None, bus, event).await?;
        out.push(id);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Assertion helpers used by `--assert`
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct AssertOutcome {
    pub matched: Vec<String>,
    pub failed: Vec<String>,
}

impl AssertOutcome {
    pub fn ok(&self) -> bool {
        self.failed.is_empty()
    }
    pub fn total(&self) -> usize {
        self.matched.len() + self.failed.len()
    }
}

/// Run every assertion in `fixture.expected` against the seeded repo
/// state. Missing fields in `expected` are silently skipped — callers
/// can ship partial fixtures while building up coverage.
///
/// Returns the matched/failed breakdown rather than panicking so the
/// binary can decide its own exit code + stdout format.
pub async fn assert_expected(repo: &SqlxRepo, fixture: &Fixture) -> anyhow::Result<AssertOutcome> {
    let mut matched: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    // last_event_kind — read the head of the event log.
    if let Some(expected_kind) = &fixture.expected.last_event_kind {
        // The fixture seeded N events via `log_pure_event`; the highest
        // id is the last one we inserted. Reuse `events_since(0)` to
        // grab the whole log in order (small fixtures only — replay
        // throughput target is 10k events per §6.4).
        let log = repo.events_since(0, None).await?;
        match log.last() {
            Some((_, ev)) => {
                let actual = ev.kind_tag();
                if actual == expected_kind {
                    matched.push(format!("last_event_kind == {expected_kind}"));
                } else {
                    failed.push(format!(
                        "last_event_kind: expected `{expected_kind}`, got `{actual}`"
                    ));
                }
            }
            None => failed.push(format!(
                "last_event_kind: expected `{expected_kind}` but event log is empty"
            )),
        }
    }

    // layout_positions — replay the event log to derive the current
    // `view/layout` overlay state, then compare its `positions` map.
    //
    // We can't query `overlays_for` here: `log_pure_event` writes the
    // event row but does *not* project to the entity tables (that's
    // the write-handler's job, and the loader bypasses handlers by
    // design — the whole point of a fixture is to seed a raw event
    // log without re-running the business logic that produced it).
    //
    // So we fold the event stream ourselves: walk events in id order,
    // and let each `overlay.set` for the target wave's `view/layout`
    // overwrite the running state. This is what a WS replay consumer
    // (`useOverlayState` in the frontend, the test in
    // `tests/replay_fixtures.rs`) would do, just done server-side
    // without the WS hop.
    if !fixture.expected.layout_positions.is_empty() {
        let wave_id = infer_wave_id(fixture);
        match wave_id {
            None => failed.push(
                "layout_positions: fixture does not reference a wave id we can target".into(),
            ),
            Some(wave_id) => {
                let actual_positions = derive_layout_positions(repo, &wave_id).await?;
                match actual_positions {
                    None => failed.push(format!(
                        "layout_positions: no `view/layout` overlay-set events for wave `{wave_id}`"
                    )),
                    Some(actual) => {
                        let expected = &fixture.expected.layout_positions;
                        let mut diff: Vec<String> = Vec::new();
                        for (k, v) in expected {
                            match actual.get(k) {
                                Some(av) if av == v => {}
                                Some(av) => {
                                    diff.push(format!("  card `{k}`: expected {v}, got {av}"))
                                }
                                None => diff.push(format!("  card `{k}`: missing")),
                            }
                        }
                        if actual.len() != expected.len() {
                            diff.push(format!(
                                "  cardinality: expected {} positions, got {}",
                                expected.len(),
                                actual.len()
                            ));
                        }
                        if diff.is_empty() {
                            matched.push(format!(
                                "layout_positions ({} entries) match",
                                expected.len()
                            ));
                        } else {
                            failed.push(format!(
                                "layout_positions mismatch on wave `{wave_id}`:\n{}",
                                diff.join("\n")
                            ));
                        }
                    }
                }
            }
        }
    }

    Ok(AssertOutcome { matched, failed })
}

/// Fold the persisted event log to derive the current `view/layout`
/// positions map for `wave_id`. Returns `None` if no `overlay.set` for
/// `(view, wave_id, layout)` has been observed (or the most recent
/// event for that overlay was an `overlay.deleted`). The fold mirrors
/// what `useOverlayState` does on the frontend: later events for the
/// same `(plugin_id, entity_kind, entity_id, kind)` quad overwrite
/// earlier ones, and `overlay.deleted` clears state.
///
/// Made `pub` so the integration test in `tests/replay_fixtures.rs`
/// can exercise the fold directly (a 3-step set/delete/set sequence is
/// easier to drive without a full WS round-trip). Not part of any
/// stable surface; treat as test-helper-shape.
pub async fn derive_layout_positions(
    repo: &SqlxRepo,
    wave_id: &str,
) -> anyhow::Result<Option<serde_json::Map<String, serde_json::Value>>> {
    let log = repo.events_since(0, None).await?;
    Ok(fold_layout_positions(log.into_iter().map(|(_id, ev)| ev), wave_id))
}

/// Pure fold used by `derive_layout_positions` — exposed so tests can
/// feed a hand-built event sequence without touching the repo. Walks
/// events in caller-provided order and tracks the running `view/layout`
/// state for `wave_id`: `overlay.set` upserts, `overlay.deleted` clears.
pub fn fold_layout_positions<I>(
    events: I,
    wave_id: &str,
) -> Option<serde_json::Map<String, serde_json::Value>>
where
    I: IntoIterator<Item = Event>,
{
    let mut current: Option<serde_json::Map<String, serde_json::Value>> = None;
    for ev in events {
        match ev {
            Event::OverlaySet(o)
                if o.entity_kind == "view" && o.entity_id == wave_id && o.kind == "layout" =>
            {
                current = o
                    .payload
                    .get("positions")
                    .and_then(|v| v.as_object().cloned())
                    .or(current);
            }
            Event::OverlayDeleted {
                entity_kind,
                entity_id,
                kind,
                ..
            } if entity_kind == "view" && entity_id == wave_id && kind == "layout" => {
                current = None;
            }
            _ => {}
        }
    }
    current
}

/// Best-effort: find the wave id the fixture's `view/layout` overlay
/// is attached to. The fixture schema does not name the wave directly
/// in `expected`, so we walk the seeded events and pick the first
/// `overlay.set` whose entity_kind is `view` and kind is `layout`.
fn infer_wave_id(fixture: &Fixture) -> Option<String> {
    for ev in &fixture.events {
        if ev.kind == "overlay.set"
            && ev.payload.get("entity_kind").and_then(|v| v.as_str()) == Some("view")
            && ev.payload.get("kind").and_then(|v| v.as_str()) == Some("layout")
        {
            if let Some(id) = ev.payload.get("entity_id").and_then(|v| v.as_str()) {
                return Some(id.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// RECORD_SESSION — append every emitted envelope as line-delimited JSON
// ---------------------------------------------------------------------------

/// Spawn a tokio task that subscribes to the event bus and appends every
/// envelope to `path` as a JSON line in the fixture's per-event shape
/// (`{"kind", "actor", "payload"}`).
///
/// Honored by `calm-server` when `RECORD_SESSION=<path>` is set in the
/// environment. The resulting file is directly playable by `replay --file`.
///
/// Limitations (design doc §6.3 calls this out):
///   - Actor is not currently threaded through `BroadcastEnvelope`, so
///     every recorded event lands with `actor: "unknown"`. Replaying the
///     trace produces the same `events.kind` rows; the actor field can be
///     hand-edited if it matters for the assertion. Follow-up tracked
///     in issue #39 (`[sync-engine] Thread actor through
///     BroadcastEnvelope so RECORD_SESSION captures real actors`).
///   - The leading entity snapshot mentioned in §6.3 is deferred: a
///     snapshot would let a fixture target a non-empty starting state
///     without re-seeding from scratch, but the existing §6.3 fixtures
///     (and the wave-grid trace) already start from empty, so this gap
///     doesn't block the headline workflow.
pub fn spawn_session_recorder(bus: &EventBus, path: std::path::PathBuf) {
    let mut rx = bus.subscribe();
    tokio::spawn(async move {
        // Open in append mode — multiple server restarts under the same
        // `RECORD_SESSION` accumulate into one trace, which is usually
        // what you want when reproducing a "weird thing that happened
        // across a restart" bug.
        let file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(
                    target: "replay",
                    path = %path.display(),
                    error = %e,
                    "RECORD_SESSION: failed to open session file — recording disabled"
                );
                return;
            }
        };
        tracing::info!(
            target: "replay",
            path = %path.display(),
            "RECORD_SESSION: appending events to session file"
        );
        let mut writer = std::io::BufWriter::new(file);
        use std::io::Write;
        loop {
            match rx.recv().await {
                Ok(envelope) => {
                    let kind = envelope.event.kind_tag();
                    let payload = envelope.event.payload_value();
                    let line = serde_json::json!({
                        "kind": kind,
                        // Actor is not present on `BroadcastEnvelope` —
                        // see module docs. Recorded as a placeholder so
                        // the file remains valid for `replay --file`.
                        "actor": "unknown",
                        "payload": payload,
                    });
                    if let Err(e) = writeln!(writer, "{line}") {
                        tracing::error!(
                            target: "replay",
                            error = %e,
                            "RECORD_SESSION: write failed — recording aborted"
                        );
                        return;
                    }
                    let _ = writer.flush();
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        target: "replay",
                        skipped = n,
                        "RECORD_SESSION: lagged behind bus — events skipped"
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}
