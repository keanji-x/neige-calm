//! Frozen gate-denial security vectors — issue #679 PR0-A.
//!
//! The role-gate decision matrix (event kind × actor × scope → allow/deny)
//! is materialized as data files under `tests/vectors/gate_denials/*.json`.
//! This driver loads every vector and executes it through the **real write
//! entry** — `Repo::log_pure_event` on a real sqlite `SqlxRepo` with a
//! seeded card-role / track-area cache — exactly the path production MCP /
//! REST writes take after `routes`/`emit` construct the `(actor, scope,
//! event)` tuple. It deliberately imports **no role_gate internals**
//! (no `enforce_role`, no `RoleViolation`): the gate is observed only
//! through its transactional effect (Forbidden error, no event row, no
//! broadcast) so a future gate rewrite (#679 PR7's Principal gate) must
//! pass the *same vector files unmodified*.
//!
//! These vectors are CHARACTERIZATION — they pin current `main` behavior,
//! including cells that look like bugs (see the `note` fields in
//! `06_task_report_and_reportcard.json`: the kernel gate allows
//! AiPlanner→task.completed and performs no self-scope check for
//! ReportCard-bound actors). Do not "fix" a vector to match intuition:
//! changing any file under `tests/vectors/` requires a commit message
//! carrying `FROZEN-VECTOR-CHANGE:` + rationale (CI-enforced, see
//! `.github/workflows/ci.yml` job `frozen-vectors`).
//!
//! Vector schema (stable):
//! ```json
//! {
//!   "description": "...",
//!   "note": "optional characterization caveat",
//!   "actor":  { "kind": "AiCodex", "id": "$WORKER_CARD" },
//!   "event":  { "ev": "task.completed", "data": { ... } },
//!   "scope":  { "kind": "Card", "id": { "card": "...", "track": "...", "area": "..." } },
//!   "expected": { "decision": "allow" } | { "decision": "deny", "error_contains": "..." }
//! }
//! ```
//! `actor` / `scope` / `event` use the production serde wire shapes of
//! `ActorId` / `EventScope` / `Event` (adjacent-tagged), so the files stay
//! valid against the same compatibility guarantees as the persisted event
//! log. `$PLACEHOLDER` strings are substituted with ids minted by the
//! sqlite fixture before deserialization.

use std::path::PathBuf;
use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_insert_tx};
use calm_server::error::CalmError;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, AreaId, CardId, TrackId};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack};
use calm_server::session_projection_repo::WorkerSessionState;
use calm_server::track_area_cache::TrackAreaCache;
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// Total number of vectors shipped across all files. Pinned so a vector
/// silently dropped from a JSON file (e.g. a bad merge) fails loudly.
/// Adding/removing vectors updates this constant in the same
/// `FROZEN-VECTOR-CHANGE:` commit that touches the vectors dir.
const EXPECTED_VECTOR_COUNT: usize = 68;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vector {
    description: String,
    #[serde(default)]
    note: Option<String>,
    actor: Value,
    event: Value,
    scope: Value,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "decision", rename_all = "lowercase", deny_unknown_fields)]
enum Expected {
    Allow,
    Deny { error_contains: String },
}

/// Real-sqlite fixture mirroring `dispatcher_role_scope.rs`: two areas,
/// each with one track; the home track hosts a codex worker, a claude
/// worker, a planner card, a report card, and a second ("other") worker.
/// Roles land in both the cards table and the in-memory caches the
/// write entry consults.
struct Fixture {
    repo: Arc<SqlxRepo>,
    bus: EventBus,
    cache: CardRoleCache,
    wcc: TrackAreaCache,
    /// `$PLACEHOLDER` → minted id. Longest keys first so no placeholder
    /// is a prefix of an earlier-substituted one.
    subst: Vec<(&'static str, String)>,
}

impl Fixture {
    async fn boot() -> Self {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let bus = EventBus::new();
        let cache = CardRoleCache::new();
        repo.seed_card_role_cache(&cache).await.unwrap();
        let wcc = TrackAreaCache::new();
        repo.seed_track_area_cache(&wcc).await.unwrap();

        let (home_area, home_track) = seed_area_track(&repo, &wcc, "home-area", "home-track").await;
        let (other_area, other_track) =
            seed_area_track(&repo, &wcc, "other-area", "other-track").await;

        let worker = seed_card(&repo, &cache, &home_track, CardRole::Worker).await;
        let claude_worker = seed_card(&repo, &cache, &home_track, CardRole::Worker).await;
        let planner = seed_card(&repo, &cache, &home_track, CardRole::Planner).await;
        let report = seed_card(&repo, &cache, &home_track, CardRole::ReportCard).await;
        let other = seed_card(&repo, &cache, &home_track, CardRole::Worker).await;
        // #1189 — the Assistant arm needs an assistant card in the home
        // track and a report card in a *foreign* track (the "may not write
        // another track's report card" cell).
        let assistant = seed_card(&repo, &cache, &home_track, CardRole::Assistant).await;
        let other_track_report = seed_card(&repo, &cache, &other_track, CardRole::ReportCard).await;

        let worker_session = seed_worker_session(
            &repo,
            &home_track,
            "session-worker-vector-0001",
            WorkerSessionState::Running,
            Some(worker.clone()),
            WorkerContract::Executor,
        )
        .await;
        let planner_session = seed_worker_session(
            &repo,
            &home_track,
            "session-planner-vector-0001",
            WorkerSessionState::Running,
            Some(planner.clone()),
            WorkerContract::Planner,
        )
        .await;
        let terminal_session = seed_worker_session(
            &repo,
            &home_track,
            "session-terminal-vector-0001",
            WorkerSessionState::Exited,
            Some(worker.clone()),
            WorkerContract::Executor,
        )
        .await;
        let cardless_session = seed_worker_session(
            &repo,
            &home_track,
            "session-cardless-vector-0001",
            WorkerSessionState::Running,
            None,
            WorkerContract::Executor,
        )
        .await;

        let subst = vec![
            // Longest keys first so no placeholder is a prefix of an
            // earlier-substituted one ($OTHER_TRACK_REPORT_CARD would
            // otherwise be eaten by $OTHER_TRACK).
            (
                "$OTHER_TRACK_REPORT_CARD",
                other_track_report.as_str().to_string(),
            ),
            ("$CLAUDE_WORKER_CARD", claude_worker.as_str().to_string()),
            ("$ASSISTANT_CARD", assistant.as_str().to_string()),
            ("$TERMINAL_SESSION", terminal_session.as_str().to_string()),
            ("$CARDLESS_SESSION", cardless_session.as_str().to_string()),
            ("$UNKNOWN_SESSION", "session-never-minted-0000".to_string()),
            ("$WORKER_SESSION", worker_session.as_str().to_string()),
            ("$PLANNER_SESSION", planner_session.as_str().to_string()),
            ("$UNKNOWN_CARD", "card-never-minted-0000".to_string()),
            ("$WORKER_CARD", worker.as_str().to_string()),
            ("$REPORT_CARD", report.as_str().to_string()),
            ("$OTHER_CARD", other.as_str().to_string()),
            ("$PLANNER_CARD", planner.as_str().to_string()),
            ("$HOME_TRACK", home_track.as_str().to_string()),
            ("$HOME_AREA", home_area.as_str().to_string()),
            ("$OTHER_TRACK", other_track.as_str().to_string()),
            ("$OTHER_AREA", other_area.as_str().to_string()),
        ];

        Self {
            repo,
            bus,
            cache,
            wcc,
            subst,
        }
    }
}

async fn seed_area_track(
    repo: &SqlxRepo,
    wcc: &TrackAreaCache,
    area_name: &str,
    track_title: &str,
) -> (AreaId, TrackId) {
    let area = repo
        .area_create(NewArea {
            name: area_name.into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: track_title.into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let (area_id, track_id) = (
        AreaId::from(area.id.as_str()),
        TrackId::from(track.id.as_str()),
    );
    // The gate's #234 area cross-check consults this cache.
    wcc.insert(track_id.clone(), area_id.clone());
    (area_id, track_id)
}

async fn seed_card(
    repo: &SqlxRepo,
    cache: &CardRoleCache,
    track: &TrackId,
    role: CardRole,
) -> CardId {
    let card = repo
        .card_create(NewCard {
            track_id: track.as_str().into(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    let role_str = role.as_db_str();
    sqlx::query("UPDATE cards SET role = ?1 WHERE id = ?2")
        .bind(role_str)
        .bind(card.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    cache.insert(card.id.clone(), role, track.clone());
    CardId::from(card.id.as_str())
}

async fn seed_worker_session(
    repo: &SqlxRepo,
    track: &TrackId,
    session_id: &str,
    state: WorkerSessionState,
    card_id: Option<CardId>,
    contract: WorkerContract,
) -> WorkerSessionId {
    let session = WorkerSession {
        id: WorkerSessionId::from(session_id),
        track_id: track.clone(),
        provider: WorkerProviderKind::Codex,
        mode: SessionMode::Resumable,
        contract,
        parent_session_id: None,
        requester_session_id: None,
        state,
        mcp_token_hash: None,
        thread_id: None,
        agent_session_id: None,
        active_turn_id: None,
        terminal_run_id: None,
        card_id,
        handle_state_json: None,
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        last_activity_ms: None,
        last_thread_status: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let id = session.id.clone();
    calm_server::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            session_insert_tx(tx, session)
                .await
                .map_err(CalmError::from)?;
            Ok(())
        })
    })
    .await
    .expect("seed worker session");
    id
}

/// Replace `$PLACEHOLDER` tokens inside every string of a JSON value.
fn substitute(v: &Value, subst: &[(&'static str, String)]) -> Value {
    match v {
        Value::String(s) => {
            let mut out = s.clone();
            for (key, val) in subst {
                out = out.replace(key, val);
            }
            Value::String(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|x| substitute(x, subst)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, x)| (k.clone(), substitute(x, subst)))
                .collect(),
        ),
        other => other.clone(),
    }
}

async fn total_events(repo: &SqlxRepo) -> i64 {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    row.0
}

/// Execute one vector through the real write entry and check the frozen
/// expectation. Returns `Err(reason)` on any divergence.
async fn run_vector(fx: &Fixture, v: &Vector) -> Result<(), String> {
    let actor: ActorId = serde_json::from_value(substitute(&v.actor, &fx.subst))
        .map_err(|e| format!("vector `actor` failed to deserialize as ActorId: {e}"))?;
    let scope: EventScope = serde_json::from_value(substitute(&v.scope, &fx.subst))
        .map_err(|e| format!("vector `scope` failed to deserialize as EventScope: {e}"))?;
    let event: Event = serde_json::from_value(substitute(&v.event, &fx.subst))
        .map_err(|e| format!("vector `event` failed to deserialize as Event: {e}"))?;

    let before = total_events(&fx.repo).await;
    let mut sub = fx.bus.subscribe();

    let res = fx
        .repo
        .log_pure_event(actor, scope, None, &fx.bus, &fx.cache, &fx.wcc, event)
        .await;

    let after = total_events(&fx.repo).await;

    match &v.expected {
        Expected::Allow => {
            if let Err(e) = &res {
                return Err(format!("expected allow, write was refused: {e:?}"));
            }
            if after != before + 1 {
                return Err(format!(
                    "allowed write must append exactly one event row (before={before}, after={after})"
                ));
            }
            if sub.try_recv().is_err() {
                return Err("allowed write must broadcast its envelope".into());
            }
        }
        Expected::Deny { error_contains } => {
            match &res {
                Err(CalmError::Forbidden(msg)) if msg.contains(error_contains.as_str()) => {}
                other => {
                    return Err(format!(
                        "expected Forbidden containing {error_contains:?}, got {other:?}"
                    ));
                }
            }
            if after != before {
                return Err(format!(
                    "denied write must not append an event row (before={before}, after={after})"
                ));
            }
            if sub.try_recv().is_ok() {
                return Err("denied write must not broadcast".into());
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn frozen_gate_denial_vectors_hold() {
    let fx = Fixture::boot().await;

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/gate_denials");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("vectors dir {} unreadable: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no vector files found under {} — the frozen corpus is gone",
        dir.display(),
    );

    let mut total = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for file in &files {
        let raw = std::fs::read_to_string(file).unwrap();
        let vectors: Vec<Vector> = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("{}: invalid vector JSON: {e}", file.display()));
        let file_name = file.file_name().unwrap().to_string_lossy().into_owned();
        for (idx, vector) in vectors.iter().enumerate() {
            total += 1;
            if let Err(reason) = run_vector(&fx, vector).await {
                let note = vector
                    .note
                    .as_deref()
                    .map(|n| format!(" [note: {n}]"))
                    .unwrap_or_default();
                failures.push(format!(
                    "{file_name}[{idx}] `{}`: {reason}{note}",
                    vector.description,
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} frozen gate vector(s) diverged from current behavior:\n{}",
        failures.len(),
        failures.join("\n"),
    );
    assert_eq!(
        total, EXPECTED_VECTOR_COUNT,
        "vector corpus size changed — update EXPECTED_VECTOR_COUNT in the same \
         FROZEN-VECTOR-CHANGE commit that edits tests/vectors/",
    );
}
