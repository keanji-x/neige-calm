use super::*;
use crate::ids::WaveId;
use serde::Serialize;
use serde_json::{Value, json};

fn pretty_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).expect("value serializes")
}

fn assert_same_json_bytes<T: Serialize>(new_value: &T, old_value: &Value) {
    assert_eq!(pretty_json(new_value), pretty_json(old_value));
}

fn test_card(id: &str, kind: &str, payload: Value) -> Card {
    Card {
        id: CardId::from(id),
        wave_id: WaveId::from("wave-test"),
        title: None,
        kind: kind.to_string(),
        sort: 1.25,
        payload,
        runtime: None,
        deletable: true,
        created_at: 1000,
        updated_at: 2000,
    }
}

fn run_event(event_id: i64, at: i64, kind: &'static str, payload: Value) -> RunEventProjection {
    RunEventProjection {
        event_id,
        at,
        kind,
        payload,
    }
}

fn old_card_meta_value(card: &Card, role: Value) -> Value {
    json!({
        "id": card.id,
        "kind": card.kind,
        "role": role,
        "sort": card.sort,
        "deletable": card.deletable,
        "created_at": card.created_at,
        "updated_at": card.updated_at,
    })
}

fn old_run_index_entry(run: &RunProjection) -> Value {
    json!({
        "idempotency_key": run.idempotency_key,
        "status": run.status.as_str(),
        "kind": run.kind,
        "verdict": old_run_verdict_index_json(run),
        "requested_at": run.requested_at,
        "finished_at": run.finished_at,
        "worker_card_id": run.worker_card.as_ref().map(|card| card.id.as_str()),
    })
}

fn old_run_json(run: &RunProjection) -> Value {
    json!({
        "idempotency_key": run.idempotency_key,
        "status": run.status.as_str(),
        "kind": run.kind,
        "verdict": old_run_verdict_full_json(run),
        "requested_at": run.requested_at,
        "finished_at": run.finished_at,
        "worker_card_id": run.worker_card.as_ref().map(|card| card.id.as_str()),
        "worker_card_payload": run.worker_card.as_ref().map(|card| card.payload.clone()),
        "events": {
            "requested": run.requested_event.as_ref().map(old_event_json),
            "completed": run.completed_event.as_ref().map(old_event_json),
            "failed": run.failed_event.as_ref().map(old_event_json),
            "verdict": run.verdict_event.as_ref().map(old_event_json),
        },
    })
}

fn old_run_verdict_index_json(run: &RunProjection) -> Value {
    run.verdict
        .as_ref()
        .map(|verdict| {
            json!({
                "status": verdict.status,
                "at": verdict.at,
            })
        })
        .unwrap_or(Value::Null)
}

fn old_run_verdict_full_json(run: &RunProjection) -> Value {
    run.verdict
        .as_ref()
        .map(|verdict| {
            json!({
                "status": verdict.status,
                "reason": verdict.reason,
                "at": verdict.at,
            })
        })
        .unwrap_or(Value::Null)
}

fn old_event_json(event: &RunEventProjection) -> Value {
    json!({
        "event_id": event.event_id,
        "kind": event.kind,
        "created_at": event.at,
        "payload": event.payload,
    })
}

fn old_hook_events_json(events: &[HookEventProjection]) -> Vec<Value> {
    events
        .iter()
        .map(|event| {
            json!({
                "event_id": event.event_id,
                "kind": event.kind,
                "hook_kind": event.hook_kind,
                "created_at": event.at,
                "payload": event.payload,
            })
        })
        .collect()
}

fn run_with_verdict_and_events() -> RunProjection {
    let worker_card = test_card(
        "card-worker",
        "codex",
        json!({
            "idempotency_key": "run-full",
            "prompt": "do the thing",
            "context": {"priority": "high"},
        }),
    );
    let requested_event = run_event(
        10,
        1100,
        "codex.worker_requested",
        json!({"idempotency_key": "run-full", "goal": "ship it"}),
    );
    let completed_event = run_event(
        11,
        1200,
        "task.completed",
        json!({"idempotency_key": "run-full", "result": {"summary": "done"}}),
    );
    let verdict_event = run_event(
        12,
        1300,
        "task.completed",
        json!({"idempotency_key": "run-full", "result": {"status": "accepted"}}),
    );
    RunProjection {
        idempotency_key: "run-full".into(),
        status: WaveFsRunStatus::Completed,
        kind: "codex".into(),
        requested_at: Some(requested_event.at),
        finished_at: Some(completed_event.at),
        worker_card: Some(worker_card),
        requested_event: Some(requested_event),
        completed_event: Some(completed_event),
        failed_event: None,
        verdict: Some(RunVerdictProjection {
            status: "accepted".into(),
            reason: None,
            at: verdict_event.at,
        }),
        verdict_event: Some(verdict_event),
    }
}

fn run_without_verdict_or_events() -> RunProjection {
    RunProjection {
        idempotency_key: "run-empty".into(),
        status: WaveFsRunStatus::Unknown,
        kind: "unknown".into(),
        requested_at: None,
        finished_at: None,
        worker_card: None,
        requested_event: None,
        completed_event: None,
        failed_event: None,
        verdict: None,
        verdict_event: None,
    }
}

#[test]
fn card_meta_dto_serializes_like_old_json_builder() {
    let card = test_card("card-meta", "terminal", json!({"terminal_id": "term-1"}));

    assert_same_json_bytes(
        &card_meta_value(&card, CardRole::Worker),
        &old_card_meta_value(&card, json!("worker")),
    );
    assert_same_json_bytes(
        &card_meta_value(&card, CardRole::default()),
        &old_card_meta_value(&card, json!("worker")),
    );
    assert_same_json_bytes(
        &card_meta_value(&card, CardRole::Spec),
        &old_card_meta_value(&card, json!("spec")),
    );
    assert_same_json_bytes(
        &card_meta_value(&card, CardRole::ReportCard),
        &old_card_meta_value(&card, json!("reportcard")),
    );
}

#[test]
fn run_index_entry_dto_serializes_like_old_json_builder() {
    let full = run_with_verdict_and_events();
    assert_same_json_bytes(&run_index_entry(&full), &old_run_index_entry(&full));

    let full_value = serde_json::to_value(run_index_entry(&full)).expect("dto serializes");
    assert!(
        full_value["verdict"].get("reason").is_none(),
        "index verdict must not include reason: {full_value:?}"
    );

    let empty = run_without_verdict_or_events();
    assert_same_json_bytes(&run_index_entry(&empty), &old_run_index_entry(&empty));
    let empty_value = serde_json::to_value(run_index_entry(&empty)).expect("dto serializes");
    assert!(empty_value["verdict"].is_null());
    assert!(empty_value["worker_card_id"].is_null());
    assert!(empty_value["requested_at"].is_null());
    assert!(empty_value["finished_at"].is_null());
}

#[test]
fn run_detail_dto_serializes_like_old_json_builder() {
    let full = run_with_verdict_and_events();
    assert_same_json_bytes(&run_json(&full), &old_run_json(&full));
    let full_value = serde_json::to_value(run_json(&full)).expect("dto serializes");
    assert!(full_value["events"]["requested"].is_object());
    assert!(full_value["events"]["completed"].is_object());
    assert!(full_value["events"]["failed"].is_null());
    assert!(full_value["events"]["verdict"].is_object());
    assert!(
        full_value["verdict"]["reason"].is_null(),
        "detail verdict must emit explicit null reason: {full_value:?}"
    );

    let empty = run_without_verdict_or_events();
    assert_same_json_bytes(&run_json(&empty), &old_run_json(&empty));
    let empty_value = serde_json::to_value(run_json(&empty)).expect("dto serializes");
    assert!(empty_value["verdict"].is_null());
    assert!(empty_value["worker_card_id"].is_null());
    assert!(empty_value["worker_card_payload"].is_null());
    assert!(empty_value["events"]["requested"].is_null());
    assert!(empty_value["events"]["completed"].is_null());
    assert!(empty_value["events"]["failed"].is_null());
    assert!(empty_value["events"]["verdict"].is_null());
}

// ------------------------------------------------------------------
// Issue #644 PR-B — §5.6 requested-record fallback: a key with a
// `task.dispatched` claim record but no `*.worker_requested` event
// projects from the dispatch record (requested_at, kind, the
// requested/running/terminal statuses).
// ------------------------------------------------------------------

fn fallback_write() -> WriteContext {
    WriteContext::new(
        crate::card_role_cache::CardRoleCache::new(),
        crate::wave_cove_cache::WaveCoveCache::new(),
    )
}

fn wave_scoped(id: i64, at: i64, actor: ActorId, event: Event) -> WaveEvent {
    WaveEvent {
        id,
        at,
        actor,
        scope: EventScope::Wave {
            wave: WaveId::from("wave-test"),
            cove: crate::ids::CoveId::from("cove-test"),
        },
        event,
    }
}

fn dispatched_event(id: i64, at: i64, key: &str, kind: &str) -> WaveEvent {
    wave_scoped(
        id,
        at,
        ActorId::KernelDispatcher,
        Event::TaskDispatched {
            idempotency_key: key.into(),
            kind: kind.into(),
            agent_message: None,
        },
    )
}

#[test]
fn project_runs_uses_task_dispatched_as_requested_record_fallback() {
    let write = fallback_write();
    let runs = project_runs(
        &write,
        vec![],
        vec![dispatched_event(5, 500, "w:k", "codex")],
    );
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run.idempotency_key, "w:k");
    assert_eq!(
        run.status,
        WaveFsRunStatus::Requested,
        "dispatch record alone (no worker card visible) → requested"
    );
    assert_eq!(
        run.requested_at,
        Some(500),
        "requested_at from the claim record"
    );
    assert_eq!(run.kind, "codex", "kind from the claim record");
    assert_eq!(
        run.requested_event.as_ref().map(|e| e.kind),
        Some("task.dispatched"),
        "the dispatch record IS the requested-record"
    );
}

#[test]
fn project_runs_dispatched_then_completed_resolves_terminal_status() {
    let write = fallback_write();
    let completed = wave_scoped(
        6,
        600,
        // Kernel-emitted completion (terminal-exit path) — actor
        // KernelDispatcher means NOT a spec verdict.
        ActorId::KernelDispatcher,
        Event::TaskCompleted {
            idempotency_key: "w:k".into(),
            result: json!({"exit_code": 0}),
            artifacts: vec![],
            agent_message: None,
        },
    );
    let runs = project_runs(
        &write,
        vec![],
        vec![dispatched_event(5, 500, "w:k", "terminal"), completed],
    );
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run.status, WaveFsRunStatus::Completed);
    assert_eq!(run.requested_at, Some(500));
    assert_eq!(run.finished_at, Some(600));
    assert_eq!(run.kind, "terminal");
    assert!(
        run.verdict.is_none(),
        "KernelDispatcher completion must never classify as a spec verdict"
    );
}

#[test]
fn project_runs_real_requested_event_wins_over_dispatch_record() {
    // Legacy `calm.task.dispatch` keys keep their `*.worker_requested`
    // record even if a dispatch record ever coexisted; the fallback
    // is fallback-only.
    let write = fallback_write();
    let requested = wave_scoped(
        2,
        200,
        ActorId::User,
        Event::TerminalWorkerRequested {
            idempotency_key: "w:k".into(),
            cmd: "ls".into(),
            cwd: None,
            agent_message: None,
        },
    );
    let runs = project_runs(
        &write,
        vec![],
        vec![requested, dispatched_event(5, 500, "w:k", "codex")],
    );
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(
        run.requested_event.as_ref().map(|e| e.kind),
        Some("terminal.worker_requested"),
        "real requested event wins"
    );
    assert_eq!(run.requested_at, Some(200));
    assert_eq!(
        run.kind, "terminal",
        "requested kind wins over dispatch-record kind"
    );
}

#[test]
fn run_kind_static_vocabulary() {
    assert_eq!(run_kind_static("codex"), "codex");
    assert_eq!(run_kind_static("terminal"), "terminal");
    assert_eq!(run_kind_static("claude"), "claude");
}

#[test]
fn hook_events_dto_serializes_like_old_json_builder() {
    let events = vec![
        HookEventProjection {
            event_id: 21,
            at: 2100,
            kind: "codex.hook",
            hook_kind: "hook.codex.user_prompt_submit".into(),
            payload: json!({"prompt": "hello"}),
        },
        HookEventProjection {
            event_id: 22,
            at: 2200,
            kind: "claude.hook",
            hook_kind: "Stop".into(),
            payload: json!({"last_assistant_message": "done"}),
        },
    ];

    assert_same_json_bytes(
        &hook_events_json(&events),
        &json!(old_hook_events_json(&events)),
    );
}

// ---- #695 PR3: worker-flow markdown projection -------------------------

fn flow_env(seq: u64, turn: u32) -> calm_types::worker_flow::FlowEnvelope {
    use calm_types::worker::{WorkerProviderKind, WorkerSessionId};
    calm_types::worker_flow::FlowEnvelope {
        seq,
        turn,
        session_id: WorkerSessionId::from("sess-1"),
        provider: WorkerProviderKind::Codex,
        timestamp: None,
        source_uuid: None,
        provider_extra: None,
        raw_ref: None,
    }
}

#[test]
fn worker_flow_markdown_renders_meaningful_transcript() {
    use calm_types::worker_flow::{
        ExecSource, ExecStatus, FileChangeKind, FileEdit, MessageBlock, PatchStatus, ToolCallId,
        WorkerFlowItem,
    };

    let items = vec![
        WorkerFlowItem::UserMessage {
            env: flow_env(0, 1),
            content: vec![MessageBlock::Text {
                text: "Fix the build".into(),
            }],
        },
        WorkerFlowItem::CommandExecution {
            env: flow_env(1, 1),
            call_id: Some(ToolCallId::from("c1")),
            command: "cargo build".into(),
            cwd: None,
            parsed_actions: vec![],
            aggregated_output: None,
            exit_code: Some(0),
            duration_ms: None,
            status: ExecStatus::Completed,
            source: ExecSource::Agent,
        },
        WorkerFlowItem::CommandExecution {
            env: flow_env(2, 1),
            call_id: Some(ToolCallId::from("c2")),
            command: "cargo test".into(),
            cwd: None,
            parsed_actions: vec![],
            aggregated_output: Some("assertion failed: foo".into()),
            exit_code: Some(101),
            duration_ms: None,
            status: ExecStatus::Failed,
            source: ExecSource::Agent,
        },
        WorkerFlowItem::FileChange {
            env: flow_env(3, 1),
            call_id: None,
            changes: vec![FileEdit {
                path: "src/lib.rs".into(),
                kind: FileChangeKind::Update { move_path: None },
                diff: None,
            }],
            status: PatchStatus::Completed,
        },
        WorkerFlowItem::AgentMessage {
            env: flow_env(4, 1),
            text: "All green now.".into(),
            is_final: true,
            phase: None,
        },
        WorkerFlowItem::Unknown {
            env: flow_env(5, 1),
            raw_type: "future.provider.thing".into(),
        },
    ];

    let md = worker_flow_markdown(&CardId::from("card-9"), &items);

    assert!(md.starts_with(
        "> READ-ONLY PROJECTION: derived from persisted worker flow items. This is not the source of truth."
    ));
    assert!(md.contains("# Conversation — card card-9"));
    assert!(md.contains("## User\n\nFix the build"));
    assert!(md.contains("- ran `cargo build` ✓"), "md = {md}");
    assert!(
        md.contains("- ran `cargo test` ✗ exit 101 — assertion failed: foo"),
        "md = {md}"
    );
    assert!(md.contains("- edit src/lib.rs"), "md = {md}");
    assert!(md.contains("## Assistant\n\nAll green now."), "md = {md}");
    assert!(md.contains("- (future.provider.thing)"), "md = {md}");
}

#[test]
fn worker_flow_markdown_coalesces_file_changes_by_call_id() {
    use calm_types::worker_flow::{
        FileChangeKind, FileEdit, PatchStatus, ToolCallId, WorkerFlowItem,
    };

    let edit_a = FileEdit {
        path: "a.rs".into(),
        kind: FileChangeKind::Update { move_path: None },
        diff: None,
    };
    let edit_b = FileEdit {
        path: "b.rs".into(),
        kind: FileChangeKind::Update { move_path: None },
        diff: None,
    };
    let add_free = FileEdit {
        path: "free.rs".into(),
        kind: FileChangeKind::Add,
        diff: None,
    };
    let items = vec![
        WorkerFlowItem::FileChange {
            env: flow_env(0, 1),
            call_id: Some(ToolCallId::from("c1")),
            changes: vec![edit_a.clone()],
            status: PatchStatus::InProgress,
        },
        WorkerFlowItem::FileChange {
            env: flow_env(1, 1),
            call_id: Some(ToolCallId::from("c1")),
            changes: vec![edit_a],
            status: PatchStatus::Completed,
        },
        WorkerFlowItem::FileChange {
            env: flow_env(2, 1),
            call_id: Some(ToolCallId::from("c2")),
            changes: vec![edit_b],
            status: PatchStatus::InProgress,
        },
        WorkerFlowItem::FileChange {
            env: flow_env(3, 1),
            call_id: None,
            changes: vec![add_free],
            status: PatchStatus::Completed,
        },
    ];

    let md = worker_flow_markdown(&CardId::from("card-file-coalesce"), &items);

    assert_eq!(md.matches("- edit a.rs\n").count(), 1, "md = {md}");
    assert_eq!(md.matches("- edit b.rs\n").count(), 1, "md = {md}");
    assert_eq!(md.matches("- add free.rs\n").count(), 1, "md = {md}");
}

#[test]
fn worker_flow_markdown_coalesces_web_searches_by_call_id() {
    use calm_types::worker_flow::{ToolCallId, WorkerFlowItem};

    let items = vec![
        WorkerFlowItem::WebSearch {
            env: flow_env(0, 1),
            call_id: Some(ToolCallId::from("w1")),
            query: Some("rust serde".into()),
            results_summary: None,
        },
        WorkerFlowItem::WebSearch {
            env: flow_env(1, 1),
            call_id: Some(ToolCallId::from("w1")),
            query: Some("rust serde".into()),
            results_summary: Some("3 results".into()),
        },
    ];

    let md = worker_flow_markdown(&CardId::from("card-web-coalesce"), &items);

    assert_eq!(
        md.matches("- searched: rust serde\n").count(),
        1,
        "md = {md}"
    );
}

#[test]
fn worker_flow_markdown_coalesces_mcp_tool_calls_by_call_id() {
    use calm_types::worker_flow::{McpStatus, ToolCallId, WorkerFlowItem};

    let items = vec![
        WorkerFlowItem::McpToolCall {
            env: flow_env(0, 1),
            call_id: ToolCallId::from("m1"),
            server: Some("fs".into()),
            tool: "read".into(),
            arguments: json!({"path": "a.txt"}),
            status: McpStatus::InProgress,
            result: None,
            error: None,
            duration_ms: None,
        },
        WorkerFlowItem::McpToolCall {
            env: flow_env(1, 1),
            call_id: ToolCallId::from("m1"),
            server: Some("fs".into()),
            tool: "read".into(),
            arguments: json!({"path": "a.txt"}),
            status: McpStatus::Completed,
            result: Some(json!({"content": "ok"})),
            error: None,
            duration_ms: Some(12),
        },
    ];

    let md = worker_flow_markdown(&CardId::from("card-mcp-coalesce"), &items);

    assert_eq!(md.matches("- fs.read").count(), 1, "md = {md}");
    assert!(md.contains("- fs.read ✓"), "md = {md}");

    let distinct = vec![
        WorkerFlowItem::McpToolCall {
            env: flow_env(0, 1),
            call_id: ToolCallId::from("m2"),
            server: Some("fs".into()),
            tool: "first".into(),
            arguments: json!({}),
            status: McpStatus::Completed,
            result: Some(json!({"ok": true})),
            error: None,
            duration_ms: None,
        },
        WorkerFlowItem::McpToolCall {
            env: flow_env(1, 1),
            call_id: ToolCallId::from("m3"),
            server: Some("fs".into()),
            tool: "second".into(),
            arguments: json!({}),
            status: McpStatus::Completed,
            result: Some(json!({"ok": true})),
            error: None,
            duration_ms: None,
        },
    ];

    let md = worker_flow_markdown(&CardId::from("card-mcp-distinct"), &distinct);

    assert_eq!(md.matches("- fs.first ✓\n").count(), 1, "md = {md}");
    assert_eq!(md.matches("- fs.second ✓\n").count(), 1, "md = {md}");
}

#[test]
fn worker_flow_markdown_renders_command_execution_statuses() {
    use calm_types::worker_flow::{ExecSource, ExecStatus, WorkerFlowItem};

    fn command_item(
        seq: u64,
        command: &str,
        status: ExecStatus,
        exit_code: Option<i32>,
        aggregated_output: Option<&str>,
    ) -> WorkerFlowItem {
        WorkerFlowItem::CommandExecution {
            env: flow_env(seq, 1),
            call_id: None,
            command: command.into(),
            cwd: None,
            parsed_actions: vec![],
            aggregated_output: aggregated_output.map(str::to_string),
            exit_code,
            duration_ms: None,
            status,
            source: ExecSource::Agent,
        }
    }

    let items = vec![
        command_item(0, "cargo check", ExecStatus::InProgress, None, None),
        command_item(1, "cargo fmt", ExecStatus::Completed, Some(0), None),
        command_item(2, "cargo test", ExecStatus::Failed, Some(1), Some("boom")),
        command_item(3, "dangerous command", ExecStatus::Declined, None, None),
    ];

    let md = worker_flow_markdown(&CardId::from("card-statuses"), &items);

    assert!(md.contains("- ran `cargo check` ⋯ running"), "md = {md}");
    assert!(!md.contains("- ran `cargo check` ✓"), "md = {md}");
    assert!(md.contains("- ran `cargo fmt` ✓"), "md = {md}");
    assert!(
        md.contains("- ran `cargo test` ✗ exit 1 — boom"),
        "md = {md}"
    );
    assert!(
        md.contains("- ran `dangerous command` ⊘ declined"),
        "md = {md}"
    );
}

#[test]
fn worker_flow_markdown_empty_reports_no_items() {
    let md = worker_flow_markdown(&CardId::from("card-0"), &[]);
    assert!(md.contains("_No worker-flow items recorded._"), "md = {md}");
}

/// Regression for #695 PR3: a worker session with >500 flow items must
/// render the WHOLE transcript. The db layer clamps `limit` to 500, so a
/// single `worker_flow_item_list_by_card(.., 0, 1000, false)` returns only
/// the OLDEST 500 rows (ascending) and DROPS the tail — including the final
/// `AgentMessage{is_final:true}` answer. `worker_flow_rows_all` (the same
/// paging path the `conversation.md` cat branch uses) must page through all
/// of them. Without the fix this test fails: the final answer (highest id)
/// lands past row 500 and never reaches the rendered markdown.
#[tokio::test]
async fn conversation_md_paging_renders_full_transcript_over_500_items() {
    use crate::db::sqlite::{
        SqlxRepo, card_create_with_id_tx, cove_create_tx, session_insert_tx, wave_create_tx,
        worker_flow_item_insert_tx,
    };
    use crate::model::{NewCard, NewCove, NewWave, RequestTheme};
    use calm_types::worker::{
        LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession,
        WorkerSessionId, WorkerSessionState,
    };
    use calm_types::worker_flow::{MessageBlock, WorkerFlowItem};

    const FIRST_USER: &str = "FIRST-USER-MESSAGE-MARKER";
    const FINAL_ANSWER: &str = "FINAL-ANSWER-MARKER-TAIL-NOT-DROPPED";

    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();

    // Seed a real cove → wave → card chain (FK target) and bulk-insert
    // 600 flow items in ONE transaction for speed: a UserMessage first,
    // 598 CommandExecution rows, then the final AgentMessage LAST (highest
    // id). `flow_env(seq, turn)` keeps every item in turn 1.
    let mut tx = repo.pool().begin().await.unwrap();
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            workflow_input: None,
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        "card-big".into(),
        NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "worker".into(),
            sort: None,
            payload: serde_json::json!({}),
        },
        CardRole::Worker,
        true,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    let card_id = card.id.to_string();
    let session_id = "rt-card-big";
    session_insert_tx(
        &mut tx,
        WorkerSession {
            id: WorkerSessionId::from(session_id),
            wave_id: wave.id.clone(),
            provider: WorkerProviderKind::Codex,
            mode: SessionMode::Resumable,
            contract: WorkerContract::Executor,
            parent_session_id: None,
            requester_session_id: None,
            state: WorkerSessionState::Running,
            mcp_token_hash: None,
            thread_id: Some("thread-card-big".into()),
            agent_session_id: Some("agent-card-big".into()),
            active_turn_id: None,
            terminal_run_id: None,
            card_id: Some(calm_types::ids::CardId(card_id.clone())),
            handle_state_json: None,
            liveness: LivenessTag::Alive,
            liveness_probed_at_ms: None,
            exit_code: None,
            exit_interpretation: None,
            spawn_op_id: None,
            last_activity_ms: None,
            last_thread_status: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: None,
        },
    )
    .await
    .unwrap();

    // First user message (lowest id).
    let first = WorkerFlowItem::UserMessage {
        env: flow_env(0, 1),
        content: vec![MessageBlock::Text {
            text: FIRST_USER.into(),
        }],
    };
    worker_flow_item_insert_tx(
        &mut tx,
        Some(&card_id),
        Some(session_id),
        Some(wave.id.as_str()),
        Some(session_id),
        "user_message",
        &serde_json::to_string(&first).unwrap(),
        1,
    )
    .await
    .unwrap();

    // 598 command executions in the middle.
    for n in 0..598u64 {
        let item = WorkerFlowItem::CommandExecution {
            env: flow_env(n + 1, 1),
            call_id: None,
            command: format!("echo {n}"),
            cwd: None,
            parsed_actions: vec![],
            aggregated_output: None,
            exit_code: Some(0),
            duration_ms: None,
            status: calm_types::worker_flow::ExecStatus::Completed,
            source: calm_types::worker_flow::ExecSource::Agent,
        };
        worker_flow_item_insert_tx(
            &mut tx,
            Some(&card_id),
            Some(session_id),
            Some(wave.id.as_str()),
            Some(session_id),
            "command_execution",
            &serde_json::to_string(&item).unwrap(),
            (2 + n) as i64,
        )
        .await
        .unwrap();
    }

    // Final answer LAST (highest id) — well past row 500.
    let final_item = WorkerFlowItem::AgentMessage {
        env: flow_env(599, 1),
        text: FINAL_ANSWER.into(),
        is_final: true,
        phase: None,
    };
    worker_flow_item_insert_tx(
        &mut tx,
        Some(&card_id),
        Some(session_id),
        Some(wave.id.as_str()),
        Some(session_id),
        "assistant_message",
        &serde_json::to_string(&final_item).unwrap(),
        600,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // Render through the SAME paging path the cat branch uses.
    let rows = worker_flow_rows_all(&repo, &card_id).await.unwrap();
    assert_eq!(rows.len(), 600, "all 600 rows must be paged in");
    let items: Vec<WorkerFlowItem> = rows
        .iter()
        .map(|row| deserialize_flow_row(&row.kind, &row.payload))
        .collect();
    let md = worker_flow_markdown(&card.id, &items);

    assert!(
        md.contains(FIRST_USER),
        "first user message must be rendered"
    );
    assert!(
        md.contains(FINAL_ANSWER),
        "final answer (tail past row 500) must NOT be dropped"
    );
}
