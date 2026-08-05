//! Issue #644 PR-A — `mcp_server::tools::plan` integration coverage.
//!
//! Boots an in-memory `SqlxRepo` + `EventBus` + pre-seeded role caches,
//! constructs an `AppContext` directly (no live MCP listener — the
//! tools' contract is "given a `ToolCallIdentity` + `Value` args, do
//! the right thing"), and drives the hidden `calm.plan.upsert` shim plus
//! retained `calm.plan.cancel` / `calm.plan.list` end-to-end.
//!
//! Field-level validation details (key regex, kind vocabulary, gate
//! shape, cycle paths, …) are pinned by the unit tests inside
//! `tools/plan.rs`; this file covers shim zero-write behavior, cancel
//! semantics, role gating, list projection, and the #644 `WavePatch` fields.

use std::collections::BTreeMap;
use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::{Event, EventBus};
use calm_server::ids::{CardId, CoveId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::plan::{
    TOOL_PLAN_CANCEL, TOOL_PLAN_LIST, TOOL_PLAN_UPSERT, plan_cancel_after_pre_read_for_test,
};
use calm_server::mcp_server::tools::wave_report_blocks::TOOL_REPORT_BLOCKS_UPSERT;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{
    CardRole, NewCard, NewCove, NewWave, TaskStatus, WaveLifecycle, WavePatch, now_ms,
};
use calm_server::operation::spec_harness_start_adapter::render_spec_developer_instructions_for_test;
use calm_server::plugin_host::Manifest;
use calm_server::plugin_host::mcp::RpcError;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::wave_report::WaveReportPayload;
use serde_json::{Value, json};

struct Boot {
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    repo: Arc<dyn Repo>,
    cove_id: CoveId,
    wave_id: WaveId,
    spec_card_id: CardId,
    worker_card_id: CardId,
    report_card_id: CardId,
}

async fn boot() -> Boot {
    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
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
            workflow_input: None,
            cove_id: cove.id.clone(),
            title: "initial".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "spec".into(),
            sort: None,
            payload: serde_json::Value::Null,
        })
        .await
        .unwrap();
    let worker_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::Value::Null,
        })
        .await
        .unwrap();
    let report_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "wave-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(WaveReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();

    // PR-C activated rule 6 and new waves default `require_task_gates
    // = 1` (migration 0041 DB DEFAULT) — most of this suite plans
    // ungated codex tasks, so the boot wave opts out. The complete
    // report-block admission matrix lives in task_projection_acceptance.
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
    card_role_cache.insert(
        report_card.id.clone(),
        CardRole::ReportCard,
        wave.id.clone(),
    );
    seed_runtime_session(
        &sqlx_repo,
        spec_card.id.as_str(),
        "spec-session",
        "spec-thread",
    )
    .await;
    sqlx::query("UPDATE waves SET root_session_id = 'spec-session' WHERE id = ?1")
        .bind(wave.id.as_str())
        .execute(sqlx_repo.pool())
        .await
        .expect("mark spec session as wave root");
    seed_runtime_session(
        &sqlx_repo,
        worker_card.id.as_str(),
        "worker-session",
        "worker-thread",
    )
    .await;

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
        plugin_host: Arc::new(tokio::sync::OnceCell::new()),
        operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
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
        report_card_id: report_card.id,
    }
}

async fn seed_runtime_session(repo: &SqlxRepo, card_id: &str, session_id: &str, thread_id: &str) {
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: session_id.to_string(),
            card_id: card_id.to_string(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: Some(thread_id.to_string()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
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
        provider: calm_server::session_projection_repo::AgentProvider::Codex,
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
        provider: calm_server::session_projection_repo::AgentProvider::Codex,
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

async fn write_task_block(boot: &Boot, mut payload: Value) -> Value {
    let object = payload.as_object_mut().expect("task payload object");
    object.insert("ready".into(), json!(true));
    object.insert("declared_by".into(), json!("spec"));
    let report = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .expect("report card");
    let report: WaveReportPayload = serde_json::from_value(report.payload).unwrap();
    call_tool(
        boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(boot),
        json!({"kind": "task", "payload": payload, "if_doc_rev": report.doc_rev}),
    )
    .await
    .expect("task block write")
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

/// Snapshot every SQLite table, including CRDT/VCS/operational tables and
/// sqlite_sequence. Values are losslessly represented with SQLite `quote()`;
/// table and row order are deterministic for exact before/after comparison.
async fn all_persistent_rows(boot: &Boot) -> BTreeMap<String, Vec<String>> {
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .fetch_all(&pool)
            .await
            .expect("list persistent tables");
    let mut snapshot = BTreeMap::new();
    for table in tables {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
                .bind(&table)
                .fetch_all(&pool)
                .await
                .unwrap_or_else(|error| panic!("inspect table {table}: {error}"));
        let quoted_columns = columns
            .iter()
            .map(|column| format!("quote(\"{}\")", column.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(", ");
        let quoted_table = table.replace('"', "\"\"");
        let sql = format!("SELECT json_array({quoted_columns}) FROM \"{quoted_table}\" ORDER BY 1");
        let rows = sqlx::query_scalar(&sql)
            .fetch_all(&pool)
            .await
            .unwrap_or_else(|error| panic!("snapshot table {table}: {error}"));
        snapshot.insert(table, rows);
    }
    snapshot
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
            workflow_input: None,
            cove_id: boot.cove_id.clone(),
            title: "defaults".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
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
// calm.plan.upsert hidden shim
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plan_upsert_shim_returns_migration_and_writes_nothing() {
    let boot = boot().await;
    let mut rx = boot.ctx.events.subscribe();
    let before = all_persistent_rows(&boot).await;

    // Deliberately invalid under the legacy schema: the shim must not parse it.
    let out = call_tool(&boot, TOOL_PLAN_UPSERT, spec_identity(&boot), json!(null))
        .await
        .expect("registered compatibility shim");

    assert!(out["error"].as_str().unwrap().contains("retired (#985)"));
    assert_eq!(out["migration"]["use"], "calm.report.blocks.upsert");
    assert!(
        out["migration"]["shape"]
            .as_str()
            .unwrap()
            .contains("ready: true")
    );
    assert_eq!(
        all_persistent_rows(&boot).await,
        before,
        "spec shim changed a persistent table"
    );
    assert!(
        drain_events(&mut rx).await.is_empty(),
        "spec shim broadcast an EventBus envelope"
    );
}

#[tokio::test]
async fn all_shipped_plan_template_items_round_trip_through_real_blocks_upsert_path() {
    let boot = boot().await;
    let manifest = Manifest::parse(include_str!("../../../../plugins/git-forge/manifest.json"))
        .expect("shipped git-forge manifest");
    let workflow = manifest
        .workflows
        .iter()
        .find(|workflow| workflow.id == "issue-development")
        .expect("issue-development workflow");
    let rendered =
        render_spec_developer_instructions_for_test("wave-template", Some(workflow), None);
    let template_json = rendered
        .split("## Bound Workflow Plan Template\n```json\n")
        .nth(1)
        .expect("bound plan heading")
        .split("\n```")
        .next()
        .expect("bound plan JSON");
    let items: Vec<Value> = serde_json::from_str(template_json).expect("rendered template JSON");
    assert_eq!(items.len(), 8, "shipped workflow template size drifted");
    let expected_payloads: Vec<Value> = workflow
        .plan_template
        .iter()
        .map(calm_server::mcp_server::tools::plan::plan_template_task_block_payload)
        .collect();
    assert_eq!(
        items, expected_payloads,
        "rendered JSON changed the payloads"
    );

    for payload in &items {
        let report = boot
            .repo
            .card_get(boot.report_card_id.as_str())
            .await
            .unwrap()
            .expect("report card");
        let report: WaveReportPayload = serde_json::from_value(report.payload).unwrap();
        call_tool(
            &boot,
            TOOL_REPORT_BLOCKS_UPSERT,
            spec_identity(&boot),
            json!({"kind": "task", "payload": payload, "if_doc_rev": report.doc_rev}),
        )
        .await
        .unwrap_or_else(|error| panic!("real upsert rejected {payload:#}: {error:?}"));
    }

    let projected = boot
        .repo
        .tasks_by_wave(boot.wave_id.as_str())
        .await
        .expect("read projected tasks");
    assert_eq!(projected.len(), 8, "every shipped item must project");
    for input in &workflow.plan_template {
        let row = boot
            .repo
            .task_get(&format!("{}:{}", boot.wave_id, input.key))
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{} did not project", input.key));
        assert_eq!(row.goal, input.goal, "{} goal", input.key);
        assert_eq!(
            row.acceptance_criteria, input.acceptance_criteria,
            "{} acceptance",
            input.key
        );
        assert_eq!(
            row.depends_on(),
            input.depends_on,
            "{} dependencies",
            input.key
        );
        assert_eq!(row.declared_by, "spec", "{} author", input.key);
    }
}

// ---------------------------------------------------------------------------
// calm.plan.cancel
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_pending_task_flips_row_and_emits_plan_updated() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    write_task_block(&boot, json!({ "key": "a", "kind": "codex", "goal": "g" })).await;

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
    write_task_block(&boot, json!({ "key": "a", "kind": "codex", "goal": "g" })).await;
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
    write_task_block(&boot, json!({ "key": "a", "kind": "codex", "goal": "g" })).await;
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
    write_task_block(&boot, json!({ "key": "a", "kind": "codex", "goal": "g" })).await;

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
async fn cancel_rechecks_pending_inside_transaction_after_concurrent_state_advance() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    write_task_block(
        &boot,
        json!({ "key": "race", "kind": "codex", "goal": "g" }),
    )
    .await;
    let task_id = format!("{}:race", boot.wave_id);
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let task_id_for_hook = task_id.clone();
    let mut rx = boot.ctx.events.subscribe();

    let err = plan_cancel_after_pre_read_for_test(
        boot.ctx.clone(),
        spec_identity(&boot),
        json!({"key": "race", "message": "too late", "lifecycle": "dispatching"}),
        move || async move {
            sqlx::query("UPDATE tasks SET status='running' WHERE id=?1")
                .bind(task_id_for_hook)
                .execute(&pool)
                .await
                .expect("advance task after cancel pre-read");
        },
    )
    .await
    .expect_err("in-tx pending guard must reject the advanced row");
    assert_eq!(err.code, -32409, "{err:?}");
    assert!(
        err.message.contains("changed state concurrently"),
        "{err:?}"
    );

    let row = boot.repo.task_get(&task_id).await.unwrap().unwrap();
    assert_eq!(row.status, TaskStatus::Running);
    assert!(row.finished_at_ms.is_none());
    assert_eq!(
        boot.repo
            .wave_get(boot.wave_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        WaveLifecycle::Planning
    );
    assert!(
        drain_events(&mut rx).await.is_empty(),
        "rejected race must emit no event"
    );
}

#[tokio::test]
async fn cancel_terminal_or_unknown_task_rejected() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    write_task_block(&boot, json!({ "key": "a", "kind": "codex", "goal": "g" })).await;
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
    write_task_block(&boot, json!({ "key": "a", "kind": "codex", "goal": "g" })).await;
    write_task_block(
        &boot,
        json!({ "key": "b", "kind": "terminal", "goal": "cargo test" }),
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
    write_task_block(&boot, json!({ "key": "a", "kind": "codex", "goal": "g" })).await;
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

// ---------------------------------------------------------------------------
// calm.plan.list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_returns_plan_shape_without_gate_commands() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    write_task_block(
        &boot,
        json!({ "key": "a", "kind": "codex", "goal": "g",
                "gate": {"steps": [{"name": "fmt", "cmd": "cargo fmt --check"},
                                      {"name": "test", "cmd": "cargo test --secret"}]}}),
    )
    .await;
    write_task_block(
        &boot,
        json!({ "key": "b", "kind": "terminal", "goal": "cargo test", "depends_on": ["a"] }),
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
    let mut rx = boot.ctx.events.subscribe();
    let before = all_persistent_rows(&boot).await;
    for (tool, args) in [
        (
            TOOL_PLAN_UPSERT,
            json!({"tasks": [{ "key": "a", "kind": "codex", "goal": "g" }], "message": "m"}),
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
    assert_eq!(
        all_persistent_rows(&boot).await,
        before,
        "unauthorized caller changed a persistent table"
    );
    assert!(
        drain_events(&mut rx).await.is_empty(),
        "unauthorized caller broadcast an EventBus envelope"
    );
}

#[tokio::test]
async fn plan_list_hides_gate_commands_but_shows_step_names() {
    let boot = boot().await;
    write_task_block(
        &boot,
        json!({ "key": "gated", "kind": "codex", "goal": "g",
                "gate": { "steps": [ { "name": "fmt", "cmd": "cargo fmt --check" },
                                      { "name": "test", "cmd": "cargo test -p secret" } ] } }),
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
