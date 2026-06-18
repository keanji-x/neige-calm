//! Issue #644 PR-A — `mcp_server::tools::plan` integration coverage.
//!
//! Boots an in-memory `SqlxRepo` + `EventBus` + pre-seeded role caches,
//! constructs an `AppContext` directly (no live MCP listener — the
//! tools' contract is "given a `ToolCallIdentity` + `Value` args, do
//! the right thing"), and drives `calm.plan.upsert` / `calm.plan.cancel`
//! / `calm.plan.list` end-to-end against migration 0041.
//!
//! Field-level validation details (key regex, kind vocabulary, gate
//! shape, cycle paths, …) are pinned by the unit tests inside
//! `tools/plan.rs`; this file covers the DB-visible behavior: rows,
//! whole-batch atomicity, `plan.updated` emission, cancel semantics,
//! role gating at the MCP entry, list projection, and the #644
//! `WavePatch` fields.

use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::ids::{CardId, CoveId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::plan::{TOOL_PLAN_CANCEL, TOOL_PLAN_LIST, TOOL_PLAN_UPSERT};
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, WaveLifecycle, WavePatch};
use calm_server::plugin_host::mcp::RpcError;
use serde_json::{Value, json};

struct Boot {
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    repo: Arc<dyn Repo>,
    cove_id: CoveId,
    wave_id: WaveId,
    spec_card_id: CardId,
    worker_card_id: CardId,
}

async fn boot() -> Boot {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "mcp-plan".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "initial".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "spec".into(),
            sort: None,
            payload: serde_json::Value::Null,
        })
        .await
        .unwrap();
    let worker_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: serde_json::Value::Null,
        })
        .await
        .unwrap();

    // PR-C activated rule 6 and new waves default `require_task_gates
    // = 1` (migration 0041 DB DEFAULT) — most of this suite plans
    // ungated codex tasks, so the boot wave opts out; the dedicated
    // rule-6 matrix test flips the flag back on.
    repo.wave_update(
        wave.id.as_str(),
        WavePatch {
            require_task_gates: Some(false),
            ..Default::default()
        },
    )
    .await
    .expect("boot wave opts out of rule 6");

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    card_role_cache.insert(worker_card.id.clone(), CardRole::Worker, wave.id.clone());

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        wave_vcs: repo
            .sqlite_pool()
            .map(calm_truth::wave_vcs_repo::SqlxWaveVcsRepo::shared),
        events,
        write: calm_server::state::WriteContext::new(card_role_cache, wave_cove_cache),
        daemon_token_hash: None,
        gate_logs_dir: std::env::temp_dir().join("neige-test-gate-logs"),
    });

    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    let registry = Arc::new(registry);

    Boot {
        ctx,
        registry,
        repo,
        cove_id: cove.id,
        wave_id: wave.id,
        spec_card_id: spec_card.id,
        worker_card_id: worker_card.id,
    }
}

async fn call_tool(
    boot: &Boot,
    name: &str,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let handler = boot
        .registry
        .lookup(name)
        .unwrap_or_else(|| panic!("tool not registered: {name}"));
    handler(boot.ctx.clone(), identity, args).await
}

fn spec_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        session_id: "spec-session".to_string(),
        wave_id: Some(boot.wave_id.as_str().to_string()),
        cove_id: boot.cove_id.as_str().to_string(),
        thread_id: "spec-thread".to_string(),
    }
}

fn worker_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.worker_card_id.as_str().to_string(),
        role: CardRole::Worker,
        session_id: "worker-session".to_string(),
        wave_id: Some(boot.wave_id.as_str().to_string()),
        cove_id: boot.cove_id.as_str().to_string(),
        thread_id: "worker-thread".to_string(),
    }
}

async fn set_wave_lifecycle(boot: &Boot, lifecycle: WaveLifecycle) {
    boot.repo
        .wave_update(
            boot.wave_id.as_str(),
            WavePatch {
                lifecycle: Some(lifecycle),
                ..Default::default()
            },
        )
        .await
        .expect("set test wave lifecycle");
}

/// Direct SQL escape hatch for states the PR-A tool surface cannot
/// produce (in-flight statuses, gate_json rows — both PR-B/PR-C
/// territory).
async fn exec_sql(boot: &Boot, sql: &str) {
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    sqlx::query(sql).execute(&pool).await.expect("exec sql");
}

fn upsert_args(tasks: Value) -> Value {
    json!({ "tasks": tasks, "message": "test plan revision" })
}

async fn upsert_ok(boot: &Boot, tasks: Value) -> Value {
    call_tool(
        boot,
        TOOL_PLAN_UPSERT,
        spec_identity(boot),
        upsert_args(tasks),
    )
    .await
    .expect("plan.upsert ok")
}

/// Count surviving `tasks` rows for the boot wave directly — after a
/// wave/cove delete the repo read path would trivially return empty, so
/// orphan detection must go to the table.
async fn task_row_count(boot: &Boot) -> i64 {
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tasks WHERE wave_id = ?1")
        .bind(boot.wave_id.as_str())
        .fetch_one(&pool)
        .await
        .expect("count tasks");
    count
}

/// Drain every envelope the bus delivers within a short quiet window.
async fn drain_events(
    rx: &mut tokio::sync::broadcast::Receiver<calm_server::event::BroadcastEnvelope>,
) -> Vec<Event> {
    let mut seen = Vec::new();
    while let Ok(Ok(env)) =
        tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
    {
        seen.push(env.event);
    }
    seen
}

fn outcomes_of(resp: &Value) -> Vec<(String, String)> {
    resp["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| {
            (
                r["key"].as_str().unwrap().to_string(),
                r["outcome"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// migration 0041 + WavePatch fields
// ---------------------------------------------------------------------------

#[tokio::test]
async fn migration_0041_new_wave_defaults_gates_on_and_budget_null() {
    let boot = boot().await;
    // The boot wave opts out of rule 6 for the rest of the suite —
    // assert the DB DEFAULT on a FRESH wave instead.
    let fresh = boot
        .repo
        .wave_create(calm_server::model::NewWave {
            cove_id: boot.cove_id.clone(),
            title: "defaults".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("fresh wave");
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let (require_gates, budget): (i64, Option<i64>) =
        sqlx::query_as("SELECT require_task_gates, task_budget FROM waves WHERE id = ?1")
            .bind(fresh.id.as_str())
            .fetch_one(&pool)
            .await
            .expect("read wave policy columns");
    assert_eq!(
        require_gates, 1,
        "post-migration waves default require_task_gates = 1 via the DB DEFAULT"
    );
    assert_eq!(
        budget, None,
        "task_budget defaults to NULL (kernel default)"
    );
}

#[tokio::test]
async fn wave_patch_persists_task_budget_and_require_task_gates() {
    let boot = boot().await;
    boot.repo
        .wave_update(
            boot.wave_id.as_str(),
            WavePatch {
                task_budget: Some(Some(3)),
                require_task_gates: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("patch persists");

    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let (require_gates, budget): (i64, Option<i64>) =
        sqlx::query_as("SELECT require_task_gates, task_budget FROM waves WHERE id = ?1")
            .bind(boot.wave_id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(budget, Some(3));
    assert_eq!(require_gates, 0);

    // `Some(None)` clears the budget back to the kernel default; an
    // omitted field leaves the other column alone.
    boot.repo
        .wave_update(
            boot.wave_id.as_str(),
            WavePatch {
                task_budget: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (require_gates, budget): (i64, Option<i64>) =
        sqlx::query_as("SELECT require_task_gates, task_budget FROM waves WHERE id = ?1")
            .bind(boot.wave_id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(budget, None);
    assert_eq!(require_gates, 0, "untouched by the second patch");
}

// ---------------------------------------------------------------------------
// calm.plan.upsert — happy path + idempotency + events
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upsert_creates_rows_and_emits_plan_updated() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    let mut rx = boot.ctx.events.subscribe();

    let resp = upsert_ok(
        &boot,
        json!([
            { "key": "write-spec", "kind": "codex", "goal": "write the spec" },
            {
                "key": "impl-parser",
                "kind": "codex",
                "goal": "implement the parser",
                "depends_on": ["write-spec"],
                "priority": 10,
                "context": { "hint": "x" },
                "acceptance_criteria": "tests pass"
            },
            { "key": "smoke", "kind": "terminal", "goal": "cargo test", "cwd": "/repo" }
        ]),
    )
    .await;
    assert_eq!(
        outcomes_of(&resp),
        vec![
            ("write-spec".to_string(), "created".to_string()),
            ("impl-parser".to_string(), "created".to_string()),
            ("smoke".to_string(), "created".to_string()),
        ]
    );

    // Rows persisted with pending status, composed ids, normalized deps.
    let tasks = boot
        .repo
        .tasks_by_wave(boot.wave_id.as_str())
        .await
        .unwrap();
    assert_eq!(tasks.len(), 3);
    let parser = tasks.iter().find(|t| t.key == "impl-parser").unwrap();
    assert_eq!(parser.id, format!("{}:impl-parser", boot.wave_id.as_str()));
    assert_eq!(parser.priority, 10);
    assert_eq!(parser.depends_on(), vec!["write-spec"]);
    assert_eq!(parser.acceptance_criteria.as_deref(), Some("tests pass"));
    assert!(parser.gate_json.is_none());
    // Highest priority sorts first (scheduler order).
    assert_eq!(tasks[0].key, "impl-parser");

    // One plan.updated event, wave-scoped, AiSpec actor, all keys changed.
    let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers")
        .expect("bus open");
    match envelope.event {
        Event::PlanUpdated {
            wave_id,
            changed_keys,
            agent_message,
        } => {
            assert_eq!(wave_id, boot.wave_id);
            assert_eq!(changed_keys, vec!["write-spec", "impl-parser", "smoke"]);
            assert_eq!(agent_message.as_deref(), Some("test plan revision"));
        }
        other => panic!("expected PlanUpdated, got {other:?}"),
    }
    assert!(
        matches!(envelope.actor, calm_server::ids::ActorId::AiSpec(_)),
        "actor = {:?}",
        envelope.actor
    );
}

#[tokio::test]
async fn upsert_identical_batch_is_unchanged_and_emits_nothing() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    let tasks = json!([
        { "key": "a", "kind": "codex", "goal": "g", "depends_on": [] },
    ]);
    upsert_ok(&boot, tasks.clone()).await;

    let mut rx = boot.ctx.events.subscribe();
    let resp = upsert_ok(&boot, tasks).await;
    assert_eq!(
        outcomes_of(&resp),
        vec![("a".to_string(), "unchanged".to_string())]
    );
    let no_event = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        no_event.is_err(),
        "idempotent retry must not emit: {no_event:?}"
    );
}

#[tokio::test]
async fn upsert_revises_pending_row_and_reports_updated() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    upsert_ok(&boot, json!([{ "key": "a", "kind": "codex", "goal": "g" }])).await;

    let mut rx = boot.ctx.events.subscribe();
    let resp = upsert_ok(
        &boot,
        json!([{ "key": "a", "kind": "codex", "goal": "revised goal", "priority": 5 }]),
    )
    .await;
    assert_eq!(
        outcomes_of(&resp),
        vec![("a".to_string(), "updated".to_string())]
    );
    let row = boot
        .repo
        .task_get(&format!("{}:a", boot.wave_id.as_str()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.goal, "revised goal");
    assert_eq!(row.priority, 5);

    let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers")
        .expect("bus open");
    match envelope.event {
        Event::PlanUpdated { changed_keys, .. } => assert_eq!(changed_keys, vec!["a"]),
        other => panic!("expected PlanUpdated, got {other:?}"),
    }
}

#[tokio::test]
async fn upsert_batch_is_atomic_one_bad_task_rolls_back_all() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;

    let err = call_tool(
        &boot,
        TOOL_PLAN_UPSERT,
        spec_identity(&boot),
        upsert_args(json!([
            { "key": "good", "kind": "codex", "goal": "fine" },
            { "key": "bad", "kind": "codex", "goal": "dep missing", "depends_on": ["ghost"] }
        ])),
    )
    .await
    .expect_err("unknown dep rejects whole batch");
    assert_eq!(err.code, -32602);
    assert!(
        err.message.contains("unknown dependency `ghost`"),
        "{err:?}"
    );

    let tasks = boot
        .repo
        .tasks_by_wave(boot.wave_id.as_str())
        .await
        .unwrap();
    assert!(tasks.is_empty(), "no row from a failed batch: {tasks:?}");
}

#[tokio::test]
async fn upsert_non_pending_same_payload_unchanged_different_payload_rejected() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    let tasks = json!([{ "key": "a", "kind": "codex", "goal": "g" }]);
    upsert_ok(&boot, tasks.clone()).await;
    exec_sql(&boot, "UPDATE tasks SET status = 'running' WHERE key = 'a'").await;

    // Identical payload → idempotent `unchanged`.
    let resp = upsert_ok(&boot, tasks).await;
    assert_eq!(
        outcomes_of(&resp),
        vec![("a".to_string(), "unchanged".to_string())]
    );

    // Different payload → rule-5 mutability refusal.
    let err = call_tool(
        &boot,
        TOOL_PLAN_UPSERT,
        spec_identity(&boot),
        upsert_args(json!([{ "key": "a", "kind": "codex", "goal": "different" }])),
    )
    .await
    .expect_err("non-pending revise refused");
    assert_eq!(err.code, -32602);
    assert!(
        err.message
            .contains("task a already dispatched; insert a new task instead"),
        "{err:?}"
    );
}

#[tokio::test]
async fn upsert_validation_errors_surface_as_invalid_params() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;

    for (tasks, needle) in [
        // Rule 1 — key regex.
        (
            json!([{ "key": "Bad-Key", "kind": "codex", "goal": "g" }]),
            "invalid task key `Bad-Key`",
        ),
        // Rule 1 — duplicate key in batch.
        (
            json!([
                { "key": "a", "kind": "codex", "goal": "g" },
                { "key": "a", "kind": "codex", "goal": "g2" }
            ]),
            "duplicate key `a` in batch",
        ),
        // Rule 2 — claude not yet supported.
        (
            json!([{ "key": "a", "kind": "claude", "goal": "g" }]),
            "not yet supported",
        ),
        // Rule 4 — cycle with path.
        (
            json!([
                { "key": "a", "kind": "codex", "goal": "g", "depends_on": ["b"] },
                { "key": "b", "kind": "codex", "goal": "g", "depends_on": ["a"] }
            ]),
            "dependency cycle: a -> b -> a",
        ),
        // Rule 7 — relative cwd.
        (
            json!([{ "key": "a", "kind": "terminal", "goal": "ls", "cwd": "rel/path" }]),
            "must be an absolute path",
        ),
        // Rule 7 — control chars in gate cmd.
        (
            json!([{
                "key": "a", "kind": "codex", "goal": "g",
                "gate": { "steps": [ { "name": "t", "cmd": "cargo\u{0007}test" } ] }
            }]),
            "ASCII control",
        ),
        // Rule 7 — empty gate steps (rule 8 was deleted in PR-C; the
        // shape contract still rejects degenerate gates).
        (
            json!([{
                "key": "a", "kind": "codex", "goal": "g",
                "gate": { "steps": [] }
            }]),
            "gate.steps must be non-empty",
        ),
    ] {
        let err = call_tool(
            &boot,
            TOOL_PLAN_UPSERT,
            spec_identity(&boot),
            upsert_args(tasks.clone()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, -32602, "tasks={tasks}: {err:?}");
        assert!(
            err.message.contains(needle),
            "tasks={tasks}: expected `{needle}` in {err:?}"
        );
        let rows = boot
            .repo
            .tasks_by_wave(boot.wave_id.as_str())
            .await
            .unwrap();
        assert!(rows.is_empty(), "rejected batch left rows: {rows:?}");
    }
}

#[tokio::test]
async fn upsert_auto_promotes_draft_wave() {
    let boot = boot().await;
    // Wave starts in Draft; the first spec plan write auto-promotes to
    // Planning in the same tx (parity with the other spec write tools).
    upsert_ok(&boot, json!([{ "key": "a", "kind": "codex", "goal": "g" }])).await;
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Planning);
}

#[tokio::test]
async fn upsert_all_unchanged_with_lifecycle_applies_lifecycle_without_plan_updated() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    let tasks = json!([{ "key": "a", "kind": "codex", "goal": "g" }]);
    upsert_ok(&boot, tasks.clone()).await;

    let mut rx = boot.ctx.events.subscribe();
    let resp = call_tool(
        &boot,
        TOOL_PLAN_UPSERT,
        spec_identity(&boot),
        json!({ "tasks": tasks, "message": "plan is final", "lifecycle": "dispatching" }),
    )
    .await
    .expect("all-unchanged upsert with lifecycle ok");
    assert_eq!(
        outcomes_of(&resp),
        vec![("a".to_string(), "unchanged".to_string())]
    );

    // The lifecycle landed…
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Dispatching);

    // …with its own events, but no `plan.updated` (the plan rows did
    // not change; empty `changed_keys` must never be emitted).
    let events = drain_events(&mut rx).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::WaveLifecycleChanged { .. })),
        "lifecycle event missing: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::PlanUpdated { .. })),
        "all-unchanged batch emitted plan.updated: {events:?}"
    );
}

/// Review round 3 (#656 F1): a spec retrying the exact same call —
/// identical batch + `lifecycle` equal to the wave's current state —
/// is fully idempotent. It must short-circuit to success (all
/// `unchanged`, zero events) instead of reaching the tx with an empty
/// event batch, which `write_with_actor_events` rejects as an internal
/// error.
#[tokio::test]
async fn upsert_identical_batch_with_same_state_lifecycle_is_idempotent_success() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    let tasks = json!([{ "key": "a", "kind": "codex", "goal": "g" }]);
    let args = json!({ "tasks": tasks, "message": "plan is final", "lifecycle": "dispatching" });
    call_tool(&boot, TOOL_PLAN_UPSERT, spec_identity(&boot), args.clone())
        .await
        .expect("first upsert with lifecycle ok");

    // Retry the exact same call: batch is identical, wave is already
    // `dispatching`.
    let mut rx = boot.ctx.events.subscribe();
    let resp = call_tool(&boot, TOOL_PLAN_UPSERT, spec_identity(&boot), args)
        .await
        .expect("idempotent retry with same-state lifecycle must succeed");
    assert_eq!(
        outcomes_of(&resp),
        vec![("a".to_string(), "unchanged".to_string())]
    );

    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Dispatching);

    let events = drain_events(&mut rx).await;
    assert!(
        events.is_empty(),
        "idempotent retry must emit nothing: {events:?}"
    );
}

// ---------------------------------------------------------------------------
// calm.plan.cancel
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_pending_task_flips_row_and_emits_plan_updated() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    upsert_ok(&boot, json!([{ "key": "a", "kind": "codex", "goal": "g" }])).await;

    let mut rx = boot.ctx.events.subscribe();
    let out = call_tool(
        &boot,
        TOOL_PLAN_CANCEL,
        spec_identity(&boot),
        json!({ "key": "a", "message": "obsolete" }),
    )
    .await
    .expect("cancel ok");
    assert_eq!(out["ok"], true);

    let row = boot
        .repo
        .task_get(&format!("{}:a", boot.wave_id.as_str()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::to_value(row.status).unwrap(), json!("canceled"));
    assert!(row.finished_at_ms.is_some());

    let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers")
        .expect("bus open");
    match envelope.event {
        Event::PlanUpdated { changed_keys, .. } => assert_eq!(changed_keys, vec!["a"]),
        other => panic!("expected PlanUpdated, got {other:?}"),
    }

    // Second cancel — idempotent success, no event.
    let mut rx = boot.ctx.events.subscribe();
    let out = call_tool(
        &boot,
        TOOL_PLAN_CANCEL,
        spec_identity(&boot),
        json!({ "key": "a", "message": "retry" }),
    )
    .await
    .expect("idempotent cancel ok");
    assert_eq!(out["ok"], true);
    let no_event = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(no_event.is_err(), "idempotent cancel emitted: {no_event:?}");
}

/// Review F2/F3 (#656): an already-`canceled` task + a `lifecycle` arg
/// must not short-circuit before the lifecycle applies. This also pins
/// the in-tx re-read: the guarded UPDATE flips 0 rows (the row is
/// already `canceled` — same branch a lost cancel/cancel race lands
/// in), the re-read classifies it as idempotent success, and no
/// `plan.updated` is emitted.
#[tokio::test]
async fn cancel_already_canceled_with_lifecycle_applies_lifecycle_without_plan_updated() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    upsert_ok(&boot, json!([{ "key": "a", "kind": "codex", "goal": "g" }])).await;
    call_tool(
        &boot,
        TOOL_PLAN_CANCEL,
        spec_identity(&boot),
        json!({ "key": "a", "message": "obsolete" }),
    )
    .await
    .expect("first cancel ok");

    let mut rx = boot.ctx.events.subscribe();
    let out = call_tool(
        &boot,
        TOOL_PLAN_CANCEL,
        spec_identity(&boot),
        json!({ "key": "a", "message": "plan empty, moving on", "lifecycle": "dispatching" }),
    )
    .await
    .expect("idempotent cancel with lifecycle ok");
    assert_eq!(out["ok"], true);

    // Row untouched, lifecycle applied.
    let row = boot
        .repo
        .task_get(&format!("{}:a", boot.wave_id.as_str()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::to_value(row.status).unwrap(), json!("canceled"));
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Dispatching);

    // Lifecycle events land; `plan.updated` is suppressed (nothing in
    // the plan changed, a retry must not re-trigger the scheduler).
    let events = drain_events(&mut rx).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::WaveLifecycleChanged { .. })),
        "lifecycle event missing: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::PlanUpdated { .. })),
        "idempotent cancel emitted plan.updated: {events:?}"
    );
}

/// Review round 3 (#656 F2): re-cancel of an already-`canceled` task
/// with a `lifecycle` equal to the wave's current state is a fully
/// idempotent retry — success, zero events — instead of falling into
/// the tx where the 0-row flip plus the same-state lifecycle would
/// produce an empty event batch (rejected by `write_with_actor_events`
/// as an internal error).
#[tokio::test]
async fn cancel_already_canceled_with_same_state_lifecycle_is_idempotent_success() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    upsert_ok(&boot, json!([{ "key": "a", "kind": "codex", "goal": "g" }])).await;
    let args =
        json!({ "key": "a", "message": "plan empty, moving on", "lifecycle": "dispatching" });
    call_tool(&boot, TOOL_PLAN_CANCEL, spec_identity(&boot), args.clone())
        .await
        .expect("first cancel with lifecycle ok");

    // Retry the exact same call: row already `canceled`, wave already
    // `dispatching`.
    let mut rx = boot.ctx.events.subscribe();
    let out = call_tool(&boot, TOOL_PLAN_CANCEL, spec_identity(&boot), args)
        .await
        .expect("idempotent re-cancel with same-state lifecycle must succeed");
    assert_eq!(out["ok"], true);

    let row = boot
        .repo
        .task_get(&format!("{}:a", boot.wave_id.as_str()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::to_value(row.status).unwrap(), json!("canceled"));
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Dispatching);

    let events = drain_events(&mut rx).await;
    assert!(
        events.is_empty(),
        "idempotent re-cancel must emit nothing: {events:?}"
    );
}

#[tokio::test]
async fn cancel_in_flight_task_refused_with_409_text() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    upsert_ok(&boot, json!([{ "key": "a", "kind": "codex", "goal": "g" }])).await;

    for status in ["dispatched", "running", "verifying"] {
        exec_sql(
            &boot,
            &format!("UPDATE tasks SET status = '{status}' WHERE key = 'a'"),
        )
        .await;
        let err = call_tool(
            &boot,
            TOOL_PLAN_CANCEL,
            spec_identity(&boot),
            json!({ "key": "a", "message": "too late" }),
        )
        .await
        .expect_err("in-flight cancel refused");
        assert_eq!(err.code, -32409, "status={status}: {err:?}");
        assert!(
            err.message.contains("task a is in-flight")
                && err.message.contains("out of scope (#644)")
                && err
                    .message
                    .contains("Cancel or rewire its successors instead"),
            "status={status}: {err:?}"
        );
    }
}

#[tokio::test]
async fn cancel_terminal_or_unknown_task_rejected() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    upsert_ok(&boot, json!([{ "key": "a", "kind": "codex", "goal": "g" }])).await;
    exec_sql(&boot, "UPDATE tasks SET status = 'done' WHERE key = 'a'").await;

    let err = call_tool(
        &boot,
        TOOL_PLAN_CANCEL,
        spec_identity(&boot),
        json!({ "key": "a", "message": "m" }),
    )
    .await
    .expect_err("done task can't be canceled");
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("only pending tasks"), "{err:?}");

    let err = call_tool(
        &boot,
        TOOL_PLAN_CANCEL,
        spec_identity(&boot),
        json!({ "key": "ghost", "message": "m" }),
    )
    .await
    .expect_err("unknown task");
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("unknown task `ghost`"), "{err:?}");
}

// ---------------------------------------------------------------------------
// delete cleanup — `tasks` has no FK to `waves` (review F1, #656)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wave_delete_removes_plan_rows() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    upsert_ok(
        &boot,
        json!([
            { "key": "a", "kind": "codex", "goal": "g" },
            { "key": "b", "kind": "terminal", "goal": "cargo test" }
        ]),
    )
    .await;
    assert_eq!(task_row_count(&boot).await, 2);

    boot.repo
        .wave_delete(boot.wave_id.as_str())
        .await
        .expect("wave delete");
    assert_eq!(
        task_row_count(&boot).await,
        0,
        "wave delete must not orphan plan rows"
    );
}

#[tokio::test]
async fn cove_delete_removes_plan_rows() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    upsert_ok(&boot, json!([{ "key": "a", "kind": "codex", "goal": "g" }])).await;
    assert_eq!(task_row_count(&boot).await, 1);

    boot.repo
        .cove_delete(boot.cove_id.as_str())
        .await
        .expect("cove delete");
    assert_eq!(
        task_row_count(&boot).await,
        0,
        "cove delete must not orphan plan rows"
    );
}

/// The upsert tx re-checks the wave row in-tx (no FK backs it), so a
/// wave deleted between the tool's resolve and the write surfaces as a
/// conflict instead of inserting plan rows for a dead wave. Pinned at
/// the tx layer — the tool layer can't interleave a delete mid-call.
#[tokio::test]
async fn upsert_wave_guard_refuses_deleted_wave_in_tx() {
    let boot = boot().await;
    boot.repo
        .wave_delete(boot.wave_id.as_str())
        .await
        .expect("wave delete");

    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let mut tx = pool.begin().await.expect("begin tx");
    let err = calm_server::db::sqlite::require_wave_exists_tx(&mut tx, boot.wave_id.as_str())
        .await
        .expect_err("deleted wave refused");
    assert!(
        matches!(err, calm_server::error::CalmError::Conflict(_)),
        "expected Conflict, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// calm.plan.list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_returns_plan_shape_without_gate_commands() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    upsert_ok(
        &boot,
        json!([
            { "key": "a", "kind": "codex", "goal": "g" },
            { "key": "b", "kind": "terminal", "goal": "cargo test", "depends_on": ["a"] }
        ]),
    )
    .await;
    // Simulate a PR-C era row carrying a gate so the projection's
    // no-command contract is pinned now.
    exec_sql(
        &boot,
        r#"UPDATE tasks SET gate_json = '{"steps":[{"name":"fmt","cmd":"cargo fmt --check"},{"name":"test","cmd":"cargo test --secret"}],"timeout_secs":600}' WHERE key = 'a'"#,
    )
    .await;

    let out = call_tool(&boot, TOOL_PLAN_LIST, spec_identity(&boot), json!({}))
        .await
        .expect("list ok");
    let tasks = out["tasks"].as_array().expect("tasks array");
    assert_eq!(tasks.len(), 2);

    let a = tasks
        .iter()
        .find(|t| t["key"] == "a")
        .expect("task a listed");
    assert_eq!(a["kind"], "codex");
    assert_eq!(a["status"], "pending");
    assert_eq!(a["gate"]["present"], true);
    assert_eq!(a["gate"]["steps"], json!(["fmt", "test"]));
    assert_eq!(a["gate_result"], Value::Null);
    let rendered = out.to_string();
    assert!(
        !rendered.contains("cargo fmt") && !rendered.contains("--secret"),
        "gate commands leaked: {rendered}"
    );

    let b = tasks
        .iter()
        .find(|t| t["key"] == "b")
        .expect("task b listed");
    assert_eq!(b["gate"]["present"], false);
    assert_eq!(b["depends_on"], json!(["a"]));
    assert_eq!(b["worker_card_id"], Value::Null);
}

// ---------------------------------------------------------------------------
// role gating
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plan_tools_refuse_worker_callers_at_mcp_entry() {
    let boot = boot().await;
    for (tool, args) in [
        (
            TOOL_PLAN_UPSERT,
            upsert_args(json!([{ "key": "a", "kind": "codex", "goal": "g" }])),
        ),
        (TOOL_PLAN_CANCEL, json!({ "key": "a", "message": "m" })),
        (TOOL_PLAN_LIST, json!({})),
    ] {
        let err = call_tool(&boot, tool, worker_identity(&boot), args)
            .await
            .expect_err("worker refused");
        assert_eq!(err.code, -32602, "{tool}: {err:?}");
        assert!(err.message.contains("Spec"), "{tool}: {err:?}");
    }
    let tasks = boot
        .repo
        .tasks_by_wave(boot.wave_id.as_str())
        .await
        .unwrap();
    assert!(tasks.is_empty(), "worker call wrote rows: {tasks:?}");
}

// ---------------------------------------------------------------------------
// PR-C — rule 6 matrix (§4.1/§6.6) + gate acceptance (rule 8 deleted)
// ---------------------------------------------------------------------------

async fn set_require_gates(boot: &Boot, on: bool) {
    boot.repo
        .wave_update(
            boot.wave_id.as_str(),
            WavePatch {
                require_task_gates: Some(on),
                ..Default::default()
            },
        )
        .await
        .expect("set require_task_gates");
}

#[tokio::test]
async fn rule6_matrix_require_task_gates() {
    let boot = boot().await;

    // Flag OFF — an ungated codex task is accepted (the suite default).
    upsert_ok(
        &boot,
        json!([{ "key": "off-ok", "kind": "codex", "goal": "g" }]),
    )
    .await;

    set_require_gates(&boot, true).await;

    // Ungated codex task → rejected, whole batch atomic.
    let err = call_tool(
        &boot,
        TOOL_PLAN_UPSERT,
        spec_identity(&boot),
        upsert_args(json!([
            { "key": "gated", "kind": "codex", "goal": "g",
              "gate": { "steps": [ { "name": "t", "cmd": "true" } ] } },
            { "key": "naked", "kind": "codex", "goal": "g" }
        ])),
    )
    .await
    .expect_err("rule 6 rejects the ungated codex task");
    assert!(err.message.contains("rule 6"), "{err:?}");
    assert!(err.message.contains("naked"), "{err:?}");
    let rows = boot
        .repo
        .tasks_by_wave(boot.wave_id.as_str())
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "rejected batch must not land its gated sibling either (atomic): {rows:?}"
    );

    // codex + gate → accepted, gate_json stored canonically.
    upsert_ok(
        &boot,
        json!([{ "key": "gated", "kind": "codex", "goal": "g",
                 "gate": { "steps": [ { "name": "t", "cmd": "true" } ] } }]),
    )
    .await;
    let row = boot
        .repo
        .task_get(&format!("{}:gated", boot.wave_id.as_str()))
        .await
        .unwrap()
        .expect("gated row");
    let gate_json = row.gate_json.expect("gate stored (rule 8 deleted)");
    assert!(gate_json.contains("\"cmd\":\"true\""), "{gate_json}");

    // codex + no_gate_reason → accepted; the reason rides context_json.
    upsert_ok(
        &boot,
        json!([{ "key": "excused", "kind": "codex", "goal": "g",
                 "no_gate_reason": "docs only" }]),
    )
    .await;
    let row = boot
        .repo
        .task_get(&format!("{}:excused", boot.wave_id.as_str()))
        .await
        .unwrap()
        .expect("excused row");
    assert!(
        row.context_json.contains("docs only"),
        "{}",
        row.context_json
    );

    // Round-3 review F2 — a blank escape hatch is not an escape
    // hatch: empty/whitespace `no_gate_reason` is rejected instead of
    // excusing the ungated codex task with an empty audit note.
    for blank in ["", "  "] {
        let err = call_tool(
            &boot,
            TOOL_PLAN_UPSERT,
            spec_identity(&boot),
            upsert_args(json!([
                { "key": "blank-excuse", "kind": "codex", "goal": "g",
                  "no_gate_reason": blank }
            ])),
        )
        .await
        .expect_err("blank no_gate_reason must be rejected");
        assert!(
            err.message
                .contains("`no_gate_reason` must be a non-empty reason"),
            "blank {blank:?}: {err:?}"
        );
    }
    assert!(
        boot.repo
            .task_get(&format!("{}:blank-excuse", boot.wave_id.as_str()))
            .await
            .unwrap()
            .is_none(),
        "rejected blank-reason task must not land"
    );

    // terminal ungated → exempt.
    upsert_ok(
        &boot,
        json!([{ "key": "term", "kind": "terminal", "goal": "true" }]),
    )
    .await;

    // Unchanged passthrough: the pre-flag ungated codex row resubmitted
    // byte-identically stays `unchanged` and is NOT re-policed —
    // idempotent retries of older plans keep working.
    let out = upsert_ok(
        &boot,
        json!([{ "key": "off-ok", "kind": "codex", "goal": "g" }]),
    )
    .await;
    assert_eq!(out["results"][0]["outcome"], "unchanged", "{out}");
}

#[tokio::test]
async fn plan_list_hides_gate_commands_but_shows_step_names() {
    let boot = boot().await;
    upsert_ok(
        &boot,
        json!([{ "key": "gated", "kind": "codex", "goal": "g",
                 "gate": { "steps": [ { "name": "fmt", "cmd": "cargo fmt --check" },
                                       { "name": "test", "cmd": "cargo test -p secret" } ] } }]),
    )
    .await;
    let out = call_tool(&boot, TOOL_PLAN_LIST, spec_identity(&boot), json!({}))
        .await
        .expect("plan.list");
    let listed = out["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .find(|t| t["key"] == "gated")
        .expect("gated entry")
        .clone();
    assert_eq!(listed["gate"]["present"], true, "{listed}");
    assert_eq!(listed["gate"]["steps"], json!(["fmt", "test"]), "{listed}");
    let rendered = out.to_string();
    assert!(
        !rendered.contains("cargo test -p secret"),
        "gate commands must never be echoed (§6.7): {rendered}"
    );
}
