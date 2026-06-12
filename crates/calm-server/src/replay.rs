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

use futures::future::BoxFuture;
use serde::Deserialize;

use crate::db::RepoEventWrite;
use crate::db::sqlite::SqlxRepo;
use crate::event::{Event, EventBus, EventScope};
use crate::ids::ActorId;
use crate::operation::SpawnHandle;
use crate::operation::terminal_adapter::SpawnHook;
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
    /// PR2 of #136 — accepts both shapes so the loader round-trips:
    ///   * `"user"` / `"kernel"` / `"ai:codex"` — the pre-PR2 string
    ///     audit grammar (still present in checked-in fixtures).
    ///   * `{"kind": "User"}` / `{"kind": "AiCodex", "id": "..."}` —
    ///     the typed [`ActorId`] JSON form the recorder now writes.
    ///
    /// [`seed_events`] maps either back onto the typed [`ActorId`] via
    /// [`actor_from_legacy_string`] (string branch) or direct
    /// deserialization (object branch).
    pub actor: serde_json::Value,
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
    if let Some(line) = first_line
        && serde_json::from_str::<FixtureEvent>(line).is_ok()
    {
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
    // PR3 (#136) — replay path doesn't need role enforcement coverage
    // (fixtures replay as `ActorId::User`, which the gate lets through
    // without a cache lookup). An empty cache is fine.
    let card_role_cache = crate::card_role_cache::CardRoleCache::new();
    let wave_cove_cache = crate::wave_cove_cache::WaveCoveCache::new();
    let write = crate::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone());
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        std::path::PathBuf::new(),
        std::env::temp_dir().join("calm-plugins-data"),
        Vec::new(),
        events.clone(),
        write,
    ));
    let state = AppState::from_parts_with_terminal_spawn_hook(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        Some(wave_cove_cache),
        replay_terminal_spawn_hook(),
    );
    Ok((repo, events, state))
}

fn replay_terminal_spawn_hook() -> SpawnHook {
    Arc::new(
        |terminal_id: String,
         _program: String,
         _cwd: String,
         _env: serde_json::Value|
         -> BoxFuture<'static, crate::error::Result<SpawnHandle>> {
            Box::pin(async move {
                Ok(SpawnHandle::Terminal {
                    terminal_id: terminal_id.clone(),
                    renderer_id: terminal_id,
                })
            })
        },
    )
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
    // PR3 (#136) — seed path uses an empty cache. Fixture events are
    // replayed under their persisted actor (predominantly `User`); the
    // role gate lets `User`/`Kernel`/`Plugin` through without a cache
    // lookup. `AiCodex` actors in legacy fixtures predate PR3's role
    // model and would be denied for unknown card — that's intentional
    // (replay should refuse to ingest events the live kernel would
    // refuse to mint).
    let cache = crate::card_role_cache::CardRoleCache::new();
    let wcc = crate::wave_cove_cache::WaveCoveCache::new();
    for ev in &fixture.events {
        let event = Event::from_kind_and_payload(&ev.kind, ev.payload.clone())
            .map_err(|e| anyhow::anyhow!("reconstruct event {}: {}", ev.kind, e))?;
        // PR2 of #136: Fixtures predate typed actors/scope but the
        // RECORD_SESSION recorder now writes the typed JSON form. Accept
        // either:
        //   * a bare string (`"user"` / `"kernel"` / `"ai:codex"` / `"plugin:foo"`)
        //     — the legacy fixture grammar, mapped via `actor_from_legacy_string`,
        //   * the typed `ActorId` JSON object (`{"kind":"User"}` etc.)
        //     — directly deserialized.
        // Scope is always `System` for fixtures — replays don't carry
        // ancestor metadata and the NULL-fallback collapses it anyway.
        // Tests that need scope assertions should write through the
        // typed surface, not the fixture path.
        let actor = if let Some(s) = ev.actor.as_str() {
            actor_from_legacy_string(s)
        } else {
            serde_json::from_value(ev.actor.clone())
                .map_err(|e| anyhow::anyhow!("invalid actor on fixture event: {e}"))?
        };
        let id = repo
            .log_pure_event(actor, EventScope::System, None, bus, &cache, &wcc, event)
            .await?;
        out.push(id);
    }
    Ok(out)
}

/// Map a legacy fixture-actor string to an [`ActorId`]. The pre-PR2
/// audit grammar was `"user"` / `"kernel"` / `"plugin:<id>"` / `"ai:<id>"`;
/// PR2 of #136 superseded that with the typed enum. This helper preserves
/// the round-trip for replay fixtures shipped under the old wire format.
fn actor_from_legacy_string(s: &str) -> ActorId {
    if s == "user" {
        ActorId::User
    } else if s == "kernel" {
        ActorId::Kernel
    } else if let Some(id) = s.strip_prefix("plugin:") {
        ActorId::Plugin(id.to_string())
    } else if s == "ai:codex" {
        // Legacy fixtures don't carry a card id; PR3 will reattribute
        // via the dispatcher. Use an empty CardId tag — honest "we know
        // it's codex but the fixture doesn't say which card".
        ActorId::AiCodex(crate::ids::CardId::from(""))
    } else {
        // Unknown legacy form — attribute as User rather than fabricate
        // a typed identity from an attacker-controlled string. Replay
        // fixtures are checked-in test data, so this is just paranoia.
        ActorId::User
    }
}

/// Wipe every row from the in-memory repo and re-seed the fixture's event
/// stream. Used exclusively by `--serve` mode's `POST /dev/reset`
/// endpoint to give the Playwright `a11y` suite a hermetic starting
/// point per test (issue #56 followup).
///
/// **Dev-only.** This bypasses the audited write path on purpose — the
/// reset is conceptually a fresh boot of the in-memory kernel, not a
/// business mutation. To keep parity with `boot_in_memory()` we delete
/// every domain row + the entire event log + the `sqlite_sequence` rows
/// (so re-seeding starts at `events.id = 1` like a cold boot would). The
/// re-seed then runs through the normal `seed_events` path, emitting
/// envelopes on the bus exactly as the initial boot did.
///
/// Tables wiped match the schema declared by `migrations/0001..0004`:
/// `events`, `overlays`, `cards`, `waves`, `coves`, `terminals`,
/// `plugins`, `plugin_kv`, `plugin_tokens`, `settings`, plus the
/// #644 `tasks` table (migration 0041). Migration rows
/// (`_sqlx_migrations`) are preserved — wiping them would force a
/// re-migrate that we don't need for a stateful reset.
///
/// Returns the assigned `events.id`s of the freshly-seeded events.
pub async fn reset_from_fixture(
    repo: &SqlxRepo,
    bus: &EventBus,
    fixture: &Fixture,
) -> anyhow::Result<Vec<i64>> {
    // Delete order respects FK chains, children before parents:
    // `terminals.card_id → cards` is now `ON DELETE RESTRICT`
    // (migration 0011), so terminals MUST be wiped before cards or
    // the FK trips with `(code: 1811) FOREIGN KEY constraint failed`.
    // After that, `cards.wave_id → waves` and `waves.cove_id → coves`
    // still cascade, but we delete them explicitly in child-first order
    // so the whole table-wipe sequence is uniform and order-correct
    // regardless of which FKs are RESTRICT vs CASCADE. The SqlxRepo
    // opens with `PRAGMA foreign_keys = ON`, so an out-of-order delete
    // would surface as a constraint error — this explicit ordering is
    // what enforces correctness; we no longer rely on `ON DELETE CASCADE`
    // declarations to bail us out.
    //
    // `overlays` and `events` have no FKs into the domain tables, so
    // they can go anywhere; we drain them first to keep the audit log
    // out of the way of the structural wipe.
    let pool = repo.pool();
    let mut tx = pool.begin().await?;
    for stmt in [
        "DELETE FROM events",
        "DELETE FROM overlays",
        "DELETE FROM terminals",
        "DELETE FROM cards",
        // `tasks` (migration 0041, issue #644) deliberately has no FK to
        // `waves`, so deleting `waves` will NOT cascade here — the wipe
        // must name it explicitly or task rows leak across resets.
        "DELETE FROM tasks",
        "DELETE FROM waves",
        "DELETE FROM coves",
        "DELETE FROM plugin_kv",
        "DELETE FROM plugin_tokens",
        "DELETE FROM plugins",
        "DELETE FROM settings",
        // Reset all AUTOINCREMENT counters so re-seeded events start at
        // id=1 (matching a cold `boot_in_memory()`). Without this, a
        // fixture's `expected.last_event_kind` would still pass — the
        // assertion only looks at the tip — but any WS replay client
        // that recorded a cursor between resets would see id-skips,
        // which is exactly the determinism break this endpoint exists
        // to prevent.
        "DELETE FROM sqlite_sequence",
    ] {
        sqlx::query(stmt).execute(&mut *tx).await?;
    }
    tx.commit().await?;

    seed_events(repo, bus, fixture).await
}

// ---------------------------------------------------------------------------
// `force_spec_phase` — issue #682, dev hook behind `POST /dev/force-spec-phase`
// ---------------------------------------------------------------------------

/// Sentinel thread id stamped on dev-forced spec runtimes. The stub
/// app-server can never start a real thread in replay mode, but the
/// harness needs *a* thread id to be recoverable (boot recovery and
/// `/spec/input` lazy recovery both refuse rows with no thread anywhere).
#[cfg(feature = "fixtures")]
pub const DEV_FORCED_THREAD_ID: &str = "dev-forced-thread";

/// Outcome of [`force_spec_phase`], serialized verbatim into the replay
/// binary's `POST /dev/force-spec-phase` response body.
#[cfg(feature = "fixtures")]
#[derive(Debug, serde::Serialize)]
pub struct ForceSpecPhaseOutcome {
    pub card_id: String,
    pub runtime_id: String,
    pub old_phase: crate::harness::HarnessPhaseTag,
    pub new_phase: crate::harness::HarnessPhaseTag,
}

/// Issue #682 PR-1 — force a spec card's harness phase. Dev-only: this is
/// the engine behind the replay binary's `POST /dev/force-spec-phase`, so
/// Playwright e2e can drive `GET /spec/run`, `harness.phase.changed`
/// events, and the SpecCurrentRun UI without a real codex daemon.
///
/// Why the function must stand its own harness up (Step-0 probe, pinned by
/// `tests/replay_force_spec_phase.rs`): in replay boot the shared codex
/// app-server is a stub (`is_running()` == false), so the
/// `spec-harness-start` operation submitted by `POST /api/waves` fails at
/// `validate` — the spec card exists but has NO runtime row and NO
/// registered harness. A 404 on registry miss would make e2e setup
/// impossible, so this converges any valid spec card to a forceable
/// harness:
///
/// 1. card guard mirrors the production `/spec/*` routes (404 unknown
///    card / role, 403 non-spec-codex);
/// 2. no active runtime row → insert one (`runtime_start_tx`, kind
///    `SharedSpec`) carrying an initial [`HarnessSnapshot`] and the
///    [`DEV_FORCED_THREAD_ID`] sentinel;
/// 3. registry miss → [`crate::harness::spawn_recovered_harness`] — the
///    exact seam boot recovery uses; it does no codex RPC;
/// 4. [`crate::harness::SpecHarness::force_phase_for_dev`] sets the FSM
///    state and reuses the regular persist path, so snapshot, runtime
///    status, `GET /spec/run`, and the `HarnessPhaseChanged` event all
///    agree by construction.
///
/// Errors use [`crate::error::CalmError`] so the binary's handler can map
/// `e.status()` straight to an HTTP status.
#[cfg(feature = "fixtures")]
pub async fn force_spec_phase(
    state: &AppState,
    repo: Arc<dyn crate::db::Repo>,
    card_id: &str,
    to: crate::harness::HarnessPhaseTag,
) -> crate::error::Result<ForceSpecPhaseOutcome> {
    use crate::db::write_in_tx_typed;
    use crate::error::CalmError;
    use crate::harness::{HarnessSnapshot, is_harness_snapshot_value};
    use crate::model::{CardRole, new_id, now_ms};
    use crate::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};

    // Guard chain mirrors `routes::cards::get_spec_run`: card → role → kind.
    let card = repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let role = state
        .write()
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    if card.kind != "codex" || role != CardRole::Spec {
        return Err(CalmError::Forbidden(format!(
            "card {card_id} is not a spec codex card",
        )));
    }

    // Ensure an active runtime row exists (Step-0: replay boot leaves none).
    let card_id_string = card.id.to_string();
    let runtime = match repo.runtime_get_active_for_card(&card_id_string).await? {
        Some(runtime) => runtime,
        None => {
            let runtime_id = new_id();
            let mut snapshot = HarnessSnapshot::initial(0, Vec::new());
            snapshot.last_thread_id = Some(DEV_FORCED_THREAD_ID.into());
            let snapshot_value = serde_json::to_value(&snapshot)?;
            let runtime_id_for_tx = runtime_id.clone();
            let card_id_for_tx = card_id_string.clone();
            write_in_tx_typed(repo.as_ref(), move |tx| {
                Box::pin(async move {
                    crate::db::sqlite::runtime_start_tx(
                        tx,
                        RuntimeInit {
                            id: runtime_id_for_tx,
                            card_id: card_id_for_tx,
                            kind: RuntimeKind::SharedSpec,
                            agent_provider: Some(AgentProvider::Codex),
                            status: RunStatus::Idle,
                            terminal_run_id: None,
                            thread_id: Some(DEV_FORCED_THREAD_ID.into()),
                            session_id: None,
                            active_turn_id: None,
                            handle_state_json: Some(snapshot_value),
                            lease_owner: None,
                            lease_until_ms: None,
                            now_ms: now_ms(),
                        },
                    )
                    .await?;
                    Ok(())
                })
            })
            .await?;
            repo.runtime_get_active_for_card(&card_id_string)
                .await?
                .ok_or_else(|| {
                    CalmError::Internal(format!(
                        "dev-forced runtime {runtime_id} missing right after insert"
                    ))
                })?
        }
    };

    // `spawn_recovered_harness` needs a deserializable snapshot on the row;
    // a row from some other (half-failed) source may lack one — heal it
    // with a fresh initial snapshot rather than 404ing.
    let runtime = match runtime.handle_state_json.as_ref() {
        Some(value) if is_harness_snapshot_value(value) => runtime,
        _ => {
            let mut snapshot = HarnessSnapshot::initial(0, Vec::new());
            snapshot.last_thread_id = runtime
                .thread_id
                .clone()
                .filter(|t| !t.trim().is_empty())
                .or_else(|| Some(DEV_FORCED_THREAD_ID.into()));
            let snapshot_value = serde_json::to_value(&snapshot)?;
            let runtime_id_for_tx = runtime.id.clone();
            write_in_tx_typed(repo.as_ref(), move |tx| {
                Box::pin(async move {
                    crate::db::sqlite::runtime_set_handle_state_tx(
                        tx,
                        &runtime_id_for_tx,
                        Some(snapshot_value),
                    )
                    .await?;
                    Ok(())
                })
            })
            .await?;
            repo.runtime_get_active_for_card(&card_id_string)
                .await?
                .ok_or_else(|| {
                    CalmError::Internal(format!(
                        "runtime for card {card_id_string} vanished while healing snapshot"
                    ))
                })?
        }
    };

    // Registry miss → stand the harness up via the boot-recovery seam
    // (no codex RPC; snapshot load + catch-up replay + run + register).
    let harness = match state.harness.get(&runtime.id) {
        Some(harness) => harness,
        None => crate::harness::spawn_recovered_harness(
            repo.clone(),
            state.events.clone(),
            state.card_role_cache.clone(),
            state.wave_cove_cache.clone(),
            state.shared_codex_appserver.clone(),
            &state.harness,
            runtime.clone(),
        )
        .await?
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "spawn_recovered_harness declined runtime {} for card {card_id_string}",
                runtime.id
            ))
        })?,
    };

    let (old_phase, new_phase) = harness.force_phase_for_dev(to).await?;
    Ok(ForceSpecPhaseOutcome {
        card_id: card_id_string,
        runtime_id: runtime.id,
        old_phase,
        new_phase,
    })
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
            Some((_, _, _, ev)) => {
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
    Ok(fold_layout_positions(
        log.into_iter().map(|(_id, _ver, _scope, ev)| ev),
        wave_id,
    ))
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
            && let Some(id) = ev.payload.get("entity_id").and_then(|v| v.as_str())
        {
            return Some(id.to_string());
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
/// The `actor` field on each recorded line is whatever the producing
/// `write_with_event` / `log_pure_event` call passed through — i.e. the
/// declared identity from `X-Calm-Actor` or the handler's `Actor::kernel()`
/// constant. Replayed traces preserve real attribution (issue #39).
///
/// Limitations (design doc §6.3 calls this out):
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
                        // Real per-event attribution carried on the
                        // envelope by the wrapper that committed the
                        // events row (issue #39). The replay loader
                        // round-trips this field verbatim.
                        "actor": envelope.actor,
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
