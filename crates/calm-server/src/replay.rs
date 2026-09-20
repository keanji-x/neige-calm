//! Replay-loader infrastructure shared by the `replay` binary and the `tests/replay_fixtures.rs` integration test: fixture parsing, in-memory boot, and event seeding via `Repo::log_pure_event`.

use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde::Deserialize;

use crate::db::RepoEventWrite;
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
use crate::event::{Event, EventBus, EventScope};
use crate::ids::ActorId;
use crate::operation::SpawnHandle;
use crate::operation::terminal_adapter::SpawnHook;
use crate::plugin_host::{PluginHost, PluginRegistry};
use crate::state::{AppState, CodexClient, DaemonClient};

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
    /// Either the legacy string grammar (`"user"` / `"kernel"` / `"ai:codex"`) or the typed `ActorId` JSON object.
    pub actor: serde_json::Value,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FixtureExpected {
    /// If present, assert the *last* event kind in the persisted log matches.
    #[serde(default)]
    pub last_event_kind: Option<String>,
    /// If non-empty, assert the post-replay `view/layout` overlay's `positions` map matches exactly.
    #[serde(default)]
    pub layout_positions: serde_json::Map<String, serde_json::Value>,
}

/// Read + parse a fixture from disk. Accepts a curated fixture object or an NDJSON session recording (one `{"kind","actor","payload"}` per line); the first non-blank line decides which.
pub fn load_fixture_from_path(path: &Path) -> Result<Fixture, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("read fixture {}: {}", path.display(), e))?;

    // A curated fixture's first line has no `kind` field, so the sniff is unambiguous.
    let first_line = text.lines().find(|l| !l.trim().is_empty());
    if let Some(line) = first_line
        && serde_json::from_str::<FixtureEvent>(line).is_ok()
    {
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

    serde_json::from_str(&text).map_err(|e| format!("parse fixture {}: {}", path.display(), e))
}

/// Boot an in-memory `SqlxRepo` + `EventBus` + minimal `AppState` with stub external clients. No background tasks: kernel-internal projectors would step on the seeded events.
pub async fn boot_in_memory() -> anyhow::Result<(Arc<SqlxRepo>, EventBus, AppState)> {
    let events = EventBus::new();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await?);
    // Fixtures replay as `ActorId::User`, which the role gate lets through without a cache lookup.
    let card_role_cache = crate::card_role_cache::CardRoleCache::new();
    let track_area_cache = crate::track_area_cache::TrackAreaCache::new();
    let write = crate::state::WriteContext::new(card_role_cache.clone(), track_area_cache.clone());
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
        Some(track_area_cache),
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

/// Raw-insert every fixture event via `Repo::log_pure_event` (the same path as hook ingest), returning the assigned `events.id`s in append order.
pub async fn seed_events(
    repo: &SqlxRepo,
    bus: &EventBus,
    fixture: &Fixture,
) -> anyhow::Result<Vec<i64>> {
    let mut out = Vec::with_capacity(fixture.events.len());
    // Empty cache: `AiCodex` actors in legacy fixtures would be denied for an unknown card, intentionally — replay must not ingest what the live kernel would refuse.
    let cache = crate::card_role_cache::CardRoleCache::new();
    let wcc = crate::track_area_cache::TrackAreaCache::new();
    for ev in &fixture.events {
        let event = Event::from_kind_and_payload(&ev.kind, ev.payload.clone())
            .map_err(|e| anyhow::anyhow!("reconstruct event {}: {}", ev.kind, e))?;
        // Scope is always `System` for fixtures: replays don't carry ancestor metadata.
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

/// Map a legacy fixture-actor string (`"user"` / `"kernel"` / `"plugin:<id>"` / `"ai:<id>"`) to an [`ActorId`].
fn actor_from_legacy_string(s: &str) -> ActorId {
    if s == "user" {
        ActorId::User
    } else if s == "kernel" {
        ActorId::Kernel
    } else if let Some(id) = s.strip_prefix("plugin:") {
        ActorId::Plugin(id.to_string())
    } else if s == "ai:codex" {
        // Legacy fixtures don't carry a card id; an empty CardId tag is the honest answer.
        ActorId::AiCodex(crate::ids::CardId::from(""))
    } else {
        // Unknown legacy form: attribute as User rather than fabricate a typed identity from the string.
        ActorId::User
    }
}

/// Wipe every row from the in-memory repo and re-seed the fixture's event stream (`POST /dev/reset` in `--serve` mode).
/// Dev-only: bypasses the audited write path on purpose. `_sqlx_migrations` is preserved.
pub async fn reset_from_fixture(
    repo: &SqlxRepo,
    bus: &EventBus,
    fixture: &Fixture,
) -> anyhow::Result<Vec<i64>> {
    // Delete order respects FK chains, children before parents: `PRAGMA foreign_keys = ON` and some FKs are RESTRICT, so this explicit ordering is what enforces correctness.
    let pool = repo.pool();
    // Writing transactions always BEGIN IMMEDIATE.
    let mut tx = begin_immediate_tx(pool).await?;
    for stmt in [
        "DELETE FROM task_candidate_decision_bindings",
        "DELETE FROM task_candidate_decisions",
        "DELETE FROM events",
        // A stale `events_prune_watermark` above the re-seeded ids would strand every WS client in a `_snapshot_required` loop.
        "DELETE FROM retention_meta",
        "DELETE FROM overlays",
        "DELETE FROM terminals",
        "DELETE FROM cards",
        "DELETE FROM task_ref_index",
        // `tasks` deliberately has no FK to `tracks`, so it must be named explicitly.
        "DELETE FROM tasks",
        "DELETE FROM task_attempt_allocations",
        // `tracks.root_session_id` has no ON DELETE SET NULL; clear it before worker sessions leave.
        "UPDATE tracks SET root_session_id = NULL",
        // `worker_sessions.track_id` is a NO ACTION FK, so sessions leave before their tracks.
        "DELETE FROM worker_sessions",
        "DELETE FROM tracks",
        "DELETE FROM areas",
        "DELETE FROM plugin_kv",
        "DELETE FROM plugin_tokens",
        "DELETE FROM plugins",
        "DELETE FROM settings",
        // Reset AUTOINCREMENT counters so re-seeded events start at id=1; a WS client holding a cursor across resets would otherwise see id-skips.
        "DELETE FROM sqlite_sequence",
    ] {
        sqlx::query(stmt).execute(&mut *tx).await?;
    }
    tx.commit().await?;

    seed_events(repo, bus, fixture).await
}

/// Sentinel thread id stamped on dev-forced planner runtimes; recovery refuses rows with no thread anywhere.
#[cfg(feature = "fixtures")]
pub const DEV_FORCED_THREAD_ID: &str = "dev-forced-thread";

/// Outcome of [`force_planner_phase`], serialized verbatim into the `POST /dev/force-planner-phase` response.
#[cfg(feature = "fixtures")]
#[derive(Debug, serde::Serialize)]
pub struct ForcePlannerPhaseOutcome {
    pub card_id: String,
    pub worker_session_id: String,
    pub old_phase: crate::harness::HarnessPhaseTag,
    pub new_phase: crate::harness::HarnessPhaseTag,
}

/// Dev-only: force a planner card's harness phase so e2e can drive `GET /planner/run` and `harness.phase.changed` without a real codex daemon.
/// In replay boot the app-server is a stub, so the planner card has no runtime row and no registered harness; this stands one up (runtime row, then `spawn_recovered_harness`) before forcing the phase through the regular persist path.
#[cfg(feature = "fixtures")]
pub async fn force_planner_phase(
    state: &AppState,
    repo: Arc<dyn crate::db::Repo>,
    card_id: &str,
    to: crate::harness::HarnessPhaseTag,
) -> crate::error::Result<ForcePlannerPhaseOutcome> {
    use axum::extract::FromRef;

    use crate::db::write_in_tx_typed;
    use crate::error::CalmError;
    use crate::harness::{HarnessPhaseTag, HarnessSnapshot, is_harness_snapshot_value};
    use crate::model::{CardRole, new_id, now_ms};
    use crate::per_card_lock::lock_card;
    use crate::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    use crate::state::RouteState;

    // `wedged` is not forceable: `persist_snapshot` would write `Failed`, which the read path filters, so the next force would mint a second runtime.
    if to == HarnessPhaseTag::Wedged {
        return Err(CalmError::BadRequest(
            "`wedged` is not forceable (a failed runtime row is no longer projectable by \
             GET /planner/run); supported phases: pending_thread_start, idle, issuing_turn, \
             issuing_interrupt, turn_running, turn_completed, resumed"
                .into(),
        ));
    }

    let card = repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let role = state
        .write()
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    if !crate::routes::cards::card_runs_headless_harness(&card, role) {
        return Err(CalmError::Forbidden(format!(
            "card {card_id} is not a planner codex card",
        )));
    }
    let track = repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {}", card.track_id)))?;
    if role == CardRole::Planner && track.purpose.as_deref() == Some(crate::AREA_CHAT_PURPOSE) {
        return Err(CalmError::Forbidden(format!(
            "planner harness is disabled for area chat track {}",
            track.id
        )));
    }
    // Same per-card recovery lock as `/planner/input` lazy recovery and `/planner/reset`, held through stand-up + force, or a concurrent Send could double-spawn the harness.
    let route = RouteState::from_ref(state);
    let _recovery_guard = lock_card(&route.planner_recovery_locks, card.id.as_str()).await;

    let card_id_string = card.id.to_string();
    let runtime = match repo
        .session_projection_active_for_card(&card_id_string)
        .await?
    {
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
                    crate::db::sqlite::session_start_runtime_tx(
                        tx,
                        WorkerSessionInit {
                            id: runtime_id_for_tx,
                            card_id: card_id_for_tx,
                            // Only a real planner card gets `SharedPlanner`, which makes the session the track's root authority.
                            kind: if role == CardRole::Planner {
                                WorkerSessionKind::SharedPlanner
                            } else {
                                WorkerSessionKind::CodexCard
                            },
                            agent_provider: Some(AgentProvider::Codex),
                            // `Idle`, not `Starting`: a `starting` row trips `ensure_live_planner_harness`'s 503 guard. The first `persist_snapshot` overwrites status anyway.
                            status: WorkerSessionState::Idle,
                            terminal_run_id: None,
                            thread_id: Some(DEV_FORCED_THREAD_ID.into()),
                            session_id: None,
                            active_turn_id: None,
                            handle_state_json: Some(snapshot_value),
                            spawn_op_id: None,
                            now_ms: now_ms(),
                        },
                    )
                    .await?;
                    Ok(())
                })
            })
            .await?;
            repo.session_projection_active_for_card(&card_id_string)
                .await?
                .ok_or_else(|| {
                    CalmError::Internal(format!(
                        "dev-forced runtime {runtime_id} missing right after insert"
                    ))
                })?
        }
    };

    // `spawn_recovered_harness` needs a deserializable snapshot on the row; heal a missing one rather than 404.
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
                    crate::db::sqlite::session_set_handle_state_tx(
                        tx,
                        &runtime_id_for_tx,
                        Some(snapshot_value),
                    )
                    .await?;
                    Ok(())
                })
            })
            .await?;
            repo.session_projection_active_for_card(&card_id_string)
                .await?
                .ok_or_else(|| {
                    CalmError::Internal(format!(
                        "runtime for card {card_id_string} vanished while healing snapshot"
                    ))
                })?
        }
    };

    // Registry miss → stand the harness up via the boot-recovery seam (no codex RPC).
    let harness = match state.harness.get(&runtime.id) {
        Some(harness) => harness,
        None => crate::harness::spawn_recovered_harness(
            repo.clone(),
            state.events.clone(),
            state.card_role_cache.clone(),
            state.track_area_cache.clone(),
            state.shared_codex_appserver.clone(),
            &state.harness,
            state.track_delete_locks(),
            runtime.clone(),
            crate::harness::ClaimMode::Replace,
        )
        .await?
        .installed()
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "spawn_recovered_harness declined runtime {} for card {card_id_string}",
                runtime.id
            ))
        })?,
    };

    // The recovered harness runs against the stub app-server and must never issue turns, or the run loop would churn phases every tick. Idempotent.
    harness.pause_issuance_for_dev();

    let (old_phase, new_phase) = harness.force_phase_for_dev(to).await?;
    Ok(ForcePlannerPhaseOutcome {
        card_id: card_id_string,
        worker_session_id: runtime.id,
        old_phase,
        new_phase,
    })
}

/// Shut down and deregister every registered planner harness, BEFORE `POST /dev/reset` reseeds: `reset_from_fixture` wipes the runtime rows, and a harness left registered would survive as an orphaned tick task.
/// Returns the number of harnesses shut down.
#[cfg(feature = "fixtures")]
pub async fn shutdown_registered_harnesses(state: &AppState) -> usize {
    let harnesses = state.harness.drain_all_for_dev();
    let count = harnesses.len();
    for harness in harnesses {
        if let Err(e) = harness.shutdown().await {
            tracing::warn!(error = %e, "dev reset: planner harness shutdown failed");
        }
    }
    count
}

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

/// Run every assertion in `fixture.expected` against the seeded repo; missing fields are skipped. Returns matched/failed rather than panicking.
pub async fn assert_expected(repo: &SqlxRepo, fixture: &Fixture) -> anyhow::Result<AssertOutcome> {
    let mut matched: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    if let Some(expected_kind) = &fixture.expected.last_event_kind {
        let log = repo.events_since(0, i64::MAX).await?;
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

    // `log_pure_event` does not project to the entity tables, so `overlays_for` cannot be queried; fold the event stream instead, as a WS replay consumer would.
    if !fixture.expected.layout_positions.is_empty() {
        let track_id = infer_track_id(fixture);
        match track_id {
            None => failed.push(
                "layout_positions: fixture does not reference a track id we can target".into(),
            ),
            Some(track_id) => {
                let actual_positions = derive_layout_positions(repo, &track_id).await?;
                match actual_positions {
                    None => failed.push(format!(
                        "layout_positions: no `view/layout` overlay-set events for track `{track_id}`"
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
                                "layout_positions mismatch on track `{track_id}`:\n{}",
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

/// Fold the persisted event log to derive the current `view/layout` positions for `track_id`; `None` if never set or last deleted. `pub` for the integration test.
pub async fn derive_layout_positions(
    repo: &SqlxRepo,
    track_id: &str,
) -> anyhow::Result<Option<serde_json::Map<String, serde_json::Value>>> {
    let log = repo.events_since(0, i64::MAX).await?;
    Ok(fold_layout_positions(
        log.into_iter().map(|(_id, _ver, _scope, ev)| ev),
        track_id,
    ))
}

/// Pure fold used by `derive_layout_positions`: `overlay.set` upserts, `overlay.deleted` clears.
pub fn fold_layout_positions<I>(
    events: I,
    track_id: &str,
) -> Option<serde_json::Map<String, serde_json::Value>>
where
    I: IntoIterator<Item = Event>,
{
    let mut current: Option<serde_json::Map<String, serde_json::Value>> = None;
    for ev in events {
        match ev {
            Event::OverlaySet(o)
                if o.entity_kind == "view" && o.entity_id == track_id && o.kind == "layout" =>
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
            } if entity_kind == "view" && entity_id == track_id && kind == "layout" => {
                current = None;
            }
            _ => {}
        }
    }
    current
}

/// Best-effort: the first `overlay.set` with entity_kind `view` and kind `layout` names the track.
fn infer_track_id(fixture: &Fixture) -> Option<String> {
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

/// Spawn a task that appends every bus envelope to `path` as a JSON line in the fixture's per-event shape; honored when `RECORD_SESSION=<path>` is set. The result is directly playable by `replay --file`.
pub fn spawn_session_recorder(bus: &EventBus, path: std::path::PathBuf) {
    let mut rx = bus.subscribe();
    tokio::spawn(async move {
        // Append mode: multiple restarts under the same `RECORD_SESSION` accumulate into one trace.
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
