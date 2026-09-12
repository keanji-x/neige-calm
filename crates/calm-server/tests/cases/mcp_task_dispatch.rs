//! Production registry dispatch coverage; no provider or real Worker is started.
use crate::mcp_track_report::{
    Boot, assistant_identity, boot as report_boot, call_tool, planner_identity, worker_identity,
};
use calm_server::mcp_server::tools::task_dispatch::TOOL_TASK_DISPATCH;
use calm_server::track_report::TrackReportPayload;
use serde_json::{Value, json};

pub(super) async fn boot() -> Boot {
    let b = report_boot().await;
    // This older report fixture seeds session rows only. Exercise the production
    // runtime mirror to establish the current-card link required by dispatch.
    bind_planner(&b, &planner_identity(&b).session_id, false).await;
    b
}

pub(super) async fn bind_planner(b: &Boot, id: &str, supersede: bool) {
    use calm_server::db::sqlite::{
        begin_immediate_tx, session_start_runtime_tx, session_supersede_and_start_tx,
    };
    use calm_server::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind,
    };
    use calm_types::worker::WorkerSessionState;
    let pool = b.repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    let init = WorkerSessionInit {
        id: id.into(),
        card_id: b.planner_card_id.to_string(),
        kind: WorkerSessionKind::SharedPlanner,
        agent_provider: Some(AgentProvider::Codex),
        status: WorkerSessionState::Running,
        terminal_run_id: None,
        thread_id: None,
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        spawn_op_id: None,
        now_ms: calm_server::model::now_ms(),
    };
    if supersede {
        session_supersede_and_start_tx(&mut tx, &planner_identity(b).session_id, init)
            .await
            .unwrap();
    } else {
        session_start_runtime_tx(&mut tx, init).await.unwrap();
    }
    tx.commit().await.unwrap();
}

pub(super) fn args() -> Value {
    json!({"name":"Summarize release scope", "goal":"Write a concise release scope summary", "acceptance":"The completion report names the supported scope and exclusions", "executor":"codex", "workspace":"empty"})
}

pub(super) async fn dispatch(
    b: &Boot,
    args: Value,
) -> Result<Value, calm_server::plugin_host::mcp::RpcError> {
    call_tool(b, TOOL_TASK_DISPATCH, planner_identity(b), args).await
}
pub(super) async fn payload(b: &Boot) -> TrackReportPayload {
    serde_json::from_value(
        b.repo
            .card_get(b.report_card_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .payload,
    )
    .unwrap()
}
pub(super) async fn counts(b: &Boot) -> (i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM planner_dispatch_receipts),(SELECT count(*) FROM events),(SELECT count(*) FROM task_attempt_allocations)")
        .fetch_one(&b.repo.sqlite_pool().unwrap()).await.unwrap()
}
pub(super) async fn policy(b: &Boot, policy: &str, budget: i64) {
    sqlx::query("UPDATE tracks SET automation_policy=?1,task_budget=?2 WHERE id=?3")
        .bind(policy)
        .bind(budget)
        .bind(b.track_id.as_str())
        .execute(&b.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
}

/// Planner feedback #3 — once a task is declared, `calm.track.state.next`
/// also lists the task-scoped lifecycle carriers (verdict / cancel).
#[tokio::test]
async fn track_state_next_lists_verdict_and_cancel_once_a_task_is_declared() {
    use calm_server::mcp_server::tools::track_state::TOOL_TRACK_STATE;
    let b = boot().await;
    dispatch(&b, args()).await.unwrap();
    let state = call_tool(&b, TOOL_TRACK_STATE, planner_identity(&b), json!({}))
        .await
        .unwrap();
    assert_eq!(state["tasks_declared"], json!(1), "{state:?}");
    let next = state["next"].as_array().unwrap();
    assert!(!next.is_empty(), "{state:?}");
    for entry in next {
        assert_eq!(
            entry["via"],
            json!([
                "calm.report.write",
                "calm.report.edit",
                "calm.task.verdict",
                "calm.plan.cancel"
            ]),
            "{entry:?}"
        );
    }
}

#[tokio::test]
async fn dispatch_creates_planner_declaration_and_replays_exact_contract_without_writes() {
    let b = boot().await;
    policy(&b, "auto-declare", 3).await;
    call_tool(&b, "calm.report.blocks.upsert", planner_identity(&b),
        json!({"kind":"prose","markdown":"# Existing notes\nPreserve this unrelated report block.","if_doc_rev":0})).await.unwrap();
    let before = payload(&b).await;
    let first = dispatch(&b, args()).await.unwrap();
    let after = payload(&b).await;
    assert_eq!(after.doc_rev, before.doc_rev + 1);
    let blocks = after.blocks.as_ref().unwrap();
    for block in before.blocks.as_ref().unwrap() {
        let retained = blocks.iter().find(|other| other.id == block.id).unwrap();
        assert_eq!(retained.payload, block.payload);
    }
    let block = blocks
        .iter()
        .find(|block| block.id == first["receipt"]["block_id"])
        .unwrap();
    assert_eq!(block.payload["key"], first["receipt"]["task_key"]);
    assert_eq!(
        block.payload["declared_by"],
        calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR
    );
    assert_eq!(block.payload["ready"], true);
    assert_eq!(block.payload["acceptance"], args()["acceptance"]);
    assert_eq!(
        block.payload["context"]["neige_execution"]["version"],
        "isolated-codex-v1"
    );
    assert_eq!(first["current"]["task"]["status"], "pending");
    assert!(first["current"]["allocation"]["attempt_id"].is_string());
    let stored: String = sqlx::query_scalar("SELECT contract_json FROM planner_dispatch_receipts")
        .fetch_one(&b.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&stored).unwrap(), args());
    let committed = counts(&b).await;
    let mut retry_args = args();
    retry_args["name"] = json!("  Summarize release scope  ");
    let retry = dispatch(&b, retry_args).await.unwrap();
    assert_eq!(retry["receipt"], first["receipt"]);
    assert_eq!(counts(&b).await, committed);
    assert_eq!(payload(&b).await, after);
    // A new active session may replay even if the original response was lost.
    bind_planner(&b, "dispatch-successor", true).await;
    let mut current = planner_identity(&b);
    current.session_id = "dispatch-successor".into();
    current.thread_id = "card-bound".into();
    let retry = call_tool(&b, TOOL_TASK_DISPATCH, current, args())
        .await
        .unwrap();
    assert_eq!(retry["receipt"], first["receipt"]);
    assert_eq!(counts(&b).await, committed);
    assert_eq!(dispatch(&b, args()).await.unwrap_err().code, -32403);
}

#[tokio::test]
async fn dispatch_same_name_changed_contract_conflicts_and_names_are_exact() {
    let b = boot().await;
    let first = dispatch(&b, args()).await.unwrap();
    let saved = counts(&b).await;
    for field in ["goal", "acceptance"] {
        let mut changed = args();
        changed[field] = json!(format!("{} ", changed[field].as_str().unwrap()));
        assert_eq!(dispatch(&b, changed).await.unwrap_err().code, -32409);
        assert_eq!(counts(&b).await, saved);
    }
    let mut distinct = args();
    distinct["name"] = json!("summarize release scope");
    assert_ne!(
        dispatch(&b, distinct).await.unwrap()["receipt"]["task_key"],
        first["receipt"]["task_key"]
    );
}

#[tokio::test]
async fn dispatch_concurrent_same_name_converges_and_conflicting_contract_loses() {
    let b = boot().await;
    let (a, c) = tokio::join!(dispatch(&b, args()), dispatch(&b, args()));
    assert_eq!(a.unwrap()["receipt"], c.unwrap()["receipt"]);
    assert_eq!(counts(&b).await.0, 1);
    let mut a = args();
    a["name"] = json!("Second task");
    let mut c = a.clone();
    c["goal"] = json!("A different goal");
    let (a, c) = tokio::join!(dispatch(&b, a), dispatch(&b, c));
    assert!(a.is_ok() ^ c.is_ok());
    assert_eq!(a.err().or(c.err()).unwrap().code, -32409);
    assert_eq!(counts(&b).await.0, 2);
}

#[tokio::test]
async fn dispatch_rejects_unsupported_or_incomplete_contracts_without_writes() {
    let b = boot().await;
    let initial = counts(&b).await;
    let mut cases = Vec::new();
    for field in ["name", "goal", "acceptance", "executor", "workspace"] {
        let mut a = args();
        a.as_object_mut().unwrap().remove(field);
        cases.push(a);
    }
    for (field, value) in [
        ("name", json!(" \n ")),
        ("name", json!("bad\nname")),
        ("name", json!("界".repeat(67))),
        ("goal", json!(" ")),
        ("acceptance", json!(" ")),
        ("executor", json!("claude")),
        ("workspace", json!("file-input")),
        ("depends_on", json!([])),
        ("gate", json!({})),
        ("context", json!({})),
        ("key", json!("caller-key")),
        ("track_id", json!("foreign")),
    ] {
        let mut a = args();
        a[field] = value;
        cases.push(a);
    }
    for a in cases {
        assert_eq!(
            dispatch(&b, a.clone()).await.unwrap_err().code,
            -32602,
            "{a}"
        );
        assert_eq!(counts(&b).await, initial);
    }
}

#[tokio::test]
async fn dispatch_auth_checks_current_persisted_identity_before_create_and_replay() {
    let b = boot().await;
    for replay in [false, true] {
        if replay {
            dispatch(&b, args()).await.unwrap();
        }
        let initial = counts(&b).await;
        let mut foreign = planner_identity(&b);
        foreign.track_id = Some("other-track".into());
        let mut wrong_area = planner_identity(&b);
        wrong_area.area_id = "other-area".into();
        let mut stale = planner_identity(&b);
        stale.session_id = "retired-session".into();
        let mut spoofed = assistant_identity(&b);
        spoofed.role = calm_server::model::CardRole::Planner;
        for identity in [
            worker_identity(&b),
            assistant_identity(&b),
            foreign,
            wrong_area,
            stale,
            spoofed,
        ] {
            let expected = if identity.role == calm_server::model::CardRole::Planner {
                -32403
            } else {
                -32602
            };
            assert_eq!(
                call_tool(&b, TOOL_TASK_DISPATCH, identity, args())
                    .await
                    .unwrap_err()
                    .code,
                expected
            );
            assert_eq!(counts(&b).await, initial);
        }
        // DB role, rather than the cached/claimed role, remains authoritative.
        sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1")
            .bind(b.planner_card_id.as_str())
            .execute(&b.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        assert_eq!(dispatch(&b, args()).await.unwrap_err().code, -32403);
        sqlx::query("UPDATE cards SET role='planner' WHERE id=?1")
            .bind(b.planner_card_id.as_str())
            .execute(&b.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        assert_eq!(counts(&b).await, initial);
    }
    sqlx::query("UPDATE worker_sessions SET state='superseded' WHERE id=?1")
        .bind(planner_identity(&b).session_id)
        .execute(&b.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(dispatch(&b, args()).await.unwrap_err().code, -32403);
}

#[tokio::test]
async fn dispatch_pending_snapshot_respects_release_budget_and_lifecycle() {
    for (policy_name, budget, lifecycle) in [
        ("declare-and-wait", 3, "planning"),
        ("auto-declare", 0, "planning"),
        ("auto-declare", 3, "blocked"),
        ("auto-declare", 3, "done"),
    ] {
        let b = boot().await;
        policy(&b, policy_name, budget).await;
        sqlx::query("UPDATE tracks SET lifecycle=?1 WHERE id=?2")
            .bind(lifecycle)
            .bind(b.track_id.as_str())
            .execute(&b.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        let out = dispatch(&b, args()).await.unwrap();
        assert_eq!(out["current"]["declaration_present"], true);
        let diagnostics = out["current"]["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]["pendingReason"].is_object()
                || out["current"]["blocking_reason"].is_string(),
            "{out}"
        );
        assert_ne!(out["current"]["task"]["status"], "running");
        let stored = b
            .repo
            .track_get(b.track_id.as_str())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(serde_json::to_value(stored.lifecycle).unwrap(), lifecycle);
        let block = payload(&b)
            .await
            .blocks
            .unwrap()
            .into_iter()
            .find(|x| x.kind == "task")
            .unwrap();
        assert_ne!(block.payload["released_by_user"], true);
    }
}

#[tokio::test]
async fn dispatch_replay_after_declaration_edit_or_removal_preserves_original_identity() {
    let b = boot().await;
    policy(&b, "declare-and-wait", 0).await;
    let original = dispatch(&b, args()).await.unwrap();
    let p = payload(&b).await;
    let block = p
        .blocks
        .unwrap()
        .into_iter()
        .find(|x| x.kind == "task")
        .unwrap();
    let mut changed = block.payload.clone();
    changed["goal"] = json!("Edited through the report");
    call_tool(
        &b,
        "calm.report.blocks.upsert",
        planner_identity(&b),
        json!({"id":block.id,"kind":"task","payload":changed,"if_rev":block.rev}),
    )
    .await
    .unwrap();
    let saved = counts(&b).await;
    let edited = payload(&b).await;
    assert_eq!(
        dispatch(&b, args()).await.unwrap()["receipt"],
        original["receipt"]
    );
    assert_eq!(counts(&b).await, saved);
    assert_eq!(payload(&b).await, edited);
    assert_eq!(
        dispatch(&b, args()).await.unwrap()["current"]["contract_status"],
        "differs_from_dispatch"
    );
    let block = edited
        .blocks
        .unwrap()
        .into_iter()
        .find(|x| x.kind == "task")
        .unwrap();
    call_tool(
        &b,
        "calm.report.blocks.delete",
        planner_identity(&b),
        json!({"id":block.id,"if_rev":block.rev}),
    )
    .await
    .unwrap();
    let saved = counts(&b).await;
    let replay = dispatch(&b, args()).await.unwrap();
    assert_eq!(replay["receipt"], original["receipt"]);
    assert_eq!(replay["current"]["declaration_unavailable"], true);
    assert_eq!(replay["current"]["contract_status"], "unavailable");
    assert_eq!(counts(&b).await, saved);
}

#[tokio::test]
async fn dispatch_receipt_failure_rolls_back_report_projection_and_events() {
    let b = boot().await;
    sqlx::query("CREATE TRIGGER deny_dispatch_receipt BEFORE INSERT ON planner_dispatch_receipts BEGIN SELECT RAISE(ABORT, 'injected receipt failure'); END")
        .execute(&b.repo.sqlite_pool().unwrap()).await.unwrap();
    let before = payload(&b).await;
    let initial = counts(&b).await;
    assert!(dispatch(&b, args()).await.is_err());
    assert_eq!(payload(&b).await, before);
    assert_eq!(counts(&b).await, initial);
    sqlx::query("DROP TRIGGER deny_dispatch_receipt")
        .execute(&b.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert!(dispatch(&b, args()).await.is_ok());
}

#[tokio::test]
async fn dispatch_replay_after_recovery_keeps_creation_identity_and_reports_current_allocation() {
    let b = boot().await;
    policy(&b, "auto-declare", 3).await;
    let original = dispatch(&b, args()).await.unwrap();
    let key = original["receipt"]["task_key"].as_str().unwrap();
    let task = crate::task_recovery::current(&b, key).await;
    crate::task_recovery::finish(&b, &task, false).await;
    let recovery = call_tool(
        &b,
        "calm.plan.recover",
        planner_identity(&b),
        crate::task_recovery::recovery_args(&task, "dispatch-repair"),
    )
    .await
    .unwrap();
    let saved = counts(&b).await;
    let replay = dispatch(&b, args()).await.unwrap();
    assert_eq!(replay["receipt"], original["receipt"]);
    assert_eq!(
        replay["current"]["allocation"]["attempt_id"],
        recovery["attempt_id"]
    );
    assert_ne!(
        replay["current"]["allocation"]["attempt_id"],
        original["current"]["allocation"]["attempt_id"]
    );
    assert_eq!(counts(&b).await, saved);
    b.repo.track_delete(b.track_id.as_str()).await.unwrap();
    assert_eq!(
        counts(&b).await.0,
        0,
        "Track deletion releases Dispatch names"
    );
}

#[tokio::test]
async fn dispatch_receipt_developer_reset_follows_track_lifetime() {
    use calm_server::db::prelude::*;
    use calm_server::db::sqlite::SqlxRepo;
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = repo
        .area_create(calm_server::model::NewArea {
            name: "reset".into(),
            color: "red".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(calm_server::model::NewTrack {
            area_id: area.id,
            title: "reset".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            template_input: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    // Historical provenance intentionally has no FK to a live card or session.
    sqlx::query("INSERT INTO planner_dispatch_receipts VALUES(?1,'Historical name',?2,'historical-key','old-report','old-block',1)")
        .bind(track.id.as_str()).bind(args().to_string()).execute(repo.pool()).await.unwrap();
    let fixture: calm_server::replay::Fixture =
        serde_json::from_value(json!({"name":"empty reset", "events":[]})).unwrap();
    calm_server::replay::reset_from_fixture(&repo, &calm_server::event::EventBus::new(), &fixture)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM planner_dispatch_receipts")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn dispatch_promotes_only_new_draft_declarations_and_replay_has_no_lifecycle_effect() {
    let b = boot().await;
    sqlx::query("UPDATE tracks SET lifecycle='draft' WHERE id=?1")
        .bind(b.track_id.as_str())
        .execute(&b.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let original = dispatch(&b, args()).await.unwrap();
    assert_eq!(original["current"]["track"]["lifecycle"], "planning");
    sqlx::query("UPDATE tracks SET lifecycle='draft' WHERE id=?1")
        .bind(b.track_id.as_str())
        .execute(&b.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let saved = counts(&b).await;
    let replay = dispatch(&b, args()).await.unwrap();
    assert_eq!(replay["receipt"], original["receipt"]);
    assert_eq!(replay["current"]["track"]["lifecycle"], "draft");
    assert_eq!(counts(&b).await, saved);
}

#[tokio::test]
async fn dispatch_response_states_the_fixed_executor_environment_up_front() {
    let b = boot().await;
    policy(&b, "auto-declare", 3).await;
    let first = dispatch(&b, args()).await.unwrap();
    let environment = &first["current"]["executor_environment"];
    assert_eq!(environment["executor"], "codex");
    assert_eq!(environment["network"]["enabled"], false);
    assert_eq!(environment["network"]["web_search"], false);
    assert_eq!(
        environment["mcp_tools"],
        json!([
            "calm.task.complete",
            "calm.task.fail",
            "calm.report.read",
            "calm.plan.list"
        ])
    );
    assert_eq!(
        environment["workspace"]["writable"],
        json!(["/workspace", "/tmp"])
    );
    assert_eq!(environment["recovery"]["environment"], "identical");
    // Replays restate the same envelope; it is not attempt-specific.
    let replay = dispatch(&b, args()).await.unwrap();
    assert_eq!(replay["current"]["executor_environment"], *environment);
}

#[tokio::test]
async fn recover_response_restates_identical_environment_with_new_workspace() {
    let b = boot().await;
    policy(&b, "auto-declare", 3).await;
    let original = dispatch(&b, args()).await.unwrap();
    let key = original["receipt"]["task_key"].as_str().unwrap();
    let task = crate::task_recovery::current(&b, key).await;
    crate::task_recovery::finish(&b, &task, false).await;
    let recovery = call_tool(
        &b,
        "calm.plan.recover",
        planner_identity(&b),
        crate::task_recovery::recovery_args(&task, "dispatch-environment"),
    )
    .await
    .unwrap();
    assert!(recovery["attempt_id"].is_string());
    assert_eq!(
        recovery["executor_environment"],
        original["current"]["executor_environment"]
    );
    let changes = recovery["recover_changes"].as_str().unwrap();
    assert!(changes.contains("identical execution environment"));
    assert!(changes.contains("only the workspace is new"));
    assert!(changes.contains("missing capability"));
}
