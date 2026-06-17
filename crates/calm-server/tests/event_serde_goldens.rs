//! Issue #679 PR0-B — characterization goldens for the `Event` wire format.
//!
//! Pins the serde **runtime** shape of every `Event` variant: rename tags,
//! aliases (`codex.job_requested` → `CodexWorkerRequested`), `#[serde(default)]`
//! hydration, and the `skip_serializing_if` vs always-emit-null split. The
//! ts-rs byte gate covers the *type* surface; these goldens cover the parts
//! the TS export cannot see. No behavior change — if one of these tests goes
//! red, either the wire protocol broke or the golden must change with an
//! explicit rationale.
//!
//! Golden file format (`tests/goldens/events/*.json`):
//!
//! ```json
//! {
//!   "description": "...",
//!   "wire":      { "ev": "...", "data": { ... } },   // fed to Deserialize
//!   "canonical": { "ev": "...", "data": { ... } }    // expected Serialize
//! }                                                  // output; defaults to
//! ```                                                // "wire" when omitted
//!
//! Each test:
//!   1. deserializes `wire` → `Event`, asserts the typed struct matches the
//!      in-code expected value (via `Debug` repr — `Event` has no `PartialEq`);
//!   2. serializes the expected struct, asserts canonical-JSON equality with
//!      `canonical` (field order independent, content exact);
//!   3. asserts `canonical` is a serde fixed point (deserialize → serialize
//!      returns it unchanged).

use calm_server::event::{ArtifactRef, EditAuthor, Event, WaveUpdatedPayload};
use calm_server::harness::snapshot::HarnessPhaseTag;
use calm_server::ids::{CardId, CoveId, WaveId};
use calm_server::model::{Card, CardRuntimeView, Cove, CoveKind, Overlay, Wave, WaveLifecycle};
use calm_server::runtime_repo::{AgentProvider, RuntimeKind, WorkerSessionState};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Golden {
    /// Human rationale; required so every golden documents what it pins.
    #[expect(dead_code, reason = "documentation field; presence enforced by serde")]
    description: String,
    wire: Value,
    #[serde(default)]
    canonical: Option<Value>,
}

fn goldens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/events")
}

fn load(file: &str) -> Golden {
    let path = goldens_dir().join(file);
    let raw =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("read golden {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse golden {file}: {e}"))
}

fn check(file: &str, expected: Event) {
    let golden = load(file);
    let canonical = golden.canonical.unwrap_or_else(|| golden.wire.clone());

    // 1. Deserialize the wire shape → typed struct must match.
    let got: Event = serde_json::from_value(golden.wire.clone())
        .unwrap_or_else(|e| panic!("{file}: wire failed to deserialize: {e}"));
    assert_eq!(
        format!("{got:?}"),
        format!("{expected:?}"),
        "{file}: deserialized struct differs from expected"
    );

    // 2. Serialize the expected struct → must equal the canonical golden.
    let ser = serde_json::to_value(&expected)
        .unwrap_or_else(|e| panic!("{file}: expected struct failed to serialize: {e}"));
    assert_eq!(
        ser, canonical,
        "{file}: serialization differs from canonical golden"
    );

    // 3. Canonical form is a serde fixed point.
    let round: Event = serde_json::from_value(canonical.clone())
        .unwrap_or_else(|e| panic!("{file}: canonical failed to deserialize: {e}"));
    assert_eq!(
        serde_json::to_value(&round).unwrap(),
        canonical,
        "{file}: canonical golden is not a serde fixed point"
    );
}

macro_rules! golden_test {
    ($name:ident, $file:expr, $expected:expr) => {
        #[test]
        fn $name() {
            check($file, $expected);
        }
    };
}

// ---------------------------------------------------------------------------
// Shared fixture builders (deterministic fake data; must mirror the goldens)
// ---------------------------------------------------------------------------

fn wave_min() -> Wave {
    Wave {
        id: WaveId::from("wave-01"),
        cove_id: CoveId::from("cove-01"),
        title: "Golden Wave".into(),
        sort: 1.5,
        archived_at: None,
        pinned_at: None,
        lifecycle: WaveLifecycle::Draft,
        cwd: String::new(),
        terminal_at: None,
        created_at: 1000,
        updated_at: 2000,
    }
}

fn card_min() -> Card {
    Card {
        id: CardId::from("card-01"),
        wave_id: WaveId::from("wave-01"),
        kind: "terminal".into(),
        sort: 1.5,
        payload: json!({}),
        runtime: None,
        deletable: true,
        created_at: 1000,
        updated_at: 2000,
    }
}

// ---------------------------------------------------------------------------
// Per-variant goldens
// ---------------------------------------------------------------------------

golden_test!(
    cove_updated_full,
    "cove_updated.full.json",
    Event::CoveUpdated(Cove {
        id: CoveId::from("cove-01"),
        name: "Golden Cove".into(),
        color: "#aabbcc".into(),
        sort: 1.5,
        kind: CoveKind::System,
        created_at: 1000,
        updated_at: 2000,
    })
);

golden_test!(
    cove_updated_min,
    "cove_updated.min.json",
    Event::CoveUpdated(Cove {
        id: CoveId::from("cove-01"),
        name: "Golden Cove".into(),
        color: "#aabbcc".into(),
        sort: 1.5,
        kind: CoveKind::User,
        created_at: 1000,
        updated_at: 2000,
    })
);

golden_test!(
    cove_deleted,
    "cove_deleted.json",
    Event::CoveDeleted {
        id: CoveId::from("cove-01"),
    }
);

golden_test!(
    wave_updated_full,
    "wave_updated.full.json",
    Event::WaveUpdated(WaveUpdatedPayload::new(
        Wave {
            archived_at: Some(111),
            pinned_at: Some(222),
            lifecycle: WaveLifecycle::Working,
            cwd: "/tmp/golden-wave".into(),
            terminal_at: Some(333),
            ..wave_min()
        },
        Some("spec says hi".into()),
    ))
);

golden_test!(
    wave_updated_min,
    "wave_updated.min.json",
    Event::WaveUpdated(WaveUpdatedPayload::new(wave_min(), None))
);

golden_test!(
    wave_deleted,
    "wave_deleted.json",
    Event::WaveDeleted {
        id: WaveId::from("wave-01"),
        cove_id: CoveId::from("cove-01"),
    }
);

golden_test!(
    wave_lifecycle_changed_full,
    "wave_lifecycle_changed.full.json",
    Event::WaveLifecycleChanged {
        id: WaveId::from("wave-01"),
        cove_id: CoveId::from("cove-01"),
        from: WaveLifecycle::Reviewing,
        to: WaveLifecycle::Done,
        agent_message: Some("review passed".into()),
    }
);

golden_test!(
    wave_lifecycle_changed_min,
    "wave_lifecycle_changed.min.json",
    Event::WaveLifecycleChanged {
        id: WaveId::from("wave-01"),
        cove_id: CoveId::from("cove-01"),
        from: WaveLifecycle::Draft,
        to: WaveLifecycle::Planning,
        agent_message: None,
    }
);

golden_test!(
    card_added_full,
    "card_added.full.json",
    Event::CardAdded(Card {
        payload: json!({"foo": "bar"}),
        runtime: Some(CardRuntimeView {
            runtime_id: "rt-01".into(),
            kind: RuntimeKind::CodexCard,
            status: WorkerSessionState::Running,
            provider: Some(AgentProvider::Codex),
            terminal_id: Some("term-01".into()),
            thread_id: Some("thread-01".into()),
            session_id: Some("sess-01".into()),
            source: Some("spawn".into()),
            thread_status: Some("active".into()),
        }),
        deletable: false,
        ..card_min()
    })
);

golden_test!(
    card_added_min,
    "card_added.min.json",
    Event::CardAdded(card_min())
);

golden_test!(
    card_updated,
    "card_updated.json",
    Event::CardUpdated(Card {
        kind: "ui://plugin-x/view-y".into(),
        sort: 2.5,
        payload: json!({"n": 1}),
        runtime: Some(CardRuntimeView {
            runtime_id: "rt-02".into(),
            kind: RuntimeKind::Terminal,
            status: WorkerSessionState::Exited,
            provider: None,
            terminal_id: None,
            thread_id: None,
            session_id: None,
            source: None,
            thread_status: None,
        }),
        ..card_min()
    })
);

golden_test!(
    card_deleted,
    "card_deleted.json",
    Event::CardDeleted {
        id: CardId::from("card-01"),
        wave_id: WaveId::from("wave-01"),
    }
);

golden_test!(
    runtime_started_full,
    "runtime_started.full.json",
    Event::RuntimeStarted {
        runtime_id: "rt-01".into(),
        card_id: "card-01".into(),
        kind: RuntimeKind::ClaudeCard,
        agent_provider: Some(AgentProvider::Claude),
        status: WorkerSessionState::Starting,
    }
);

golden_test!(
    runtime_started_min,
    "runtime_started.min.json",
    Event::RuntimeStarted {
        runtime_id: "rt-01".into(),
        card_id: "card-01".into(),
        kind: RuntimeKind::Terminal,
        agent_provider: None,
        status: WorkerSessionState::Starting,
    }
);

golden_test!(
    runtime_status_changed,
    "runtime_status_changed.json",
    Event::RuntimeStatusChanged {
        runtime_id: "rt-01".into(),
        card_id: "card-01".into(),
        old_status: WorkerSessionState::TurnPending,
        new_status: WorkerSessionState::Running,
    }
);

golden_test!(
    runtime_superseded,
    "runtime_superseded.json",
    Event::RuntimeSuperseded {
        old_runtime_id: "rt-01".into(),
        new_runtime_id: "rt-02".into(),
        card_id: "card-01".into(),
    }
);

golden_test!(
    harness_item_added_full,
    "harness_item_added.full.json",
    Event::HarnessItemAdded {
        runtime_id: "rt-01".into(),
        card_id: CardId::from("card-01"),
        wave_id: WaveId::from("wave-01"),
        item_db_id: 42,
        item_uuid: Some("uuid-01".into()),
        item_type: Some("agentMessage".into()),
        turn_id: Some("turn-01".into()),
        method: "item.completed".into(),
    }
);

golden_test!(
    harness_item_added_min,
    "harness_item_added.min.json",
    Event::HarnessItemAdded {
        runtime_id: "rt-01".into(),
        card_id: CardId::from("card-01"),
        wave_id: WaveId::from("wave-01"),
        item_db_id: 42,
        item_uuid: None,
        item_type: None,
        turn_id: None,
        method: "item.started".into(),
    }
);

golden_test!(
    harness_phase_changed,
    "harness_phase_changed.json",
    Event::HarnessPhaseChanged {
        runtime_id: "rt-01".into(),
        card_id: CardId::from("card-01"),
        wave_id: WaveId::from("wave-01"),
        old_phase: HarnessPhaseTag::PendingThreadStart,
        new_phase: HarnessPhaseTag::TurnRunning,
    }
);

golden_test!(
    harness_transcript_cleared,
    "harness_transcript_cleared.json",
    Event::HarnessTranscriptCleared {
        runtime_id: "rt-01".into(),
        card_id: CardId::from("card-01"),
        wave_id: WaveId::from("wave-01"),
    }
);

golden_test!(
    harness_user_message_enqueued,
    "harness_user_message_enqueued.json",
    Event::HarnessUserMessageEnqueued {
        runtime_id: "rt-01".into(),
        card_id: CardId::from("card-01"),
        wave_id: WaveId::from("wave-01"),
        char_count: 280,
    }
);

golden_test!(
    wave_report_edited_full,
    "wave_report_edited.full.json",
    Event::WaveReportEdited {
        wave_id: WaveId::from("wave-01"),
        card_id: CardId::from("card-01"),
        author: EditAuthor::Spec,
        edit_id: "edit-0001".into(),
        summary_before: "old summary".into(),
        summary_after: "new summary".into(),
        body_before: "old body".into(),
        body_after: "new body".into(),
        agent_message: Some("rewrote intro".into()),
    }
);

golden_test!(
    wave_report_edited_min,
    "wave_report_edited.min.json",
    Event::WaveReportEdited {
        wave_id: WaveId::from("wave-01"),
        card_id: CardId::from("card-01"),
        author: EditAuthor::User,
        edit_id: "edit-0002".into(),
        summary_before: String::new(),
        summary_after: "s".into(),
        body_before: String::new(),
        body_after: "b".into(),
        agent_message: None,
    }
);

golden_test!(
    overlay_set,
    "overlay_set.json",
    Event::OverlaySet(Overlay {
        id: "ovl-01".into(),
        plugin_id: "plugin-x".into(),
        entity_kind: "card".into(),
        entity_id: "card-01".into(),
        kind: "status".into(),
        payload: json!({"progress": 50}),
        updated_at: 2000,
    })
);

golden_test!(
    overlay_deleted,
    "overlay_deleted.json",
    Event::OverlayDeleted {
        plugin_id: "plugin-x".into(),
        entity_kind: "card".into(),
        entity_id: "card-01".into(),
        kind: "status".into(),
    }
);

golden_test!(
    terminal_deleted,
    "terminal_deleted.json",
    Event::TerminalDeleted {
        id: "term-01".into(),
        card_id: CardId::from("card-01"),
    }
);

golden_test!(
    plugin_state_full,
    "plugin_state.full.json",
    Event::PluginState {
        id: "plugin-x".into(),
        state: "crashed".into(),
        last_error: Some("exit code 1".into()),
    }
);

golden_test!(
    plugin_state_min,
    "plugin_state.min.json",
    Event::PluginState {
        id: "plugin-x".into(),
        state: "running".into(),
        last_error: None,
    }
);

golden_test!(
    codex_hook_full,
    "codex_hook.full.json",
    Event::CodexHook {
        card_id: CardId::from("card-01"),
        kind: "hook.codex.pre_tool_use".into(),
        hook_idempotency_key: "hook-key-01".into(),
        payload: json!({"hook_event_name": "PreToolUse"}),
    }
);

golden_test!(
    codex_hook_min,
    "codex_hook.min.json",
    Event::CodexHook {
        card_id: CardId::from("card-01"),
        kind: "hook.codex.unknown".into(),
        hook_idempotency_key: String::new(),
        payload: json!({}),
    }
);

golden_test!(
    claude_hook_full,
    "claude_hook.full.json",
    Event::ClaudeHook {
        card_id: CardId::from("card-01"),
        kind: "hook.claude.stop".into(),
        hook_idempotency_key: "hook-key-02".into(),
        payload: json!({"hook_event_name": "Stop"}),
    }
);

golden_test!(
    claude_hook_min,
    "claude_hook.min.json",
    Event::ClaudeHook {
        card_id: CardId::from("card-01"),
        kind: "hook.claude.unknown".into(),
        hook_idempotency_key: String::new(),
        payload: json!({}),
    }
);

fn codex_worker_requested_min() -> Event {
    Event::CodexWorkerRequested {
        idempotency_key: "idem-01".into(),
        goal: "build the thing".into(),
        context: Value::Null,
        acceptance_criteria: None,
        agent_message: None,
    }
}

golden_test!(
    codex_worker_requested_full,
    "codex_worker_requested.full.json",
    Event::CodexWorkerRequested {
        idempotency_key: "idem-01".into(),
        goal: "build the thing".into(),
        context: json!({"cwd": "/tmp/golden"}),
        acceptance_criteria: Some("tests pass".into()),
        agent_message: Some("dispatching worker".into()),
    }
);

golden_test!(
    codex_worker_requested_min_golden,
    "codex_worker_requested.min.json",
    codex_worker_requested_min()
);

golden_test!(
    codex_worker_requested_alias,
    "codex_worker_requested.alias.json",
    codex_worker_requested_min()
);

fn terminal_worker_requested_min() -> Event {
    Event::TerminalWorkerRequested {
        idempotency_key: "idem-02".into(),
        cmd: "cargo test".into(),
        cwd: None,
        agent_message: None,
    }
}

golden_test!(
    terminal_worker_requested_full,
    "terminal_worker_requested.full.json",
    Event::TerminalWorkerRequested {
        idempotency_key: "idem-02".into(),
        cmd: "cargo test".into(),
        cwd: Some("/tmp/golden".into()),
        agent_message: Some("running gate".into()),
    }
);

golden_test!(
    terminal_worker_requested_min_golden,
    "terminal_worker_requested.min.json",
    terminal_worker_requested_min()
);

golden_test!(
    terminal_worker_requested_alias,
    "terminal_worker_requested.alias.json",
    terminal_worker_requested_min()
);

golden_test!(
    task_completed_full,
    "task_completed.full.json",
    Event::TaskCompleted {
        idempotency_key: "idem-01".into(),
        result: json!({"summary": "done"}),
        artifacts: vec![
            ArtifactRef::from("artifact://report.md"),
            ArtifactRef::from("artifact://diff.patch"),
        ],
        agent_message: Some("worker finished".into()),
    }
);

golden_test!(
    task_completed_min,
    "task_completed.min.json",
    Event::TaskCompleted {
        idempotency_key: "idem-01".into(),
        result: Value::Null,
        artifacts: vec![],
        agent_message: None,
    }
);

golden_test!(
    task_failed_full,
    "task_failed.full.json",
    Event::TaskFailed {
        idempotency_key: "idem-01".into(),
        reason: "compile error".into(),
        agent_message: Some("giving up".into()),
    }
);

golden_test!(
    task_failed_min,
    "task_failed.min.json",
    Event::TaskFailed {
        idempotency_key: "idem-01".into(),
        reason: "compile error".into(),
        agent_message: None,
    }
);

golden_test!(
    plan_updated_full,
    "plan_updated.full.json",
    Event::PlanUpdated {
        wave_id: WaveId::from("wave-01"),
        changed_keys: vec!["t1".into(), "t2".into()],
        agent_message: Some("plan revised".into()),
    }
);

golden_test!(
    plan_updated_min,
    "plan_updated.min.json",
    Event::PlanUpdated {
        wave_id: WaveId::from("wave-01"),
        changed_keys: vec![],
        agent_message: None,
    }
);

golden_test!(
    task_dispatched_full,
    "task_dispatched.full.json",
    Event::TaskDispatched {
        idempotency_key: "wave-01:build-step".into(),
        kind: "codex".into(),
        agent_message: Some("scheduler claimed build-step".into()),
    }
);

golden_test!(
    task_dispatched_min,
    "task_dispatched.min.json",
    Event::TaskDispatched {
        idempotency_key: "wave-01:build-step".into(),
        kind: "terminal".into(),
        agent_message: None,
    }
);

golden_test!(
    task_gate_result_full,
    "task_gate_result.full.json",
    Event::TaskGateResult {
        task_id: "wave-01:build-step".into(),
        idempotency_key: "wave-01:build-step".into(),
        passed: false,
        failing_step: Some("clippy".into()),
        exit_code: Some(101),
        log_tail: "error: gate step failed\n".into(),
        log_path: "/data/gate-logs/wave-01:build-step-g2.log".into(),
        attempt: 2,
        agent_message: Some("gate attempt 2 failed at clippy".into()),
    }
);

golden_test!(
    task_gate_result_min,
    "task_gate_result.min.json",
    Event::TaskGateResult {
        task_id: "wave-01:build-step".into(),
        idempotency_key: "wave-01:build-step".into(),
        passed: true,
        failing_step: None,
        exit_code: None,
        log_tail: String::new(),
        log_path: "/data/gate-logs/wave-01:build-step-g1.log".into(),
        attempt: 1,
        agent_message: None,
    }
);

// ---------------------------------------------------------------------------
// Alias coverage through the events-table replay path
// ---------------------------------------------------------------------------

/// The replay path reconstructs typed events from `(kind, payload)` rows via
/// `Event::from_kind_and_payload`. Pre-#581 rows persist the OLD kind strings
/// (`*.job_requested`); the serde aliases are what keep those rows readable.
/// Pin the alias through this exact entry point, not just raw envelope JSON.
#[test]
fn alias_kinds_survive_from_kind_and_payload() {
    let codex = Event::from_kind_and_payload(
        "codex.job_requested",
        json!({"idempotency_key": "idem-01", "goal": "build the thing", "context": null}),
    )
    .expect("codex.job_requested alias must deserialize");
    assert_eq!(codex.kind_tag(), "codex.worker_requested");
    assert!(matches!(codex, Event::CodexWorkerRequested { .. }));

    let terminal = Event::from_kind_and_payload(
        "terminal.job_requested",
        json!({"idempotency_key": "idem-02", "cmd": "cargo test"}),
    )
    .expect("terminal.job_requested alias must deserialize");
    assert_eq!(terminal.kind_tag(), "terminal.worker_requested");
    assert!(matches!(terminal, Event::TerminalWorkerRequested { .. }));
}

// ---------------------------------------------------------------------------
// Coverage guards
// ---------------------------------------------------------------------------

/// Every `Event` variant's kind tag, in declaration order. Adding a variant
/// to the enum without adding a golden (and a tag here) fails the coverage
/// test below.
const ALL_KIND_TAGS: [&str; 29] = [
    "cove.updated",
    "cove.deleted",
    "wave.updated",
    "wave.deleted",
    "wave.lifecycle_changed",
    "card.added",
    "card.updated",
    "card.deleted",
    "runtime.started",
    "runtime.status_changed",
    "runtime.superseded",
    "harness.item.added",
    "harness.phase.changed",
    "harness.transcript.cleared",
    "harness.user_message.enqueued",
    "wave.report_edited",
    "overlay.set",
    "overlay.deleted",
    "terminal.deleted",
    "plugin.state",
    "codex.hook",
    "claude.hook",
    "codex.worker_requested",
    "terminal.worker_requested",
    "task.completed",
    "task.failed",
    "plan.updated",
    "task.dispatched",
    "task.gate_result",
];

/// Every golden file must parse, every canonical `ev` tag must be a known
/// kind tag, and every kind tag must be covered by at least one golden.
/// Catches both a stray/typo'd golden file and a new Event variant landing
/// without a golden.
#[test]
fn goldens_cover_every_event_variant() {
    let dir = goldens_dir();
    let mut covered: BTreeSet<String> = BTreeSet::new();
    let mut files = 0usize;
    for entry in fs::read_dir(&dir).expect("read goldens dir") {
        let path = entry.expect("dir entry").path();
        assert_eq!(
            path.extension().and_then(|e| e.to_str()),
            Some("json"),
            "non-JSON file in goldens dir: {}",
            path.display()
        );
        files += 1;
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let golden = load(&file);
        let canonical = golden.canonical.unwrap_or(golden.wire);
        let ev = canonical["ev"]
            .as_str()
            .unwrap_or_else(|| panic!("{file}: canonical missing string `ev`"))
            .to_owned();
        assert!(
            ALL_KIND_TAGS.contains(&ev.as_str()),
            "{file}: canonical ev tag {ev:?} is not a known Event kind tag"
        );
        covered.insert(ev);
    }
    assert_eq!(
        files, 48,
        "golden file count changed — update the per-variant tests"
    );
    for tag in ALL_KIND_TAGS {
        assert!(
            covered.contains(tag),
            "Event variant {tag:?} has no golden file"
        );
    }
}

/// Compile-level companion to `goldens_cover_every_event_variant`: a new
/// `Event` variant makes this match non-exhaustive, forcing the author to
/// come here — and to the goldens — deliberately. Mirrors `Event::kind_tag`
/// so the const list above can't silently drift from the enum.
#[test]
fn kind_tag_list_matches_enum() {
    fn tag_of(ev: &Event) -> &'static str {
        // Exhaustive on purpose — no `_` arm. New variants must extend
        // ALL_KIND_TAGS and add goldens.
        match ev {
            Event::CoveUpdated(_) => "cove.updated",
            Event::CoveDeleted { .. } => "cove.deleted",
            Event::WaveUpdated(_) => "wave.updated",
            Event::WaveDeleted { .. } => "wave.deleted",
            Event::WaveLifecycleChanged { .. } => "wave.lifecycle_changed",
            Event::CardAdded(_) => "card.added",
            Event::CardUpdated(_) => "card.updated",
            Event::CardDeleted { .. } => "card.deleted",
            Event::RuntimeStarted { .. } => "runtime.started",
            Event::RuntimeStatusChanged { .. } => "runtime.status_changed",
            Event::RuntimeSuperseded { .. } => "runtime.superseded",
            Event::HarnessItemAdded { .. } => "harness.item.added",
            Event::HarnessPhaseChanged { .. } => "harness.phase.changed",
            Event::HarnessTranscriptCleared { .. } => "harness.transcript.cleared",
            Event::HarnessUserMessageEnqueued { .. } => "harness.user_message.enqueued",
            Event::WaveReportEdited { .. } => "wave.report_edited",
            Event::OverlaySet(_) => "overlay.set",
            Event::OverlayDeleted { .. } => "overlay.deleted",
            Event::TerminalDeleted { .. } => "terminal.deleted",
            Event::PluginState { .. } => "plugin.state",
            Event::CodexHook { .. } => "codex.hook",
            Event::ClaudeHook { .. } => "claude.hook",
            Event::CodexWorkerRequested { .. } => "codex.worker_requested",
            Event::TerminalWorkerRequested { .. } => "terminal.worker_requested",
            Event::TaskCompleted { .. } => "task.completed",
            Event::TaskFailed { .. } => "task.failed",
            Event::PlanUpdated { .. } => "plan.updated",
            Event::TaskDispatched { .. } => "task.dispatched",
            Event::TaskGateResult { .. } => "task.gate_result",
        }
    }
    let sample = Event::CoveDeleted {
        id: CoveId::from("cove-01"),
    };
    assert_eq!(tag_of(&sample), sample.kind_tag());
    assert_eq!(
        ALL_KIND_TAGS.len(),
        29,
        "ALL_KIND_TAGS length drifted from the Event enum"
    );
}
