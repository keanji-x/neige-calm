//! Frozen gate-denial security vectors — issue #679 PR0-A.
//!
//! The role-gate decision matrix (event kind × actor × scope → allow/deny)
//! is materialized as data files under `tests/vectors/gate_denials/*.json`.
//! This driver loads every vector and executes it through the **real write
//! entry** — `Repo::log_pure_event` on a real sqlite `SqlxRepo` with a
//! seeded card-role / wave-cove cache — exactly the path production MCP /
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
//! AiSpec→task.completed and performs no self-scope check for
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
//!   "scope":  { "kind": "Card", "id": { "card": "...", "wave": "...", "cove": "..." } },
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
use calm_server::db::sqlite::SqlxRepo;
use calm_server::error::CalmError;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave};
use calm_server::wave_cove_cache::WaveCoveCache;
use serde::Deserialize;
use serde_json::{Value, json};

/// Total number of vectors shipped across all files. Pinned so a vector
/// silently dropped from a JSON file (e.g. a bad merge) fails loudly.
/// Adding/removing vectors updates this constant in the same
/// `FROZEN-VECTOR-CHANGE:` commit that touches the vectors dir.
const EXPECTED_VECTOR_COUNT: usize = 51;

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

/// Real-sqlite fixture mirroring `dispatcher_role_scope.rs`: two coves,
/// each with one wave; the home wave hosts a codex worker, a claude
/// worker, a spec card, a report card, and a second ("other") worker.
/// Roles land in both the cards table and the in-memory caches the
/// write entry consults.
struct Fixture {
    repo: Arc<SqlxRepo>,
    bus: EventBus,
    cache: CardRoleCache,
    wcc: WaveCoveCache,
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
        let wcc = WaveCoveCache::new();
        repo.seed_wave_cove_cache(&wcc).await.unwrap();

        let (home_cove, home_wave) = seed_cove_wave(&repo, &wcc, "home-cove", "home-wave").await;
        let (other_cove, other_wave) =
            seed_cove_wave(&repo, &wcc, "other-cove", "other-wave").await;

        let worker = seed_card(&repo, &cache, &home_wave, CardRole::Worker).await;
        let claude_worker = seed_card(&repo, &cache, &home_wave, CardRole::Worker).await;
        let spec = seed_card(&repo, &cache, &home_wave, CardRole::Spec).await;
        let report = seed_card(&repo, &cache, &home_wave, CardRole::ReportCard).await;
        let other = seed_card(&repo, &cache, &home_wave, CardRole::Worker).await;

        let subst = vec![
            ("$CLAUDE_WORKER_CARD", claude_worker.as_str().to_string()),
            ("$UNKNOWN_CARD", "card-never-minted-0000".to_string()),
            ("$WORKER_CARD", worker.as_str().to_string()),
            ("$REPORT_CARD", report.as_str().to_string()),
            ("$OTHER_CARD", other.as_str().to_string()),
            ("$SPEC_CARD", spec.as_str().to_string()),
            ("$HOME_WAVE", home_wave.as_str().to_string()),
            ("$HOME_COVE", home_cove.as_str().to_string()),
            ("$OTHER_WAVE", other_wave.as_str().to_string()),
            ("$OTHER_COVE", other_cove.as_str().to_string()),
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

async fn seed_cove_wave(
    repo: &SqlxRepo,
    wcc: &WaveCoveCache,
    cove_name: &str,
    wave_title: &str,
) -> (CoveId, WaveId) {
    let cove = repo
        .cove_create(NewCove {
            name: cove_name.into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: wave_title.into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let (cove_id, wave_id) = (
        CoveId::from(cove.id.as_str()),
        WaveId::from(wave.id.as_str()),
    );
    // The gate's #234 cove cross-check consults this cache.
    wcc.insert(wave_id.clone(), cove_id.clone());
    (cove_id, wave_id)
}

async fn seed_card(
    repo: &SqlxRepo,
    cache: &CardRoleCache,
    wave: &WaveId,
    role: CardRole,
) -> CardId {
    let card = repo
        .card_create(NewCard {
            wave_id: wave.as_str().into(),
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    let role_str = match role {
        CardRole::Worker => "worker",
        CardRole::Spec => "spec",
        CardRole::ReportCard => "reportcard",
    };
    sqlx::query("UPDATE cards SET role = ?1 WHERE id = ?2")
        .bind(role_str)
        .bind(card.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    cache.insert(card.id.clone(), role, wave.clone());
    CardId::from(card.id.as_str())
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
