use super::*;
use calm_types::worker::WorkerSessionId;

/// Env-override permits parsing — covers the four cases the helper
/// documents (unset, empty, unparseable, zero, valid).
#[test]
fn permits_from_env_fallback_paths() {
    // Save + restore so this test doesn't disturb its neighbors.
    let saved = std::env::var("NEIGE_DISPATCHER_PERMITS").ok();

    // Use a sub-fn so the unsafe SAFETY blocks are scoped tightly.
    // `set_var` / `remove_var` are unsafe in 2024-edition Rust.
    fn set(k: &str, v: &str) {
        // SAFETY: single-threaded test; no other reader of this env
        // var is racing.
        unsafe { std::env::set_var(k, v) };
    }
    fn remove(k: &str) {
        // SAFETY: see `set`.
        unsafe { std::env::remove_var(k) };
    }

    remove("NEIGE_DISPATCHER_PERMITS");
    assert_eq!(Dispatcher::permits_from_env(8), 8, "unset → default");

    set("NEIGE_DISPATCHER_PERMITS", "");
    assert_eq!(Dispatcher::permits_from_env(8), 8, "empty → default");

    set("NEIGE_DISPATCHER_PERMITS", "not-a-number");
    assert_eq!(Dispatcher::permits_from_env(8), 8, "garbage → default");

    set("NEIGE_DISPATCHER_PERMITS", "0");
    assert_eq!(Dispatcher::permits_from_env(8), 8, "zero → default");

    set("NEIGE_DISPATCHER_PERMITS", "3");
    assert_eq!(Dispatcher::permits_from_env(8), 3, "valid → override");

    // Restore.
    match saved {
        Some(v) => set("NEIGE_DISPATCHER_PERMITS", &v),
        None => remove("NEIGE_DISPATCHER_PERMITS"),
    }
}

// ---------------------------------------------------------------
// #293 PR3b — push path: filter coverage and author gating.
// ---------------------------------------------------------------

use crate::card_role_cache::CardRoleCache;
use crate::event::{ArtifactRef, BroadcastEnvelope, EventScope};
use crate::ids::CoveId;
use calm_types::event::{ChannelVerdict, ChannelVerdictKind, RatifyDecision, ReviewSubject};

fn wave_scope(wave: &WaveId, cove: &CoveId) -> EventScope {
    EventScope::Wave {
        wave: wave.clone(),
        cove: cove.clone(),
    }
}

/// The dispatcher's `SubscribeFilter` must match only the push and
/// scheduler trigger kinds. We reconstruct the exact filter the spawn
/// site builds and assert `matches()` for each kind, plus retired
/// request kinds and a non-matching kind to prove the list is still a
/// closed allowlist (not "match everything").
#[test]
fn dispatcher_filter_matches_push_kinds() {
    let filter = SubscribeFilter {
        scope: SubscribeScope::Any,
        include_descendants: true,
        kinds: Some(vec![
            "task.completed".into(),
            "task.failed".into(),
            "task.gate_result".into(),
            "wave.report_edited".into(),
            "workspace.leased".into(),
            "workspace.released".into(),
            "forge.scan.completed".into(),
            "forge.pr.opened".into(),
            "forge.pr.checks".into(),
            "forge.issue.closed".into(),
            "worktree.provisioned".into(),
            "worktree.committed".into(),
            "forge.pr.merged".into(),
            "review.round".into(),
            "ratify.requested".into(),
            "ratify.resolved".into(),
            "codex.hook".into(),
            "claude.hook".into(),
            "plan.updated".into(),
            "wave.lifecycle_changed".into(),
            "wave.updated".into(),
        ]),
    };
    let wave = WaveId::from("w");
    let cove = CoveId::from("c");
    let scope = wave_scope(&wave, &cove);

    let env = |ev: Event| BroadcastEnvelope {
        id: 1,
        event_version: 1,
        actor: ActorId::User,
        scope: scope.clone(),
        event: ev,
    };

    // The retired worker_requested kinds no longer match.
    assert!(!filter.matches(&env(Event::CodexWorkerRequested {
        idempotency_key: "k".into(),
        goal: "g".into(),
        context: serde_json::Value::Null,
        acceptance_criteria: None,
        agent_message: None,
    })));
    assert!(!filter.matches(&env(Event::TerminalWorkerRequested {
        idempotency_key: "k".into(),
        cmd: "ls".into(),
        cwd: None,
        agent_message: None,
    })));
    // The push kinds match.
    assert!(filter.matches(&env(Event::TaskCompleted {
        idempotency_key: "k".into(),
        result: serde_json::Value::Null,
        artifacts: Vec::<ArtifactRef>::new(),
        agent_message: None,
    })));
    assert!(filter.matches(&env(Event::TaskFailed {
        idempotency_key: "k".into(),
        reason: "boom".into(),
        agent_message: None,
    })));
    // Issue #644 PR-C — gate verdicts route to the push branch
    // (and poke the scheduler).
    assert!(filter.matches(&env(Event::TaskGateResult {
        task_id: "w:k".into(),
        idempotency_key: "w:k".into(),
        passed: true,
        failing_step: None,
        exit_code: Some(0),
        log_tail: String::new(),
        log_path: "/tmp/gate.log".into(),
        attempt: 1,
        agent_message: None,
    })));
    assert!(filter.matches(&env(Event::WaveReportEdited {
        wave_id: wave.clone(),
        card_id: CardId::from("card"),
        author: EditAuthor::User,
        edit_id: "e".into(),
        summary_before: String::new(),
        summary_after: String::new(),
        body_before: String::new(),
        body_after: String::new(),
        agent_message: None,
    })));
    assert!(filter.matches(&env(Event::WorkspaceLeased {
        wave_id: wave.clone(),
        card_id: CardId::from("worker"),
        lease_id: "lease-1".into(),
        path: "/tmp/workspace".into(),
    })));
    assert!(filter.matches(&env(Event::WorkspaceReleased {
        wave_id: wave.clone(),
        card_id: CardId::from("worker"),
        lease_id: "lease-1".into(),
    })));
    assert!(filter.matches(&env(Event::ForgeScanCompleted {
        wave_id: wave.clone(),
        overlapping_prs: vec![1, 2],
    })));
    assert!(filter.matches(&env(Event::ForgePrOpened {
        wave_id: wave.clone(),
        pr_number: 1,
        head_sha: "head-sha".into(),
    })));
    assert!(filter.matches(&env(Event::ForgePrChecks {
        wave_id: wave.clone(),
        pr_number: 1,
        conclusion: "success".into(),
    })));
    assert!(filter.matches(&env(Event::ForgeIssueClosed {
        wave_id: wave.clone(),
        issue_number: 1,
    })));
    assert!(filter.matches(&env(Event::WorktreeProvisioned {
        wave_id: wave.clone(),
        card_id: CardId::from("worker"),
        path: "/tmp/worktree".into(),
    })));
    assert!(filter.matches(&env(Event::WorktreeCommitted {
        wave_id: wave.clone(),
        card_id: CardId::from("worker"),
        commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
        branch: "neige/w/card".into(),
    })));
    assert!(filter.matches(&env(Event::ForgePrMerged {
        wave_id: wave.clone(),
        subject: crate::event::ForgeMergeSubject {
            phase: "impl".into(),
            slice_id: "6".into(),
            pr_number: 1,
        },
        head_sha: "head-sha".into(),
        merge_sha: "merge-sha".into(),
    })));
    assert!(filter.matches(&env(Event::ReviewRound {
        wave_id: wave.clone(),
        subject: ReviewSubject {
            phase: "impl".into(),
            slice_id: "5b".into(),
            pr_number: Some(760),
        },
        head_sha: Some("head-sha".into()),
        n: 1,
        cap: 8,
        converged: false,
        channels: vec![ChannelVerdict {
            role: "design-correctness".into(),
            verdict: ChannelVerdictKind::ChangesRequested,
        }],
        root_cause: Some("tests failing".into()),
        idempotency_key: "review.round:w:impl:5b:760:1".into(),
    })));
    assert!(filter.matches(&env(Event::RatifyRequested {
        wave_id: wave.clone(),
        reason: "cap_exhausted".into(),
    })));
    assert!(filter.matches(&env(Event::RatifyResolved {
        wave_id: wave.clone(),
        decision: RatifyDecision::Grant,
    })));
    assert!(!filter.matches(&env(Event::ForgePrDiffRead {
        wave_id: wave.clone(),
        pr_number: 1,
        base_sha: "base-sha".into(),
        head_sha: "head-sha".into(),
        artifact_path: "/tmp/diff.patch".into(),
    })));
    assert!(!filter.matches(&env(Event::ForgeIssueRead {
        wave_id: wave.clone(),
        issue_number: 1,
        artifact_path: "/tmp/issue.md".into(),
    })));
    assert!(!filter.matches(&env(Event::WorktreeRemoved {
        wave_id: wave.clone(),
        card_id: CardId::from("worker"),
        path: "/tmp/worktree".into(),
    })));
    assert!(filter.matches(&env(Event::CodexHook {
        card_id: CardId::from("worker-codex"),
        kind: "hook.codex.stop".into(),
        hook_idempotency_key: "hook-codex".into(),
        payload: serde_json::Value::Null,
    })));
    assert!(filter.matches(&env(Event::ClaudeHook {
        card_id: CardId::from("worker-claude"),
        kind: "hook.claude.stop".into(),
        hook_idempotency_key: "hook-claude".into(),
        payload: serde_json::Value::Null,
    })));
    // Issue #644 PR-B — the scheduler trigger kinds match.
    assert!(filter.matches(&env(Event::PlanUpdated {
        wave_id: wave.clone(),
        changed_keys: vec!["impl-parser".into()],
        agent_message: None,
    })));
    assert!(filter.matches(&env(Event::WaveLifecycleChanged {
        id: wave.clone(),
        cove_id: cove.clone(),
        from: crate::model::WaveLifecycle::Draft,
        to: crate::model::WaveLifecycle::Planning,
        agent_message: None,
    })));
    // Round-2 review F4 — budget PATCHes emit only `wave.updated`
    // when the lifecycle is unchanged; it must reach the poke arm.
    assert!(filter.matches(&env(Event::WaveUpdated(
        crate::event::WaveUpdatedPayload::new(
            crate::model::Wave {
                id: wave.clone(),
                cove_id: cove.clone(),
                title: "w".into(),
                sort: 0.0,
                archived_at: None,
                pinned_at: None,
                lifecycle: crate::model::WaveLifecycle::Working,
                cwd: String::new(),
                workflow_id: None,
                purpose: None,
                workflow_input: None,
                terminal_at: None,
                created_at: 1,
                updated_at: 1,
            },
            None,
        )
    ))));
    // `task.dispatched` is emitted BY the scheduler inside its claim
    // tx and deliberately NOT subscribed (§5.1).
    assert!(!filter.matches(&env(Event::TaskDispatched {
        idempotency_key: "w:k".into(),
        kind: "codex".into(),
        agent_message: None,
    })));
    // A kind NOT in the list must not match — the filter is still a
    // closed allowlist.
    assert!(!filter.matches(&env(Event::WaveDeleted {
        id: wave.clone(),
        cove_id: cove.clone(),
    })));
}

/// The push branch in `handle_envelope` acts on a User-authored
/// `wave.report_edited` and ignores Spec/Kernel ones. The gating is a
/// simple `author == EditAuthor::User` check; assert that predicate
/// directly against each variant (the branch itself is exercised
/// end-to-end by the gated e2e).
#[test]
fn wave_report_edited_author_gating() {
    assert!(EditAuthor::User == EditAuthor::User);
    assert!(EditAuthor::Spec != EditAuthor::User);
    assert!(EditAuthor::Kernel != EditAuthor::User);
}

/// Issue #644 PR-C (§6.5) — the gated-self-report predicate the
/// live push branch and the boot replay both consult: TRUE exactly
/// for a `task.completed` whose key resolves to a tasks row with
/// `gate_json` set, plus (round-3 review F1) a `task.failed` for a
/// gated row that did NOT land a pre-gate failure on the row
/// (stale/retried report while the gate is in flight or decided).
/// Ungated rows, legacy keys (no row), genuine pre-gate failures
/// (`failed` + `worker-reported`/`spawn-failed`/`worker-timeout`),
/// and the gate
/// result itself all push.
#[tokio::test]
async fn gated_self_report_predicate() {
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mk_task = |key: &str, gate: Option<String>| crate::model::Task {
        id: format!("w:{key}"),
        wave_id: "w".into(),
        key: key.into(),
        kind: crate::model::TaskKind::Codex,
        goal: "g".into(),
        context_json: "null".into(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: "[]".into(),
        priority: 0,
        gate_json: gate,
        status: crate::model::TaskStatus::Verifying,
        status_detail: None,
        worker_card_id: None,
        gate_result_json: None,
        gate_attempt: 0,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        finished_at_ms: None,
    };
    let gate_json = || Some("{\"steps\":[{\"name\":\"t\",\"cmd\":\"true\"}]}".to_string());
    let gated = mk_task("gated", gate_json());
    let ungated = mk_task("ungated", None);
    // Gated rows whose worker genuinely failed pre-gate.
    let mut gated_worker_failed = mk_task("gated-worker-failed", gate_json());
    gated_worker_failed.status = crate::model::TaskStatus::Failed;
    gated_worker_failed.status_detail = Some("worker-reported".into());
    let mut gated_spawn_failed = mk_task("gated-spawn-failed", gate_json());
    gated_spawn_failed.status = crate::model::TaskStatus::Failed;
    gated_spawn_failed.status_detail = Some("spawn-failed".into());
    let mut gated_worker_timeout = mk_task("gated-worker-timeout", gate_json());
    gated_worker_timeout.status = crate::model::TaskStatus::Failed;
    gated_worker_timeout.status_detail = Some("worker-timeout".into());
    // Gated row the gate already failed — a late worker
    // `task.failed` retry must not re-wake the spec.
    let mut gated_gate_failed = mk_task("gated-gate-failed", gate_json());
    gated_gate_failed.status = crate::model::TaskStatus::Failed;
    gated_gate_failed.status_detail = Some("gate-red".into());
    // Gated row the gate already passed.
    let mut gated_done = mk_task("gated-done", gate_json());
    gated_done.status = crate::model::TaskStatus::Done;
    // Ungated row that failed — ungated failures always push.
    let mut ungated_failed = mk_task("ungated-failed", None);
    ungated_failed.status = crate::model::TaskStatus::Failed;
    ungated_failed.status_detail = Some("worker-reported".into());
    crate::db::write_in_tx_typed(&repo, move |tx| {
        Box::pin(async move {
            for t in [
                &gated,
                &ungated,
                &gated_worker_failed,
                &gated_spawn_failed,
                &gated_worker_timeout,
                &gated_gate_failed,
                &gated_done,
                &ungated_failed,
            ] {
                crate::db::sqlite::task_insert_tx(tx, t).await?;
            }
            Ok(())
        })
    })
    .await
    .expect("seed tasks");

    let completed = |key: &str| Event::TaskCompleted {
        idempotency_key: format!("w:{key}"),
        result: serde_json::Value::Null,
        artifacts: Vec::new(),
        agent_message: None,
    };
    let failed = |key: &str| Event::TaskFailed {
        idempotency_key: format!("w:{key}"),
        reason: "boom".into(),
        agent_message: None,
    };
    assert!(is_gated_self_report(&repo, &completed("gated")).await);
    assert!(!is_gated_self_report(&repo, &completed("ungated")).await);
    assert!(
        !is_gated_self_report(&repo, &completed("legacy-no-row")).await,
        "legacy keys with no tasks row push as today"
    );
    // Round-3 review F1 — gated `task.failed` matrix.
    assert!(
        is_gated_self_report(&repo, &failed("gated")).await,
        "stale task.failed while the gate is in flight (`verifying`) is suppressed"
    );
    assert!(
        is_gated_self_report(&repo, &failed("gated-gate-failed")).await,
        "late task.failed after the gate already failed the row is suppressed"
    );
    assert!(
        is_gated_self_report(&repo, &failed("gated-done")).await,
        "late task.failed after the gate already passed the row is suppressed"
    );
    assert!(
        !is_gated_self_report(&repo, &failed("gated-worker-failed")).await,
        "a genuine pre-gate worker failure pushes as today (no gate runs on failure)"
    );
    assert!(
        !is_gated_self_report(&repo, &failed("gated-spawn-failed")).await,
        "a spawn failure pushes as today (no gate runs on failure)"
    );
    assert!(
        !is_gated_self_report(&repo, &failed("gated-worker-timeout")).await,
        "a worker liveness timeout pushes as a pre-gate failure"
    );
    assert!(
        !is_gated_self_report(&repo, &failed("ungated-failed")).await,
        "ungated failures keep today's behavior"
    );
    assert!(
        !is_gated_self_report(&repo, &failed("legacy-no-row")).await,
        "legacy task.failed keys with no tasks row push as today"
    );
    assert!(
        !is_gated_self_report(
            &repo,
            &Event::TaskGateResult {
                task_id: "w:gated".into(),
                idempotency_key: "w:gated".into(),
                passed: true,
                failing_step: None,
                exit_code: Some(0),
                log_tail: String::new(),
                log_path: "/tmp/gate.log".into(),
                attempt: 1,
                agent_message: None,
            }
        )
        .await,
        "the gate verdict itself is never suppressed"
    );
}

/// Issue #644 PR-C — `task.gate_result` maps to the hard-fire
/// `Observation::TaskGateResult`, with the plan key recovered from
/// the `"{wave_id}:{key}"` task-id convention (§2.1).
#[test]
fn gate_result_maps_to_hard_fire_observation_with_plan_key() {
    let wave = WaveId::from("wave-1");
    let event = Event::TaskGateResult {
        task_id: "wave-1:impl-parser".into(),
        idempotency_key: "wave-1:impl-parser".into(),
        passed: false,
        failing_step: Some("test".into()),
        exit_code: Some(101),
        log_tail: "boom".into(),
        log_path: "/tmp/gate-logs/wave-1:impl-parser-g2.log".into(),
        attempt: 2,
        agent_message: None,
    };
    let obs = harness_observation_from_event(&wave, &event)
        .expect("gate result must map to an observation");
    assert!(obs.is_hard_fire(), "gate results are hard-fired (§6.5)");
    match &obs {
        HarnessObservation::TaskGateResult {
            idempotency_key,
            key,
            passed,
            failing_step,
            exit_code,
            attempt,
            ..
        } => {
            assert_eq!(idempotency_key, "wave-1:impl-parser");
            assert_eq!(key, "impl-parser", "plan key = task id minus wave prefix");
            assert!(!passed);
            assert_eq!(failing_step.as_deref(), Some("test"));
            assert_eq!(*exit_code, Some(101));
            assert_eq!(*attempt, 2);
        }
        other => panic!("expected TaskGateResult observation, got {other:?}"),
    }
    let text = obs.to_turn_text();
    assert!(text.contains("Task impl-parser gate FAILED at step test (exit 101)"));
    assert!(text.contains("plan/impl-parser/gate.log"));
    assert!(text.contains("runs/wave-1:impl-parser.md"));
}

#[test]
fn event_warrants_spec_push_covers_push_allowlist() {
    let cache = CardRoleCache::new();
    let wave = WaveId::from("w");
    let cove = CoveId::from("c");
    let worker = CardId::from("worker");
    let spec = CardId::from("spec");
    let unknown = CardId::from("unknown");
    cache.insert(worker.clone(), CardRole::Worker, wave.clone());
    cache.insert(spec.clone(), CardRole::Spec, wave.clone());
    let write = WriteContext::new(cache, crate::wave_cove_cache::WaveCoveCache::new());

    let completed = Event::TaskCompleted {
        idempotency_key: "done".into(),
        result: serde_json::Value::Null,
        artifacts: Vec::new(),
        agent_message: None,
    };
    assert!(event_warrants_spec_push(
        &completed,
        &ActorId::AiCodex(worker.clone()),
        &write
    ));
    assert!(!event_warrants_spec_push(
        &completed,
        &ActorId::AiSpec(spec.clone()),
        &write
    ));

    let failed = Event::TaskFailed {
        idempotency_key: "fail".into(),
        reason: "boom".into(),
        agent_message: None,
    };
    assert!(event_warrants_spec_push(
        &failed,
        &ActorId::AiCodex(worker.clone()),
        &write
    ));
    assert!(!event_warrants_spec_push(
        &failed,
        &ActorId::AiSpec(spec.clone()),
        &write
    ));

    // Issue #644 PR-C — the gate verdict always warrants a push
    // (kernel-only kind; the gated-self-report consultation is a
    // separate async predicate).
    let gate_result = Event::TaskGateResult {
        task_id: "w:k".into(),
        idempotency_key: "w:k".into(),
        passed: false,
        failing_step: Some("test".into()),
        exit_code: Some(101),
        log_tail: "boom".into(),
        log_path: "/tmp/gate.log".into(),
        attempt: 1,
        agent_message: None,
    };
    assert!(event_warrants_spec_push(
        &gate_result,
        &ActorId::KernelDispatcher,
        &write
    ));

    let report = |author| Event::WaveReportEdited {
        wave_id: wave.clone(),
        card_id: spec.clone(),
        author,
        edit_id: "edit".into(),
        summary_before: String::new(),
        summary_after: String::new(),
        body_before: String::new(),
        body_after: String::new(),
        agent_message: None,
    };
    assert!(event_warrants_spec_push(
        &report(EditAuthor::User),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_spec_push(
        &report(EditAuthor::Spec),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_spec_push(
        &report(EditAuthor::Kernel),
        &ActorId::User,
        &write
    ));

    // Issue #760 slice ⑦ — workspace lease lifecycle events always warrant a
    // push (kernel-emitted; no author/role gate).
    let leased = Event::WorkspaceLeased {
        wave_id: wave.clone(),
        card_id: worker.clone(),
        lease_id: "lease".into(),
        path: "/tmp/ws".into(),
    };
    assert!(event_warrants_spec_push(
        &leased,
        &ActorId::KernelDispatcher,
        &write
    ));
    let released = Event::WorkspaceReleased {
        wave_id: wave.clone(),
        card_id: worker.clone(),
        lease_id: "lease".into(),
    };
    assert!(event_warrants_spec_push(
        &released,
        &ActorId::KernelDispatcher,
        &write
    ));

    for forge_event in [
        Event::ForgePrMerged {
            wave_id: wave.clone(),
            subject: crate::event::ForgeMergeSubject {
                phase: "impl".into(),
                slice_id: "6".into(),
                pr_number: 1,
            },
            head_sha: "head-sha".into(),
            merge_sha: "merge-sha".into(),
        },
        Event::ReviewRound {
            wave_id: wave.clone(),
            subject: ReviewSubject {
                phase: "impl".into(),
                slice_id: "5b".into(),
                pr_number: Some(760),
            },
            head_sha: Some("head-sha".into()),
            n: 1,
            cap: 8,
            converged: false,
            channels: vec![ChannelVerdict {
                role: "design-correctness".into(),
                verdict: ChannelVerdictKind::ChangesRequested,
            }],
            root_cause: Some("tests failing".into()),
            idempotency_key: "review.round:w:impl:5b:760:1".into(),
        },
        Event::RatifyRequested {
            wave_id: wave.clone(),
            reason: "cap_exhausted".into(),
        },
        Event::RatifyResolved {
            wave_id: wave.clone(),
            decision: RatifyDecision::Grant,
        },
        Event::ForgeScanCompleted {
            wave_id: wave.clone(),
            overlapping_prs: vec![1, 2],
        },
        Event::ForgePrOpened {
            wave_id: wave.clone(),
            pr_number: 1,
            head_sha: "head-sha".into(),
        },
        Event::ForgePrChecks {
            wave_id: wave.clone(),
            pr_number: 1,
            conclusion: "success".into(),
        },
        Event::ForgeIssueClosed {
            wave_id: wave.clone(),
            issue_number: 1,
        },
        Event::WorktreeProvisioned {
            wave_id: wave.clone(),
            card_id: worker.clone(),
            path: "/tmp/worktree".into(),
        },
        Event::WorktreeCommitted {
            wave_id: wave.clone(),
            card_id: worker.clone(),
            commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            branch: "neige/w/card".into(),
        },
    ] {
        assert!(event_warrants_spec_push(
            &forge_event,
            &ActorId::KernelDispatcher,
            &write
        ));
    }
    assert!(!event_warrants_spec_push(
        &Event::ForgePrDiffRead {
            wave_id: wave.clone(),
            pr_number: 1,
            base_sha: "base-sha".into(),
            head_sha: "head-sha".into(),
            artifact_path: "/tmp/diff.patch".into(),
        },
        &ActorId::KernelDispatcher,
        &write
    ));
    assert!(!event_warrants_spec_push(
        &Event::WorktreeRemoved {
            wave_id: wave.clone(),
            card_id: worker.clone(),
            path: "/tmp/worktree".into(),
        },
        &ActorId::KernelDispatcher,
        &write
    ));

    let codex_hook = |card_id: CardId, kind: &str| Event::CodexHook {
        card_id,
        kind: kind.into(),
        hook_idempotency_key: format!("hook-codex-{kind}"),
        payload: serde_json::Value::Null,
    };
    let claude_hook = |card_id: CardId, kind: &str| Event::ClaudeHook {
        card_id,
        kind: kind.into(),
        hook_idempotency_key: format!("hook-claude-{kind}"),
        payload: serde_json::Value::Null,
    };
    assert!(event_warrants_spec_push(
        &codex_hook(worker.clone(), "hook.codex.stop"),
        &ActorId::User,
        &write
    ));
    assert!(event_warrants_spec_push(
        &claude_hook(worker.clone(), "hook.claude.stop"),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_spec_push(
        &codex_hook(spec.clone(), "hook.codex.stop"),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_spec_push(
        &claude_hook(spec.clone(), "hook.claude.stop"),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_spec_push(
        &codex_hook(unknown.clone(), "hook.codex.stop"),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_spec_push(
        &claude_hook(unknown, "hook.claude.stop"),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_spec_push(
        &codex_hook(worker.clone(), "hook.codex.permission_request"),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_spec_push(
        &codex_hook(worker, "hook.codex.post_tool_use"),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_spec_push(
        &Event::WaveDeleted {
            id: wave,
            cove_id: cove,
        },
        &ActorId::User,
        &write
    ));
}

/// #679 PR0-E — actor-matrix pin for task terminal events plus the
/// request-kind exclusion. `event_warrants_spec_push_covers_push_allowlist`
/// above pins the AiCodex/AiSpec rows; this pins the remaining actor
/// variants (only `AiSpec` is excluded — everything else pushes) and
/// that the two `*.worker_requested` kinds never push back to the spec
/// regardless of actor.
#[test]
fn event_warrants_spec_push_task_actor_matrix_and_request_kinds_pin() {
    let cache = CardRoleCache::new();
    let wave = WaveId::from("w");
    let worker = CardId::from("worker");
    let spec = CardId::from("spec");
    cache.insert(worker.clone(), CardRole::Worker, wave.clone());
    cache.insert(spec.clone(), CardRole::Spec, wave.clone());
    let write = WriteContext::new(cache, crate::wave_cove_cache::WaveCoveCache::new());

    let completed = Event::TaskCompleted {
        idempotency_key: "done".into(),
        result: serde_json::Value::Null,
        artifacts: Vec::new(),
        agent_message: None,
    };
    let failed = Event::TaskFailed {
        idempotency_key: "fail".into(),
        reason: "boom".into(),
        agent_message: None,
    };
    // Every non-AiSpec actor warrants a push for task terminal events —
    // including the kernel dispatcher itself (its spawn-failure
    // `task.failed` fallback must wake the spec).
    for actor in [
        ActorId::User,
        ActorId::Kernel,
        ActorId::KernelDispatcher,
        ActorId::Plugin("p".into()),
        ActorId::AiClaude(worker.clone()),
        ActorId::AiCodexSession(WorkerSessionId::from("sess-codex")),
        ActorId::AiClaudeSession(WorkerSessionId::from("sess-claude")),
    ] {
        assert!(
            event_warrants_spec_push(&completed, &actor, &write),
            "task.completed must push for actor {actor}"
        );
        assert!(
            event_warrants_spec_push(&failed, &actor, &write),
            "task.failed must push for actor {actor}"
        );
    }
    for actor in [
        ActorId::AiSpec(spec.clone()),
        ActorId::AiSpecSession(WorkerSessionId::from("sess-spec")),
    ] {
        assert!(
            !event_warrants_spec_push(&completed, &actor, &write),
            "task.completed must not self-push for actor {actor}"
        );
        assert!(
            !event_warrants_spec_push(&failed, &actor, &write),
            "task.failed must not self-push for actor {actor}"
        );
    }

    // The two request kinds are dispatcher *inputs*, never spec pushes
    // — for any actor, including the spec that authored them.
    let codex_req = Event::CodexWorkerRequested {
        idempotency_key: "k".into(),
        goal: "g".into(),
        context: serde_json::Value::Null,
        acceptance_criteria: None,
        agent_message: None,
    };
    let terminal_req = Event::TerminalWorkerRequested {
        idempotency_key: "k".into(),
        cmd: "ls".into(),
        cwd: None,
        agent_message: None,
    };
    for actor in [
        ActorId::User,
        ActorId::KernelDispatcher,
        ActorId::AiSpec(spec.clone()),
        ActorId::AiCodex(worker.clone()),
    ] {
        assert!(
            !event_warrants_spec_push(&codex_req, &actor, &write),
            "codex.worker_requested must never push for actor {actor}"
        );
        assert!(
            !event_warrants_spec_push(&terminal_req, &actor, &write),
            "terminal.worker_requested must never push for actor {actor}"
        );
    }
}

/// #679 PR0-E — characterization golden for the event → harness
/// observation mapping. Both the live push path and the boot-recovery
/// replay (`harness::replay_harness_events_since`) funnel through
/// `harness_observation_from_event`; PR5-8 must preserve this mapping
/// byte-for-byte or consciously edit this pin.
#[test]
fn harness_observation_from_event_mapping_pin() {
    let wave = WaveId::from("wave-map");
    let worker = CardId::from("card-map");

    // task.completed — idempotency key + verbatim result.
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::TaskCompleted {
                idempotency_key: "map-a".into(),
                result: serde_json::json!({"ok": true, "n": 7}),
                artifacts: vec![ArtifactRef::from("art-1")],
                agent_message: Some("ignored".into()),
            }
        ),
        Some(HarnessObservation::TaskCompleted {
            idempotency_key: "map-a".into(),
            result: serde_json::json!({"ok": true, "n": 7}),
        })
    );

    // task.failed — the event's `reason` becomes the observation `error`.
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::TaskFailed {
                idempotency_key: "map-b".into(),
                reason: "boom".into(),
                agent_message: None,
            }
        ),
        Some(HarnessObservation::TaskFailed {
            idempotency_key: "map-b".into(),
            error: "boom".into(),
        })
    );

    // wave.report_edited — body_after verbatim + its sha256 (golden hex
    // computed externally, NOT via the same sha256_hex helper).
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::WaveReportEdited {
                wave_id: wave.clone(),
                card_id: worker.clone(),
                author: EditAuthor::User,
                edit_id: "e".into(),
                summary_before: String::new(),
                summary_after: "s".into(),
                body_before: "old".into(),
                body_after: "loop-pin-body".into(),
                agent_message: None,
            }
        ),
        Some(HarnessObservation::ReportEdited {
            wave_id: wave.clone(),
            body_sha256: "09b37878497ec46015d1913ba0dff1cd051ca244859c80f4a3fc14d88a4a9465".into(),
            body: "loop-pin-body".into(),
        })
    );

    // workspace.* — lifecycle carrier events map through the payload
    // fields and use the caller-provided wave id like wave.report_edited.
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::WorkspaceLeased {
                wave_id: WaveId::from("payload-wave-ignored"),
                card_id: worker.clone(),
                lease_id: "lease-map".into(),
                path: "/tmp/workspace-map".into(),
            }
        ),
        Some(HarnessObservation::WorkspaceLeased {
            wave_id: wave.clone(),
            card_id: worker.clone(),
            lease_id: "lease-map".into(),
            path: "/tmp/workspace-map".into(),
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::WorkspaceReleased {
                wave_id: WaveId::from("payload-wave-ignored"),
                card_id: worker.clone(),
                lease_id: "lease-map".into(),
            }
        ),
        Some(HarnessObservation::WorkspaceReleased {
            wave_id: wave.clone(),
            card_id: worker.clone(),
            lease_id: "lease-map".into(),
        })
    );

    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::ForgePrMerged {
                wave_id: WaveId::from("payload-wave-ignored"),
                subject: crate::event::ForgeMergeSubject {
                    phase: "impl".into(),
                    slice_id: "6".into(),
                    pr_number: 760,
                },
                head_sha: "head-sha".into(),
                merge_sha: "merge-sha".into(),
            }
        ),
        Some(HarnessObservation::ForgePrMerged {
            wave_id: wave.clone(),
            pr_number: 760,
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::ReviewRound {
                wave_id: WaveId::from("payload-wave-ignored"),
                subject: ReviewSubject {
                    phase: "impl".into(),
                    slice_id: "5b".into(),
                    pr_number: Some(760),
                },
                head_sha: Some("head-sha".into()),
                n: 1,
                cap: 8,
                converged: false,
                channels: vec![ChannelVerdict {
                    role: "design-correctness".into(),
                    verdict: ChannelVerdictKind::ChangesRequested,
                }],
                root_cause: Some("tests failing".into()),
                idempotency_key: "review.round:wave-map:impl:5b:760:1".into(),
            }
        ),
        Some(HarnessObservation::ReviewRound {
            wave_id: wave.clone(),
            phase: "impl".into(),
            slice_id: "5b".into(),
            pr_number: Some(760),
            head_sha: Some("head-sha".into()),
            n: 1,
            cap: 8,
            converged: false,
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::RatifyRequested {
                wave_id: WaveId::from("payload-wave-ignored"),
                reason: "cap_exhausted".into(),
            }
        ),
        Some(HarnessObservation::RatifyRequested {
            wave_id: wave.clone(),
            reason: "cap_exhausted".into(),
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::RatifyResolved {
                wave_id: WaveId::from("payload-wave-ignored"),
                decision: RatifyDecision::Deny,
            }
        ),
        Some(HarnessObservation::RatifyResolved {
            wave_id: wave.clone(),
            decision: RatifyDecision::Deny,
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::ForgeScanCompleted {
                wave_id: WaveId::from("payload-wave-ignored"),
                overlapping_prs: vec![1, 2],
            }
        ),
        Some(HarnessObservation::ForgeScanCompleted {
            wave_id: wave.clone(),
            overlapping_prs: vec![1, 2],
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::ForgePrOpened {
                wave_id: WaveId::from("payload-wave-ignored"),
                pr_number: 1,
                head_sha: "head-sha".into(),
            }
        ),
        Some(HarnessObservation::ForgePrOpened {
            wave_id: wave.clone(),
            pr_number: 1,
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::ForgePrChecks {
                wave_id: WaveId::from("payload-wave-ignored"),
                pr_number: 1,
                conclusion: "success".into(),
            }
        ),
        Some(HarnessObservation::ForgePrChecks {
            wave_id: wave.clone(),
            pr_number: 1,
            conclusion: "success".into(),
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::ForgeIssueClosed {
                wave_id: WaveId::from("payload-wave-ignored"),
                issue_number: 760,
            }
        ),
        Some(HarnessObservation::ForgeIssueClosed {
            wave_id: wave.clone(),
            issue_number: 760,
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::WorktreeProvisioned {
                wave_id: WaveId::from("payload-wave-ignored"),
                card_id: worker.clone(),
                path: "/tmp/worktree-map".into(),
            }
        ),
        Some(HarnessObservation::WorktreeProvisioned {
            wave_id: wave.clone(),
            card_id: worker.clone(),
            path: "/tmp/worktree-map".into(),
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::WorktreeCommitted {
                wave_id: WaveId::from("payload-wave-ignored"),
                card_id: worker.clone(),
                commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
                branch: "neige/w/card".into(),
            }
        ),
        Some(HarnessObservation::WorktreeCommitted {
            wave_id: wave.clone(),
            card_id: worker.clone(),
            commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            branch: "neige/w/card".into(),
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::ForgePrDiffRead {
                wave_id: WaveId::from("payload-wave-ignored"),
                pr_number: 1,
                base_sha: "base-sha".into(),
                head_sha: "head-sha".into(),
                artifact_path: "/tmp/diff.patch".into(),
            }
        ),
        None
    );
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::WorktreeRemoved {
                wave_id: WaveId::from("payload-wave-ignored"),
                card_id: worker.clone(),
                path: "/tmp/worktree-map".into(),
            }
        ),
        None
    );

    // Stop hooks — exact kind discriminators map to WorkerHookStop.
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::CodexHook {
                card_id: worker.clone(),
                kind: "hook.codex.stop".into(),
                hook_idempotency_key: "hook-c".into(),
                payload: serde_json::Value::Null,
            }
        ),
        Some(HarnessObservation::WorkerHookStop {
            wave_id: wave.clone(),
            card_id: worker.clone(),
            kind: HarnessHookKind::CodexStop,
            idempotency_key: "hook-c".into(),
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::ClaudeHook {
                card_id: worker.clone(),
                kind: "hook.claude.stop".into(),
                hook_idempotency_key: "hook-l".into(),
                payload: serde_json::Value::Null,
            }
        ),
        Some(HarnessObservation::WorkerHookStop {
            wave_id: wave.clone(),
            card_id: worker.clone(),
            kind: HarnessHookKind::ClaudeStop,
            idempotency_key: "hook-l".into(),
        })
    );

    // Non-stop hooks and non-push kinds map to nothing.
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::CodexHook {
                card_id: worker.clone(),
                kind: "hook.codex.permission_request".into(),
                hook_idempotency_key: "hook-p".into(),
                payload: serde_json::Value::Null,
            }
        ),
        None
    );
    assert_eq!(
        harness_observation_from_event(
            &wave,
            &Event::CodexWorkerRequested {
                idempotency_key: "k".into(),
                goal: "g".into(),
                context: serde_json::Value::Null,
                acceptance_criteria: None,
                agent_message: None,
            }
        ),
        None
    );
}

/// One consistency-table row: a representative event + actor and the
/// expected verdict at BOTH #828 seams. `expect_push` and
/// `expect_observation` are separate fields (not one bool) because the
/// invariant is one-directional — predicate ⇒ mapping. Conditional
/// kinds carry false-side rows where the observation stays `Some`
/// (the mapping is per-kind; the predicate additionally gates on
/// actor/author/role).
struct SpecPushWiringRow {
    event: Event,
    actor: ActorId,
    expect_push: bool,
    expect_observation: bool,
}

/// Canonical census of every `Event` kind tag, derived from the
/// derived deserializer's unknown-variant diagnostic: serde lists the
/// complete accepted-tag set when asked to parse an unknown `ev`, so
/// this census cannot drift from the enum — adding a variant grows
/// the set automatically, and the completeness assertion in
/// `spec_push_predicate_and_observation_mapping_agree` then fails
/// until the table gains a row. The canary assert fails loudly (with
/// the raw diagnostic) if serde's message shape ever changes, rather
/// than letting the census silently shrink.
fn all_event_kind_tags() -> std::collections::BTreeSet<String> {
    let err = Event::from_kind_and_payload("__not_an_event_kind__", serde_json::Value::Null)
        .expect_err("an unknown kind tag must fail to deserialize");
    let msg = err.to_string();
    let (_, list) = msg
        .split_once("expected one of ")
        .unwrap_or_else(|| panic!("serde unknown-variant diagnostic changed shape: {msg}"));
    // The list is backtick-quoted: `a`, `b`, … — take the odd split
    // segments.
    let mut tags: std::collections::BTreeSet<String> = list
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    // The diagnostic also lists `#[serde(alias)]` spellings —
    // alternate names for kinds already counted (the deprecated
    // pre-#644 `*.job_requested` tags kept for old-log replay), not
    // kinds of their own: `kind_tag()` never emits them, so a table
    // row can never cover them. Strip the known ones; a FUTURE alias
    // fails the completeness assertion loudly until recorded here.
    for alias in ["codex.job_requested", "terminal.job_requested"] {
        assert!(
            tags.remove(alias),
            "stale alias {alias:?} no longer in the deserializer; drop it from this list"
        );
    }
    assert!(
        tags.contains("cove.updated") && tags.contains("task.completed") && tags.len() >= 46,
        "kind census parse failed; raw diagnostic: {msg}"
    );
    tags
}

/// #828 slice 1 — predicate⇒mapping consistency table over EVERY
/// event kind. Each row checks the push predicate
/// (`event_warrants_spec_push`) and the harness-observation mapping
/// (`harness_observation_from_event`) jointly, and the
/// `all_event_kind_tags` census asserts the table covers every kind:
/// a new `Event` variant breaks compilation at the two exhaustive
/// seams AND fails this test until a row records its expected wiring
/// — so a variant wired predicate=true / mapping=None while fixing
/// the seam compile errors can no longer slip through by convention.
///
/// The invariant is one-directional (predicate ⇒ mapping), enforced
/// structurally on the expectations themselves: a row that expects
/// push without an observation is rejected before the seams are even
/// consulted. Conditional kinds carry false-side rows (spec-actor
/// task terminals, spec-authored report edit, stop hooks on
/// spec/unknown-role cards, non-stop hooks for both providers) whose
/// observation column shows the mapping staying `Some` where it is
/// kind-scoped. The exhaustive actor/author/role matrices remain
/// pinned in `event_warrants_spec_push_covers_push_allowlist` and
/// `event_warrants_spec_push_task_actor_matrix_and_request_kinds_pin`;
/// this table owns per-kind coverage and cross-seam agreement.
#[test]
fn spec_push_predicate_and_observation_mapping_agree() {
    let cache = CardRoleCache::new();
    let wave = WaveId::from("w");
    let cove = CoveId::from("c");
    let worker = CardId::from("worker");
    let spec = CardId::from("spec");
    let unknown = CardId::from("unknown");
    cache.insert(worker.clone(), CardRole::Worker, wave.clone());
    cache.insert(spec.clone(), CardRole::Spec, wave.clone());
    let write = WriteContext::new(cache, crate::wave_cove_cache::WaveCoveCache::new());

    let row = |event: Event, actor: ActorId, expect_push: bool, expect_observation: bool| {
        SpecPushWiringRow {
            event,
            actor,
            expect_push,
            expect_observation,
        }
    };
    let codex_hook = |card_id: &CardId, kind: &str| Event::CodexHook {
        card_id: card_id.clone(),
        kind: kind.into(),
        hook_idempotency_key: format!("hook-codex-{kind}"),
        payload: serde_json::Value::Null,
    };
    let claude_hook = |card_id: &CardId, kind: &str| Event::ClaudeHook {
        card_id: card_id.clone(),
        kind: kind.into(),
        hook_idempotency_key: format!("hook-claude-{kind}"),
        payload: serde_json::Value::Null,
    };
    let report_edited = |author: EditAuthor| Event::WaveReportEdited {
        wave_id: wave.clone(),
        card_id: spec.clone(),
        author,
        edit_id: "e".into(),
        summary_before: String::new(),
        summary_after: String::new(),
        body_before: String::new(),
        body_after: "body".into(),
        agent_message: None,
    };
    let task_completed = || Event::TaskCompleted {
        idempotency_key: "w:k".into(),
        result: serde_json::Value::Null,
        artifacts: Vec::new(),
        agent_message: None,
    };
    let task_failed = || Event::TaskFailed {
        idempotency_key: "w:k".into(),
        reason: "boom".into(),
        agent_message: None,
    };
    let card_sample = || crate::model::Card {
        id: worker.clone(),
        wave_id: wave.clone(),
        kind: "terminal".into(),
        sort: 0.0,
        payload: serde_json::Value::Null,
        runtime: None,
        deletable: true,
        created_at: 1,
        updated_at: 1,
    };

    let rows: Vec<SpecPushWiringRow> = vec![
        // -- Push-capable kinds, push-side rows: predicate true ⇒
        //    mapping Some. ------------------------------------------
        row(
            task_completed(),
            ActorId::AiCodex(worker.clone()),
            true,
            true,
        ),
        row(task_failed(), ActorId::AiCodex(worker.clone()), true, true),
        row(
            Event::TaskGateResult {
                task_id: "w:k".into(),
                idempotency_key: "w:k".into(),
                passed: true,
                failing_step: None,
                exit_code: Some(0),
                log_tail: String::new(),
                log_path: "/tmp/gate.log".into(),
                attempt: 1,
                agent_message: None,
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(report_edited(EditAuthor::User), ActorId::User, true, true),
        row(
            Event::WorkspaceLeased {
                wave_id: wave.clone(),
                card_id: worker.clone(),
                lease_id: "lease".into(),
                path: "/tmp/ws".into(),
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::WorkspaceReleased {
                wave_id: wave.clone(),
                card_id: worker.clone(),
                lease_id: "lease".into(),
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::ForgePrMerged {
                wave_id: wave.clone(),
                subject: crate::event::ForgeMergeSubject {
                    phase: "impl".into(),
                    slice_id: "6".into(),
                    pr_number: 1,
                },
                head_sha: "head-sha".into(),
                merge_sha: "merge-sha".into(),
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::ReviewRound {
                wave_id: wave.clone(),
                subject: ReviewSubject {
                    phase: "impl".into(),
                    slice_id: "5b".into(),
                    pr_number: Some(760),
                },
                head_sha: Some("head-sha".into()),
                n: 1,
                cap: 8,
                converged: false,
                channels: vec![ChannelVerdict {
                    role: "design-correctness".into(),
                    verdict: ChannelVerdictKind::ChangesRequested,
                }],
                root_cause: None,
                idempotency_key: "review.round:w:impl:5b:760:1".into(),
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::RatifyRequested {
                wave_id: wave.clone(),
                reason: "cap_exhausted".into(),
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::RatifyResolved {
                wave_id: wave.clone(),
                decision: RatifyDecision::Grant,
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::ForgeScanCompleted {
                wave_id: wave.clone(),
                overlapping_prs: vec![1, 2],
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::ForgePrOpened {
                wave_id: wave.clone(),
                pr_number: 1,
                head_sha: "head-sha".into(),
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::ForgePrChecks {
                wave_id: wave.clone(),
                pr_number: 1,
                conclusion: "success".into(),
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::ForgeIssueClosed {
                wave_id: wave.clone(),
                issue_number: 1,
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::WorktreeProvisioned {
                wave_id: wave.clone(),
                card_id: worker.clone(),
                path: "/tmp/worktree".into(),
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::WorktreeCommitted {
                wave_id: wave.clone(),
                card_id: worker.clone(),
                commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
                branch: "neige/w/card".into(),
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            codex_hook(&worker, "hook.codex.stop"),
            ActorId::User,
            true,
            true,
        ),
        row(
            claude_hook(&worker, "hook.claude.stop"),
            ActorId::User,
            true,
            true,
        ),
        // -- Conditional kinds, false side: predicate false while the
        //    kind-scoped mapping stays Some — the exact asymmetry a
        //    single shared bool could not express. -------------------
        row(task_completed(), ActorId::AiSpec(spec.clone()), false, true),
        row(task_failed(), ActorId::AiSpec(spec.clone()), false, true),
        row(
            report_edited(EditAuthor::Spec),
            ActorId::AiSpec(spec.clone()),
            false,
            true,
        ),
        row(
            report_edited(EditAuthor::Kernel),
            ActorId::Kernel,
            false,
            true,
        ),
        row(
            codex_hook(&spec, "hook.codex.stop"),
            ActorId::User,
            false,
            true,
        ),
        row(
            claude_hook(&spec, "hook.claude.stop"),
            ActorId::User,
            false,
            true,
        ),
        row(
            codex_hook(&unknown, "hook.codex.stop"),
            ActorId::User,
            false,
            true,
        ),
        row(
            claude_hook(&unknown, "hook.claude.stop"),
            ActorId::User,
            false,
            true,
        ),
        // Non-stop hooks map to nothing on either seam — both
        // providers.
        row(
            codex_hook(&worker, "hook.codex.permission_request"),
            ActorId::User,
            false,
            false,
        ),
        row(
            claude_hook(&worker, "hook.claude.post_tool_use"),
            ActorId::User,
            false,
            false,
        ),
        // -- Never-push kinds: explicit-false on both seams. ---------
        row(
            Event::CoveUpdated(crate::model::Cove {
                id: cove.clone(),
                name: "c".into(),
                color: "#000000".into(),
                sort: 0.0,
                kind: crate::model::CoveKind::User,
                created_at: 1,
                updated_at: 1,
            }),
            ActorId::User,
            false,
            false,
        ),
        row(
            Event::CoveDeleted { id: cove.clone() },
            ActorId::User,
            false,
            false,
        ),
        row(
            Event::WaveUpdated(crate::event::WaveUpdatedPayload::new(
                crate::model::Wave {
                    id: wave.clone(),
                    cove_id: cove.clone(),
                    title: "w".into(),
                    sort: 0.0,
                    archived_at: None,
                    pinned_at: None,
                    lifecycle: crate::model::WaveLifecycle::Working,
                    cwd: String::new(),
                    workflow_id: None,
                    purpose: None,
                    workflow_input: None,
                    terminal_at: None,
                    created_at: 1,
                    updated_at: 1,
                },
                None,
            )),
            ActorId::User,
            false,
            false,
        ),
        row(
            Event::WaveDeleted {
                id: wave.clone(),
                cove_id: cove.clone(),
            },
            ActorId::User,
            false,
            false,
        ),
        row(
            Event::WaveLifecycleChanged {
                id: wave.clone(),
                cove_id: cove.clone(),
                from: crate::model::WaveLifecycle::Draft,
                to: crate::model::WaveLifecycle::Planning,
                agent_message: None,
            },
            ActorId::User,
            false,
            false,
        ),
        row(Event::CardAdded(card_sample()), ActorId::User, false, false),
        row(
            Event::CardUpdated(card_sample()),
            ActorId::User,
            false,
            false,
        ),
        row(
            Event::CardDeleted {
                id: worker.clone(),
                wave_id: wave.clone(),
            },
            ActorId::User,
            false,
            false,
        ),
        row(
            Event::RuntimeStarted {
                runtime_id: "rt".into(),
                card_id: worker.to_string(),
                kind: calm_types::runtime::WorkerSessionKind::CodexCard,
                agent_provider: Some(calm_types::runtime::AgentProvider::Codex),
                status: calm_types::worker::WorkerSessionState::Starting,
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::RuntimeStatusChanged {
                runtime_id: "rt".into(),
                card_id: worker.to_string(),
                old_status: calm_types::worker::WorkerSessionState::Starting,
                new_status: calm_types::worker::WorkerSessionState::Running,
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::RuntimeSuperseded {
                old_runtime_id: "rt-old".into(),
                new_runtime_id: "rt-new".into(),
                card_id: worker.to_string(),
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::HarnessItemAdded {
                runtime_id: "rt".into(),
                card_id: spec.clone(),
                wave_id: wave.clone(),
                item_db_id: 1,
                item_uuid: None,
                item_type: None,
                turn_id: None,
                method: "item/agent_message".into(),
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::HarnessPhaseChanged {
                runtime_id: "rt".into(),
                card_id: spec.clone(),
                wave_id: wave.clone(),
                old_phase: calm_types::harness::HarnessPhaseTag::Idle,
                new_phase: calm_types::harness::HarnessPhaseTag::TurnRunning,
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::HarnessTranscriptCleared {
                runtime_id: "rt".into(),
                card_id: spec.clone(),
                wave_id: wave.clone(),
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::HarnessUserMessageEnqueued {
                runtime_id: "rt".into(),
                card_id: spec.clone(),
                wave_id: wave.clone(),
                char_count: 5,
            },
            ActorId::User,
            false,
            false,
        ),
        row(
            Event::OverlaySet(crate::model::Overlay {
                id: "o".into(),
                plugin_id: "p".into(),
                entity_kind: "card".into(),
                entity_id: worker.to_string(),
                kind: "status".into(),
                payload: serde_json::Value::Null,
                updated_at: 1,
            }),
            ActorId::Plugin("p".into()),
            false,
            false,
        ),
        row(
            Event::OverlayDeleted {
                plugin_id: "p".into(),
                entity_kind: "card".into(),
                entity_id: worker.to_string(),
                kind: "status".into(),
            },
            ActorId::Plugin("p".into()),
            false,
            false,
        ),
        row(
            Event::TerminalDeleted {
                id: "term".into(),
                card_id: worker.clone(),
            },
            ActorId::Kernel,
            false,
            false,
        ),
        row(
            Event::PluginState {
                id: "p".into(),
                state: "running".into(),
                last_error: None,
            },
            ActorId::Kernel,
            false,
            false,
        ),
        row(
            Event::PluginToolRegistered {
                plugin_id: "p".into(),
                tool_name: "t".into(),
            },
            ActorId::Kernel,
            false,
            false,
        ),
        row(
            Event::WorkflowRegistered {
                plugin_id: "p".into(),
                workflow_id: "wf".into(),
            },
            ActorId::Kernel,
            false,
            false,
        ),
        row(
            Event::CodexWorkerRequested {
                idempotency_key: "k".into(),
                goal: "g".into(),
                context: serde_json::Value::Null,
                acceptance_criteria: None,
                agent_message: None,
            },
            ActorId::AiSpec(spec.clone()),
            false,
            false,
        ),
        row(
            Event::TerminalWorkerRequested {
                idempotency_key: "k".into(),
                cmd: "ls".into(),
                cwd: None,
                agent_message: None,
            },
            ActorId::AiSpec(spec.clone()),
            false,
            false,
        ),
        row(
            Event::PlanUpdated {
                wave_id: wave.clone(),
                changed_keys: vec!["k".into()],
                agent_message: None,
            },
            ActorId::AiSpec(spec.clone()),
            false,
            false,
        ),
        row(
            Event::TaskDispatched {
                idempotency_key: "w:k".into(),
                kind: "codex".into(),
                agent_message: None,
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::ForgePrDiffRead {
                wave_id: wave.clone(),
                pr_number: 1,
                base_sha: "base-sha".into(),
                head_sha: "head-sha".into(),
                artifact_path: "/tmp/diff.patch".into(),
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::ForgeIssueRead {
                wave_id: wave.clone(),
                issue_number: 1,
                artifact_path: "/tmp/issue.md".into(),
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::WorktreeRemoved {
                wave_id: wave.clone(),
                card_id: worker.clone(),
                path: "/tmp/worktree".into(),
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
    ];

    let mut covered = std::collections::BTreeSet::new();
    for row in &rows {
        let kind = row.event.kind_tag();
        covered.insert(kind.to_string());
        // Reject rows that would record predicate⇒mapping drift as
        // an expectation: expecting a push without an observation is
        // exactly the silent-wiring class #828 pins against.
        assert!(
            !row.expect_push || row.expect_observation,
            "row for {kind} violates predicate⇒mapping: expect_push without expect_observation"
        );
        assert_eq!(
            event_warrants_spec_push(&row.event, &row.actor, &write),
            row.expect_push,
            "push predicate mismatch for {kind} (actor {})",
            row.actor
        );
        assert_eq!(
            harness_observation_from_event(&wave, &row.event).is_some(),
            row.expect_observation,
            "observation mapping mismatch for {kind} (actor {})",
            row.actor
        );
    }

    // Completeness: every Event kind must have at least one row. A
    // new variant fails here (its serde tag joins the census
    // automatically) until its wiring expectation is recorded above.
    let all = all_event_kind_tags();
    let missing: Vec<_> = all.difference(&covered).collect();
    assert!(
        missing.is_empty(),
        "Event kinds without a consistency-table row (add one per kind): {missing:?}"
    );
    // And the reverse guards `kind_tag()` against drifting from the
    // serde rename set (rows can only name real kinds).
    let unknown_tags: Vec<_> = covered.difference(&all).collect();
    assert!(
        unknown_tags.is_empty(),
        "row kind_tag() values missing from the serde census: {unknown_tags:?}"
    );
}

/// #313 round-2 (B3) — the per-wave push lock map must serialize
/// concurrent acquisitions for the SAME wave (so boot takeover's
/// `Dispatcher::push_lock` and the live `push_to_spec`'s lock cannot run
/// the dedup-check-and-deliver body concurrently — which would lose
/// events in the seed→insert window). DIFFERENT waves must remain
/// independent so a slow takeover for wave A doesn't block live
/// pushes for wave B.
///
/// Models the `DashMap::entry(...).or_insert_with(Arc::new Mutex)` +
/// `clone().lock_owned().await` pattern `Inner::acquire_push_lock` uses.
#[tokio::test]
async fn per_wave_push_lock_serializes_same_wave_runs_in_parallel_across_waves() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Same map shape as `Inner::push_locks`.
    let push_locks: DashMap<WaveId, Arc<tokio::sync::Mutex<()>>> = DashMap::new();
    let take_lock = |wave_id: &WaveId| -> Arc<tokio::sync::Mutex<()>> {
        push_locks
            .entry(wave_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };

    // Track concurrent occupancy. Same-wave: must never exceed 1.
    let in_flight_a = Arc::new(AtomicUsize::new(0));
    let max_in_flight_a = Arc::new(AtomicUsize::new(0));
    let wave_a = WaveId::from("wave-a");

    let mut handles = vec![];
    for i in 0..8 {
        let lock = take_lock(&wave_a);
        let in_flight = in_flight_a.clone();
        let max_in_flight = max_in_flight_a.clone();
        handles.push(tokio::spawn(async move {
            let _g = lock.lock_owned().await;
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max_in_flight.fetch_max(now, Ordering::SeqCst);
            // Simulate the dedup-check-and-deliver body holding the
            // lock for a few yields (representative of `push_to_spec`'s
            // async work).
            tokio::task::yield_now().await;
            tokio::time::sleep(std::time::Duration::from_millis(2 * (i as u64 + 1))).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(
        max_in_flight_a.load(Ordering::SeqCst),
        1,
        "same-wave per-wave lock must serialize: observed concurrent holders"
    );

    // Different waves: independent locks → can run in parallel.
    let in_flight_total = Arc::new(AtomicUsize::new(0));
    let max_in_flight_total = Arc::new(AtomicUsize::new(0));
    let mut handles = vec![];
    for i in 0..6 {
        let wave: WaveId = format!("wave-parallel-{i}").into();
        let lock = take_lock(&wave);
        let in_flight = in_flight_total.clone();
        let max_in_flight = max_in_flight_total.clone();
        handles.push(tokio::spawn(async move {
            let _g = lock.lock_owned().await;
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max_in_flight.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    // We expect parallelism > 1 across distinct wave keys (otherwise
    // the per-wave keying is broken). With 6 spawns and ~15ms each on a
    // multi-threaded runtime they should overlap routinely.
    assert!(
        max_in_flight_total.load(Ordering::SeqCst) > 1,
        "different-wave locks must allow parallel runs; observed serialization"
    );
}
