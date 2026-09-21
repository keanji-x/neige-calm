use super::*;

#[test]
fn periodic_reconcile_sweeps_context_before_scheduler() {
    let source = include_str!("mod.rs");
    source
        .find("interval.tick().await;\n                tick_inner.reconcile_once().await;")
        .expect("the production periodic loop drives the shared reconcile body");
    let method_start = source
        .find("async fn reconcile_once(&self)")
        .expect("shared production reconcile body");
    let body = &source[method_start..];
    let context = body.find("self.context_monitor.sweep().await").unwrap();
    let scheduler = body.find("self.scheduler.sweep_all().await").unwrap();
    assert!(context < scheduler);
}
use calm_types::worker::WorkerSessionId;

#[test]
fn permits_from_env_fallback_paths() {
    let saved = std::env::var("NEIGE_DISPATCHER_PERMITS").ok();

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

    match saved {
        Some(v) => set("NEIGE_DISPATCHER_PERMITS", &v),
        None => remove("NEIGE_DISPATCHER_PERMITS"),
    }
}

use crate::card_role_cache::CardRoleCache;
use crate::event::{ArtifactRef, BroadcastEnvelope, EventScope};
use crate::ids::AreaId;
use calm_types::event::{ChannelVerdict, ChannelVerdictKind, RatifyDecision, ReviewSubject};
use calm_types::git_candidate::{DeliveryFailureCode, DeliverySettlement, DeliveryWakeReason};

/// A candidate-shape settlement for `task_id` on track `w`; `wake_reason` is the only field the
/// push predicate reads.
fn git_delivery_settled_event(task_id: &str, wake_reason: DeliveryWakeReason) -> Event {
    git_delivery_settled_event_on(&TrackId::from("w"), task_id, wake_reason)
}

/// Same settlement shape on an explicit track.
fn git_delivery_settled_event_on(
    track_id: &TrackId,
    task_id: &str,
    wake_reason: DeliveryWakeReason,
) -> Event {
    Event::TaskGitDeliverySettled {
        task_id: task_id.into(),
        idempotency_key: task_id.into(),
        track_id: track_id.clone(),
        card_id: CardId::from("worker"),
        delivery_id: "delivery-1".into(),
        ordinal: 1,
        result: DeliverySettlement::Candidate {
            candidate_id: "delivery-1".into(),
            commit_sha: "c".repeat(40),
            base_sha: "b".repeat(40),
            base_is_ancestor: true,
        },
        wake_reason,
    }
}

fn track_scope(track: &TrackId, area: &AreaId) -> EventScope {
    EventScope::Track {
        track: track.clone(),
        area: area.clone(),
    }
}

/// Built from the same `dispatcher_subscription_kinds()` the spawn site reads.
#[test]
fn dispatcher_filter_matches_push_kinds() {
    let filter = SubscribeFilter {
        scope: SubscribeScope::Any,
        include_descendants: true,
        kinds: Some(dispatcher_subscription_kinds()),
    };
    let track = TrackId::from("w");
    let area = AreaId::from("c");
    let scope = track_scope(&track, &area);

    let env = |ev: Event| BroadcastEnvelope {
        id: 1,
        event_version: 1,
        actor: ActorId::User,
        scope: scope.clone(),
        event: ev,
    };

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
    assert!(filter.matches(&env(Event::TaskCompleted {
        idempotency_key: "k".into(),
        result: serde_json::Value::Null,
        artifacts: Vec::<ArtifactRef>::new(),
        agent_message: None,
    })));
    assert!(filter.matches(&env(Event::TaskFailed {
        idempotency_key: "k".into(),
        reason: "boom".into(),
        details: None,
        agent_message: None,
    })));
    assert!(filter.matches(&env(Event::TaskExecutionSettled {
        task_id: "w:k".into(),
        operation_id: "op-exec".into(),
    })));
    assert!(filter.matches(&env(Event::TaskFilePublicationSettled {
        task_id: "w:k".into(),
        operation_id: "op-pub".into(),
    })));
    assert!(
        filter.matches(&env(Event::TaskCandidateVerificationSettled {
            task_id: "w:k".into(),
            operation_id: "op-cand".into(),
        }))
    );
    assert!(filter.matches(&env(git_delivery_settled_event(
        "w:k",
        DeliveryWakeReason::DeferredToGate
    ))));
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
    assert!(filter.matches(&env(Event::TrackReportEdited {
        track_id: track.clone(),
        card_id: CardId::from("card"),
        author: EditAuthor::User,
        author_plugin_id: None,
        edit_id: "e".into(),
        summary_before: String::new(),
        summary_after: String::new(),
        body_before: String::new(),
        body_after: String::new(),
        agent_message: None,
    })));
    assert!(!filter.matches(&env(Event::WorkspaceLeased {
        track_id: track.clone(),
        card_id: CardId::from("worker"),
        lease_id: "lease-1".into(),
        path: "/tmp/workspace".into(),
    })));
    assert!(!filter.matches(&env(Event::WorkspaceReleased {
        track_id: track.clone(),
        card_id: CardId::from("worker"),
        lease_id: "lease-1".into(),
    })));
    assert!(filter.matches(&env(Event::ForgeScanCompleted {
        track_id: track.clone(),
        overlapping_prs: vec![1, 2],
    })));
    assert!(filter.matches(&env(Event::ForgePrOpened {
        track_id: track.clone(),
        pr_number: 1,
        head_sha: "head-sha".into(),
    })));
    assert!(filter.matches(&env(Event::ForgePrChecks {
        track_id: track.clone(),
        pr_number: 1,
        conclusion: "success".into(),
    })));
    assert!(filter.matches(&env(Event::ForgeIssueClosed {
        track_id: track.clone(),
        issue_number: 1,
    })));
    assert!(!filter.matches(&env(Event::WorktreeProvisioned {
        track_id: track.clone(),
        card_id: CardId::from("worker"),
        path: "/tmp/worktree".into(),
    })));
    assert!(!filter.matches(&env(Event::WorktreeCommitted {
        track_id: track.clone(),
        card_id: CardId::from("worker"),
        commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
        branch: "neige/w/card".into(),
        delivery_id: None,
        base_is_ancestor: None,
    })));
    assert!(filter.matches(&env(Event::ForgePrMerged {
        track_id: track.clone(),
        subject: crate::event::ForgeMergeSubject {
            phase: "impl".into(),
            slice_id: "6".into(),
            pr_number: 1,
        },
        head_sha: "head-sha".into(),
        merge_sha: "merge-sha".into(),
    })));
    assert!(!filter.matches(&env(Event::ReviewRound {
        track_id: track.clone(),
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
        track_id: track.clone(),
        reason: "cap_exhausted".into(),
    })));
    assert!(filter.matches(&env(Event::RatifyResolved {
        track_id: track.clone(),
        decision: RatifyDecision::Grant,
    })));
    assert!(!filter.matches(&env(Event::ForgePrDiffRead {
        track_id: track.clone(),
        pr_number: 1,
        base_sha: "base-sha".into(),
        head_sha: "head-sha".into(),
        artifact_path: "/tmp/diff.patch".into(),
    })));
    assert!(!filter.matches(&env(Event::ForgeIssueRead {
        track_id: track.clone(),
        issue_number: 1,
        artifact_path: "/tmp/issue.md".into(),
    })));
    assert!(!filter.matches(&env(Event::WorktreeRemoved {
        track_id: track.clone(),
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
    assert!(filter.matches(&env(Event::PlanUpdated {
        track_id: track.clone(),
        changed_keys: vec!["impl-parser".into()],
        agent_message: None,
    })));
    assert!(filter.matches(&env(Event::TrackLifecycleChanged {
        id: track.clone(),
        area_id: area.clone(),
        from: crate::model::TrackLifecycle::Draft,
        to: crate::model::TrackLifecycle::Planning,
        agent_message: None,
    })));
    assert!(filter.matches(&env(Event::TrackUpdated(
        crate::event::TrackUpdatedPayload::new(
            crate::model::Track {
                id: track.clone(),
                area_id: area.clone(),
                title: "w".into(),
                sort: 0.0,
                archived_at: None,
                pinned_at: None,
                lifecycle: crate::model::TrackLifecycle::Working,
                cwd_wire_alias: String::new(),
                template_id: None,
                plugin_scope: None,
                purpose: None,
                template_input: None,
                terminal_at: None,
                recipe_id: None,
                recipe_revision: None,
                claude_permissions_policy: None,
                workspace: Default::default(),
                created_at: 1,
                updated_at: 1,
            },
            None,
        )
    ))));
    assert!(filter.matches(&env(Event::TrackDeleted {
        id: track.clone(),
        area_id: area.clone(),
    })));
    assert!(filter.matches(&env(Event::AreaDeleted { id: area.clone() })));
    // `task.dispatched` is emitted BY the scheduler inside its claim tx and deliberately NOT subscribed.
    assert!(!filter.matches(&env(Event::TaskDispatched {
        idempotency_key: "w:k".into(),
        kind: "codex".into(),
        agent_message: None,
    })));
    assert!(!filter.matches(&env(Event::CardDeleted {
        id: CardId::from("card"),
        track_id: track.clone(),
    })));
    assert!(!filter.matches(&env(Event::TerminalDeleted {
        id: "t".into(),
        card_id: CardId::from("card"),
    })));
}

/// Subscription == push-capable kinds ∪ scheduler kinds, disjoint; every subscribed kind lands
/// in a real `handle_envelope` arm and every warn-arm kind is unsubscribed.
#[tokio::test]
async fn dispatcher_subscription_is_push_kinds_plus_scheduler_kinds() {
    use std::collections::{BTreeMap, BTreeSet};
    let table = planner_push_wiring_table().await;
    let all = all_event_kind_tags();

    // (1) subscription set == push-capable set ∪ scheduler set, disjoint.
    let push_capable: BTreeSet<String> = table
        .rows
        .iter()
        .filter(|row| row.expect_push)
        .map(|row| row.event.kind_tag().to_string())
        .collect();
    let scheduler: BTreeSet<String> = SCHEDULER_TRIGGER_KINDS
        .iter()
        .map(|kind| kind.to_string())
        .collect();
    assert_eq!(
        scheduler.len(),
        SCHEDULER_TRIGGER_KINDS.len(),
        "SCHEDULER_TRIGGER_KINDS lists a kind twice"
    );
    assert!(
        scheduler.is_subset(&all),
        "SCHEDULER_TRIGGER_KINDS names kinds outside the serde census: {:?}",
        scheduler.difference(&all).collect::<Vec<_>>()
    );
    let overlap: Vec<_> = scheduler.intersection(&push_capable).collect();
    assert!(
        overlap.is_empty(),
        "a push-capable kind belongs in PLANNER_CATCH_UP_KINDS, not SCHEDULER_TRIGGER_KINDS: {overlap:?}"
    );
    let subscribed_list = dispatcher_subscription_kinds();
    let subscribed: BTreeSet<String> = subscribed_list.iter().cloned().collect();
    assert_eq!(
        subscribed.len(),
        subscribed_list.len(),
        "the subscription lists a kind twice: {subscribed_list:?}"
    );
    let expected: BTreeSet<String> = push_capable.union(&scheduler).cloned().collect();
    assert_eq!(
        subscribed,
        expected,
        "dispatcher subscription must be exactly push-capable ∪ scheduler kinds \
         (missing: {:?}, extra: {:?})",
        expected.difference(&subscribed).collect::<Vec<_>>(),
        subscribed.difference(&expected).collect::<Vec<_>>()
    );

    // (2) every subscribed kind has a non-warn arm in `handle_envelope`; the warn arm's pattern
    // is read from the source.
    let variant_of: BTreeMap<String, String> = table
        .rows
        .iter()
        .map(|row| {
            let debug = format!("{:?}", row.event);
            let variant = debug
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .next()
                .expect("Debug rendering starts with the variant name")
                .to_string();
            (row.event.kind_tag().to_string(), variant)
        })
        .collect();
    assert_eq!(
        variant_of.keys().cloned().collect::<BTreeSet<_>>(),
        all,
        "the wiring table must hold a sample event for every census kind"
    );
    let source = include_str!("mod.rs");
    let body = &source[source
        .find("async fn handle_envelope(self: Arc<Self>, envelope: BroadcastEnvelope)")
        .expect("handle_envelope in dispatcher/mod.rs")..];
    let warn_at = body
        .find("dispatcher received event with no handler; filter widened unexpectedly")
        .expect("the trailing warn arm of handle_envelope");
    let arm_open = body[..warn_at]
        .rfind("=> {")
        .expect("the warn arm's `=> {`");
    // The warn arm's pattern: the run of `Event::… |` / comment lines that
    // immediately precedes its `=> {`.
    let mut pattern_lines: Vec<&str> = Vec::new();
    for line in body[..arm_open].lines().rev() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("Event::") || trimmed.starts_with('|') || trimmed.starts_with("//") {
            pattern_lines.push(trimmed);
        } else {
            break;
        }
    }
    let warn_variants: BTreeSet<String> = pattern_lines
        .iter()
        .filter(|line| !line.starts_with("//"))
        .flat_map(|line| {
            line.split("Event::").skip(1).map(|rest| {
                rest.trim_start()
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
        })
        .filter(|variant| !variant.is_empty())
        .collect();
    assert!(
        warn_variants.len() >= 20,
        "warn-arm pattern parse degenerate: {warn_variants:?}"
    );
    let all_variants: BTreeSet<String> = variant_of.values().cloned().collect();
    assert!(
        warn_variants.is_subset(&all_variants),
        "warn arm names variants the census does not know: {:?}",
        warn_variants.difference(&all_variants).collect::<Vec<_>>()
    );
    let subscribed_variants: BTreeSet<String> = subscribed
        .iter()
        .map(|kind| variant_of[kind].clone())
        .collect();
    let subscribed_but_warn: Vec<_> = subscribed_variants.intersection(&warn_variants).collect();
    assert!(
        subscribed_but_warn.is_empty(),
        "subscribed kinds that fall into handle_envelope's warn arm (no handler): {subscribed_but_warn:?}"
    );
    let handled: BTreeSet<String> = all_variants.difference(&warn_variants).cloned().collect();
    assert_eq!(
        handled,
        subscribed_variants,
        "handle_envelope's non-warn arms must be exactly the subscribed kinds \
         (handled but unsubscribed: {:?}, subscribed but unhandled: {:?})",
        handled.difference(&subscribed_variants).collect::<Vec<_>>(),
        subscribed_variants.difference(&handled).collect::<Vec<_>>()
    );
}

#[test]
fn track_report_edited_author_gating() {
    assert!(EditAuthor::User == EditAuthor::User);
    assert!(EditAuthor::Planner != EditAuthor::User);
    assert!(EditAuthor::Kernel != EditAuthor::User);
}

#[tokio::test]
async fn gated_self_report_predicate() {
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mk_task = |key: &str, gate: Option<String>| crate::model::Task {
        id: format!("w:{key}"),
        track_id: "w".into(),
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
        context_stale_at_ms: None,
        declared_by: calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR.into(),
        spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
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
    // Production writes the classifier PLUS a reason tail; the pre-gate classification must survive it.
    let mut gated_spawn_failed_reason = mk_task("gated-spawn-failed-reason", gate_json());
    gated_spawn_failed_reason.status = crate::model::TaskStatus::Failed;
    gated_spawn_failed_reason.status_detail = Some(crate::db::sqlite::status_detail_with_reason(
        "spawn-failed",
        "track w cwd /home/kenji is not a git repository: fatal: not a git repository",
    ));
    // A gate detail carrying a reason tail stays suppressed: the vocabulary lives in the class.
    let mut gated_gate_failed_reason = mk_task("gated-gate-failed-reason", gate_json());
    gated_gate_failed_reason.status = crate::model::TaskStatus::Failed;
    gated_gate_failed_reason.status_detail = Some(crate::db::sqlite::status_detail_with_reason(
        "gate-red",
        "step `worker-reported` exited 1",
    ));
    let mut gated_worker_timeout = mk_task("gated-worker-timeout", gate_json());
    gated_worker_timeout.status = crate::model::TaskStatus::Failed;
    gated_worker_timeout.status_detail = Some("worker-timeout".into());
    // Gated row the gate already failed — a late worker
    // `task.failed` retry must not re-wake the planner.
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
                &gated_spawn_failed_reason,
                &gated_gate_failed_reason,
                &gated_worker_timeout,
                &gated_gate_failed,
                &gated_done,
                &ungated_failed,
            ] {
                crate::test_support::insert_task_tx(tx, t).await?;
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
        details: None,
        agent_message: None,
    };
    assert!(is_deferred_self_report(&repo, &completed("gated")).await);
    assert!(!is_deferred_self_report(&repo, &completed("ungated")).await);
    assert!(
        !is_deferred_self_report(&repo, &completed("legacy-no-row")).await,
        "legacy keys with no tasks row push as today"
    );
    assert!(
        is_deferred_self_report(&repo, &failed("gated")).await,
        "stale task.failed while the gate is in flight (`verifying`) is suppressed"
    );
    assert!(
        is_deferred_self_report(&repo, &failed("gated-gate-failed")).await,
        "late task.failed after the gate already failed the row is suppressed"
    );
    assert!(
        is_deferred_self_report(&repo, &failed("gated-done")).await,
        "late task.failed after the gate already passed the row is suppressed"
    );
    assert!(
        !is_deferred_self_report(&repo, &failed("gated-worker-failed")).await,
        "a genuine pre-gate worker failure pushes as today (no gate runs on failure)"
    );
    assert!(
        !is_deferred_self_report(&repo, &failed("gated-spawn-failed")).await,
        "a spawn failure pushes as today (no gate runs on failure)"
    );
    assert!(
        !is_deferred_self_report(&repo, &failed("gated-spawn-failed-reason")).await,
        "#1147 ①: the reason tail must not hide the `spawn-failed` classifier"
    );
    assert!(
        is_deferred_self_report(&repo, &failed("gated-gate-failed-reason")).await,
        "#1147 ①: a pre-gate word inside a `gate-*` reason tail must not \
         reclassify the row as a pre-gate failure"
    );
    assert!(
        !is_deferred_self_report(&repo, &failed("gated-worker-timeout")).await,
        "a worker liveness timeout pushes as a pre-gate failure"
    );
    assert!(
        !is_deferred_self_report(&repo, &failed("ungated-failed")).await,
        "ungated failures keep today's behavior"
    );
    assert!(
        !is_deferred_self_report(&repo, &failed("legacy-no-row")).await,
        "legacy task.failed keys with no tasks row push as today"
    );
    assert!(
        !is_deferred_self_report(
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

/// Lookup errors are produced the way production produces them: two rows claiming the same
/// worker card make `task_for_worker_card` return `Conflict`.
#[tokio::test]
async fn stale_worker_stop_hook_consultation_per_task_status() {
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mk_task = |key: &str, card: &str, status: crate::model::TaskStatus| crate::model::Task {
        id: format!("w:{key}"),
        track_id: "w".into(),
        key: key.into(),
        kind: crate::model::TaskKind::Codex,
        goal: "g".into(),
        context_json: "null".into(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: "[]".into(),
        priority: 0,
        gate_json: None,
        status,
        status_detail: None,
        worker_card_id: Some(card.into()),
        gate_result_json: None,
        gate_attempt: 0,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR.into(),
        spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
        created_at_ms: 1,
        updated_at_ms: 1,
        finished_at_ms: None,
    };
    use crate::model::TaskStatus;
    let seeded = vec![
        mk_task("dispatched", "card-dispatched", TaskStatus::Dispatched),
        mk_task("running", "card-running", TaskStatus::Running),
        mk_task("verifying", "card-verifying", TaskStatus::Verifying),
        mk_task("done", "card-done", TaskStatus::Done),
        mk_task("failed", "card-failed", TaskStatus::Failed),
        mk_task("canceled", "card-canceled", TaskStatus::Canceled),
        // Two rows on one card: the lookup errors with `Conflict`.
        mk_task("ambiguous-a", "card-ambiguous", TaskStatus::Verifying),
        mk_task("ambiguous-b", "card-ambiguous", TaskStatus::Verifying),
    ];
    crate::db::write_in_tx_typed(&repo, move |tx| {
        Box::pin(async move {
            for t in &seeded {
                crate::test_support::insert_task_tx(tx, t).await?;
            }
            Ok(())
        })
    })
    .await
    .expect("seed tasks");
    assert!(
        calm_truth::db::RepoRead::task_for_worker_card(&repo, "card-ambiguous")
            .await
            .is_err(),
        "fixture: the ambiguous card must make the lookup fail"
    );

    let codex_stop = |card: &str| Event::CodexHook {
        card_id: CardId::from(card),
        kind: "hook.codex.stop".into(),
        hook_idempotency_key: format!("hook-codex-stop-{card}"),
        payload: serde_json::Value::Null,
    };
    let claude_stop = |card: &str| Event::ClaudeHook {
        card_id: CardId::from(card),
        kind: "hook.claude.stop".into(),
        hook_idempotency_key: format!("hook-claude-stop-{card}"),
        payload: serde_json::Value::Null,
    };
    // (card, expect_suppressed, why)
    let table: &[(&str, bool, &str)] = &[
        (
            "card-dispatched",
            false,
            "dispatched row still needs the wake",
        ),
        ("card-running", false, "running row still needs the wake"),
        ("card-verifying", true, "the gate result is the wake"),
        (
            "card-done",
            true,
            "terminal row: the task terminal event was the wake",
        ),
        (
            "card-failed",
            true,
            "terminal row: the task terminal event was the wake",
        ),
        (
            "card-canceled",
            true,
            "terminal row: nothing left to wake for",
        ),
        (
            "card-no-row",
            false,
            "no tasks row (ungated / legacy card) pushes as today",
        ),
        ("card-ambiguous", false, "lookup error pushes (fail-open)"),
    ];
    for (card, expect_suppressed, why) in table {
        for event in [codex_stop(card), claude_stop(card)] {
            assert_eq!(
                is_stale_worker_stop_hook(&repo, &event).await,
                *expect_suppressed,
                "{card} ({}): {why}",
                event.kind_tag()
            );
        }
    }
    // Non-hook events are never this consultation's business.
    assert!(
        !is_stale_worker_stop_hook(
            &repo,
            &Event::TaskCompleted {
                idempotency_key: "w:verifying".into(),
                result: serde_json::Value::Null,
                artifacts: Vec::new(),
                agent_message: None,
            }
        )
        .await
    );
}

/// Both live and boot paths resolve opaque execution IDs through the same reader.
#[tokio::test]
async fn task_recovery_gate_observation_resolves_opaque_execution_identity() {
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    crate::db::write_in_tx_typed(&repo, |tx| Box::pin(async move {
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,priority,status,created_at_ms,updated_at_ms) VALUES('attempt-opaque','w','impl-parser','codex','g','{}','[]',0,'failed',1,1)")
            .execute(&mut **tx).await?;
        Ok(())
    })).await.unwrap();
    let event = Event::TaskGateResult {
        task_id: "attempt-opaque".into(),
        idempotency_key: "attempt-opaque".into(),
        passed: false,
        failing_step: None,
        exit_code: Some(1),
        log_tail: String::new(),
        log_path: "/tmp/gate.log".into(),
        attempt: 1,
        agent_message: None,
    };
    let observation = resolve_harness_observation(&repo, &TrackId::from("w"), &event)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(observation, HarnessObservation::TaskGateResult { key, idempotency_key, .. }
        if key == "impl-parser" && idempotency_key == "attempt-opaque")
    );
    assert!(
        resolve_harness_observation(&repo, &TrackId::from("foreign"), &event)
            .await
            .is_err()
    );
}

#[test]
fn gate_result_maps_to_hard_fire_observation_with_plan_key() {
    let track = TrackId::from("track-1");
    let event = Event::TaskGateResult {
        task_id: "track-1:impl-parser".into(),
        idempotency_key: "track-1:impl-parser".into(),
        passed: false,
        failing_step: Some("test".into()),
        exit_code: Some(101),
        log_tail: "boom".into(),
        log_path: "/tmp/gate-logs/track-1:impl-parser-g2.log".into(),
        attempt: 2,
        agent_message: None,
    };
    let obs = harness_observation_from_event(&track, &event, Some("impl-parser"))
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
            assert_eq!(idempotency_key, "track-1:impl-parser");
            assert_eq!(
                key, "impl-parser",
                "plan key is resolved separately from execution identity"
            );
            assert!(!passed);
            assert_eq!(failing_step.as_deref(), Some("test"));
            assert_eq!(*exit_code, Some(101));
            assert_eq!(*attempt, 2);
        }
        other => panic!("expected TaskGateResult observation, got {other:?}"),
    }
    let text = obs.to_turn_text();
    assert!(text.contains("Task impl-parser gate FAILED at step test (exit 101)"));
    assert!(text.contains("runs/track-1:impl-parser/gates/2.log"));
    assert!(text.contains("runs/track-1:impl-parser.md"));
}

#[test]
fn event_warrants_planner_push_covers_push_allowlist() {
    let cache = CardRoleCache::new();
    let track = TrackId::from("w");
    let area = AreaId::from("c");
    let worker = CardId::from("worker");
    let planner = CardId::from("planner");
    let unknown = CardId::from("unknown");
    cache.insert(worker.clone(), CardRole::Worker, track.clone());
    cache.insert(planner.clone(), CardRole::Planner, track.clone());
    let write = WriteContext::new(cache, crate::track_area_cache::TrackAreaCache::new());

    let completed = Event::TaskCompleted {
        idempotency_key: "done".into(),
        result: serde_json::Value::Null,
        artifacts: Vec::new(),
        agent_message: None,
    };
    assert!(event_warrants_planner_push(
        &completed,
        &ActorId::AiCodex(worker.clone()),
        &write
    ));
    assert!(!event_warrants_planner_push(
        &completed,
        &ActorId::AiPlanner(planner.clone()),
        &write
    ));

    let failed = Event::TaskFailed {
        idempotency_key: "fail".into(),
        reason: "boom".into(),
        details: None,
        agent_message: None,
    };
    assert!(event_warrants_planner_push(
        &failed,
        &ActorId::AiCodex(worker.clone()),
        &write
    ));
    assert!(!event_warrants_planner_push(
        &failed,
        &ActorId::AiPlanner(planner.clone()),
        &write
    ));

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
    assert!(event_warrants_planner_push(
        &gate_result,
        &ActorId::KernelDispatcher,
        &write
    ));

    let report = |author| Event::TrackReportEdited {
        track_id: track.clone(),
        card_id: planner.clone(),
        author,
        author_plugin_id: None,
        edit_id: "edit".into(),
        summary_before: String::new(),
        summary_after: String::new(),
        body_before: String::new(),
        body_after: String::new(),
        agent_message: None,
    };
    assert!(event_warrants_planner_push(
        &report(EditAuthor::User),
        &ActorId::User,
        &write
    ));
    assert!(event_warrants_planner_push(
        &report(EditAuthor::Plugin),
        &ActorId::Kernel,
        &write
    ));
    assert!(event_warrants_planner_push(
        &report(EditAuthor::Assistant),
        &ActorId::AiCodex(worker.clone()),
        &write
    ));
    assert!(!event_warrants_planner_push(
        &report(EditAuthor::Planner),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_planner_push(
        &report(EditAuthor::Kernel),
        &ActorId::User,
        &write
    ));

    for quiet_event in [
        Event::WorkspaceLeased {
            track_id: track.clone(),
            card_id: worker.clone(),
            lease_id: "lease".into(),
            path: "/tmp/ws".into(),
        },
        Event::WorkspaceReleased {
            track_id: track.clone(),
            card_id: worker.clone(),
            lease_id: "lease".into(),
        },
        Event::ReviewRound {
            track_id: track.clone(),
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
        Event::WorktreeProvisioned {
            track_id: track.clone(),
            card_id: worker.clone(),
            path: "/tmp/worktree".into(),
        },
        Event::WorktreeCommitted {
            track_id: track.clone(),
            card_id: worker.clone(),
            commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            branch: "neige/w/card".into(),
            delivery_id: None,
            base_is_ancestor: None,
        },
    ] {
        for actor in [
            ActorId::KernelDispatcher,
            ActorId::Kernel,
            ActorId::AiPlanner(planner.clone()),
            ActorId::User,
        ] {
            assert!(
                !event_warrants_planner_push(&quiet_event, &actor, &write),
                "#1727 S1: {} must not wake the planner (actor {actor:?})",
                quiet_event.kind_tag()
            );
        }
    }

    for forge_event in [
        Event::ForgePrMerged {
            track_id: track.clone(),
            subject: crate::event::ForgeMergeSubject {
                phase: "impl".into(),
                slice_id: "6".into(),
                pr_number: 1,
            },
            head_sha: "head-sha".into(),
            merge_sha: "merge-sha".into(),
        },
        Event::RatifyRequested {
            track_id: track.clone(),
            reason: "cap_exhausted".into(),
        },
        Event::RatifyResolved {
            track_id: track.clone(),
            decision: RatifyDecision::Grant,
        },
        Event::ForgeScanCompleted {
            track_id: track.clone(),
            overlapping_prs: vec![1, 2],
        },
        Event::ForgePrOpened {
            track_id: track.clone(),
            pr_number: 1,
            head_sha: "head-sha".into(),
        },
        Event::ForgePrChecks {
            track_id: track.clone(),
            pr_number: 1,
            conclusion: "success".into(),
        },
        Event::ForgeIssueClosed {
            track_id: track.clone(),
            issue_number: 1,
        },
    ] {
        assert!(
            event_warrants_planner_push(&forge_event, &ActorId::KernelDispatcher, &write),
            "{} must still wake the planner",
            forge_event.kind_tag()
        );
    }
    assert!(!event_warrants_planner_push(
        &Event::ForgePrDiffRead {
            track_id: track.clone(),
            pr_number: 1,
            base_sha: "base-sha".into(),
            head_sha: "head-sha".into(),
            artifact_path: "/tmp/diff.patch".into(),
        },
        &ActorId::KernelDispatcher,
        &write
    ));
    assert!(!event_warrants_planner_push(
        &Event::WorktreeRemoved {
            track_id: track.clone(),
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
    assert!(event_warrants_planner_push(
        &codex_hook(worker.clone(), "hook.codex.stop"),
        &ActorId::User,
        &write
    ));
    assert!(event_warrants_planner_push(
        &claude_hook(worker.clone(), "hook.claude.stop"),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_planner_push(
        &codex_hook(planner.clone(), "hook.codex.stop"),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_planner_push(
        &claude_hook(planner.clone(), "hook.claude.stop"),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_planner_push(
        &codex_hook(unknown.clone(), "hook.codex.stop"),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_planner_push(
        &claude_hook(unknown, "hook.claude.stop"),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_planner_push(
        &codex_hook(worker.clone(), "hook.codex.permission_request"),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_planner_push(
        &codex_hook(worker, "hook.codex.post_tool_use"),
        &ActorId::User,
        &write
    ));
    assert!(!event_warrants_planner_push(
        &Event::TrackDeleted {
            id: track,
            area_id: area,
        },
        &ActorId::User,
        &write
    ));
}

#[test]
fn event_warrants_planner_push_task_actor_matrix_and_request_kinds_pin() {
    let cache = CardRoleCache::new();
    let track = TrackId::from("w");
    let worker = CardId::from("worker");
    let planner = CardId::from("planner");
    cache.insert(worker.clone(), CardRole::Worker, track.clone());
    cache.insert(planner.clone(), CardRole::Planner, track.clone());
    let write = WriteContext::new(cache, crate::track_area_cache::TrackAreaCache::new());

    let completed = Event::TaskCompleted {
        idempotency_key: "done".into(),
        result: serde_json::Value::Null,
        artifacts: Vec::new(),
        agent_message: None,
    };
    let failed = Event::TaskFailed {
        idempotency_key: "fail".into(),
        reason: "boom".into(),
        details: None,
        agent_message: None,
    };
    // Every non-AiPlanner actor pushes — including the kernel dispatcher's spawn-failure
    // `task.failed` fallback.
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
            event_warrants_planner_push(&completed, &actor, &write),
            "task.completed must push for actor {actor}"
        );
        assert!(
            event_warrants_planner_push(&failed, &actor, &write),
            "task.failed must push for actor {actor}"
        );
    }
    for actor in [
        ActorId::AiPlanner(planner.clone()),
        ActorId::AiPlannerSession(WorkerSessionId::from("sess-planner")),
    ] {
        assert!(
            !event_warrants_planner_push(&completed, &actor, &write),
            "task.completed must not self-push for actor {actor}"
        );
        assert!(
            !event_warrants_planner_push(&failed, &actor, &write),
            "task.failed must not self-push for actor {actor}"
        );
    }

    // The two request kinds are dispatcher inputs, never planner pushes — for any actor.
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
        ActorId::AiPlanner(planner.clone()),
        ActorId::AiCodex(worker.clone()),
    ] {
        assert!(
            !event_warrants_planner_push(&codex_req, &actor, &write),
            "codex.worker_requested must never push for actor {actor}"
        );
        assert!(
            !event_warrants_planner_push(&terminal_req, &actor, &write),
            "terminal.worker_requested must never push for actor {actor}"
        );
    }
}

#[test]
fn harness_observation_from_event_mapping_pin() {
    let track = TrackId::from("track-map");
    let worker = CardId::from("card-map");

    // task.completed — idempotency key + verbatim result.
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::TaskCompleted {
                idempotency_key: "map-a".into(),
                result: serde_json::json!({"ok": true, "n": 7}),
                artifacts: vec![ArtifactRef::from("art-1")],
                agent_message: Some("ignored".into()),
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::TaskCompleted {
            idempotency_key: "map-a".into(),
            result: serde_json::json!({"ok": true, "n": 7}),
        })
    );

    // task.failed — the event's `reason` becomes the observation `error`.
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::TaskFailed {
                idempotency_key: "map-b".into(),
                reason: "boom".into(),
                details: None,
                agent_message: None,
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::TaskFailed {
            idempotency_key: "map-b".into(),
            error: "boom".into(),
        })
    );

    // track.report_edited — body_after verbatim + its sha256 (golden hex computed externally).
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::TrackReportEdited {
                track_id: track.clone(),
                card_id: worker.clone(),
                author: EditAuthor::User,
                author_plugin_id: None,
                edit_id: "e".into(),
                summary_before: String::new(),
                summary_after: "s".into(),
                body_before: "old".into(),
                body_after: "loop-pin-body".into(),
                agent_message: None,
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::ReportEdited {
            track_id: track.clone(),
            body_sha256: "09b37878497ec46015d1913ba0dff1cd051ca244859c80f4a3fc14d88a4a9465".into(),
            body: "loop-pin-body".into(),
            author: Some(EditAuthor::User),
            body_before: Some("old".into()),
            doc_rev_after: None,
            blocks_after: None,
        })
    );

    // workspace.* — lifecycle carrier events map through the payload
    // fields and use the caller-provided track id like track.report_edited.
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::WorkspaceLeased {
                track_id: TrackId::from("payload-track-ignored"),
                card_id: worker.clone(),
                lease_id: "lease-map".into(),
                path: "/tmp/workspace-map".into(),
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::WorkspaceLeased {
            track_id: track.clone(),
            card_id: worker.clone(),
            lease_id: "lease-map".into(),
            path: "/tmp/workspace-map".into(),
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::WorkspaceReleased {
                track_id: TrackId::from("payload-track-ignored"),
                card_id: worker.clone(),
                lease_id: "lease-map".into(),
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::WorkspaceReleased {
            track_id: track.clone(),
            card_id: worker.clone(),
            lease_id: "lease-map".into(),
        })
    );

    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::ForgePrMerged {
                track_id: TrackId::from("payload-track-ignored"),
                subject: crate::event::ForgeMergeSubject {
                    phase: "impl".into(),
                    slice_id: "6".into(),
                    pr_number: 760,
                },
                head_sha: "head-sha".into(),
                merge_sha: "merge-sha".into(),
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::ForgePrMerged {
            track_id: track.clone(),
            pr_number: 760,
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::ReviewRound {
                track_id: TrackId::from("payload-track-ignored"),
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
                idempotency_key: "review.round:track-map:impl:5b:760:1".into(),
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::ReviewRound {
            track_id: track.clone(),
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
            &track,
            &Event::RatifyRequested {
                track_id: TrackId::from("payload-track-ignored"),
                reason: "cap_exhausted".into(),
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::RatifyRequested {
            track_id: track.clone(),
            reason: "cap_exhausted".into(),
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::RatifyResolved {
                track_id: TrackId::from("payload-track-ignored"),
                decision: RatifyDecision::Deny,
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::RatifyResolved {
            track_id: track.clone(),
            decision: RatifyDecision::Deny,
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::ForgeScanCompleted {
                track_id: TrackId::from("payload-track-ignored"),
                overlapping_prs: vec![1, 2],
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::ForgeScanCompleted {
            track_id: track.clone(),
            overlapping_prs: vec![1, 2],
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::ForgePrOpened {
                track_id: TrackId::from("payload-track-ignored"),
                pr_number: 1,
                head_sha: "head-sha".into(),
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::ForgePrOpened {
            track_id: track.clone(),
            pr_number: 1,
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::ForgePrChecks {
                track_id: TrackId::from("payload-track-ignored"),
                pr_number: 1,
                conclusion: "success".into(),
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::ForgePrChecks {
            track_id: track.clone(),
            pr_number: 1,
            conclusion: "success".into(),
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::ForgeIssueClosed {
                track_id: TrackId::from("payload-track-ignored"),
                issue_number: 760,
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::ForgeIssueClosed {
            track_id: track.clone(),
            issue_number: 760,
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::WorktreeProvisioned {
                track_id: TrackId::from("payload-track-ignored"),
                card_id: worker.clone(),
                path: "/tmp/worktree-map".into(),
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::WorktreeProvisioned {
            track_id: track.clone(),
            card_id: worker.clone(),
            path: "/tmp/worktree-map".into(),
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::WorktreeCommitted {
                track_id: TrackId::from("payload-track-ignored"),
                card_id: worker.clone(),
                commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
                branch: "neige/w/card".into(),
                delivery_id: None,
                base_is_ancestor: None,
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::WorktreeCommitted {
            track_id: track.clone(),
            card_id: worker.clone(),
            commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            branch: "neige/w/card".into(),
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::ForgePrDiffRead {
                track_id: TrackId::from("payload-track-ignored"),
                pr_number: 1,
                base_sha: "base-sha".into(),
                head_sha: "head-sha".into(),
                artifact_path: "/tmp/diff.patch".into(),
            },
            Some("impl-parser")
        ),
        None
    );
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::WorktreeRemoved {
                track_id: TrackId::from("payload-track-ignored"),
                card_id: worker.clone(),
                path: "/tmp/worktree-map".into(),
            },
            Some("impl-parser")
        ),
        None
    );

    // Stop hooks — exact kind discriminators map to WorkerHookStop.
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::CodexHook {
                card_id: worker.clone(),
                kind: "hook.codex.stop".into(),
                hook_idempotency_key: "hook-c".into(),
                payload: serde_json::Value::Null,
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::WorkerHookStop {
            track_id: track.clone(),
            card_id: worker.clone(),
            kind: HarnessHookKind::CodexStop,
            idempotency_key: "hook-c".into(),
        })
    );
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::ClaudeHook {
                card_id: worker.clone(),
                kind: "hook.claude.stop".into(),
                hook_idempotency_key: "hook-l".into(),
                payload: serde_json::Value::Null,
            },
            Some("impl-parser")
        ),
        Some(HarnessObservation::WorkerHookStop {
            track_id: track.clone(),
            card_id: worker.clone(),
            kind: HarnessHookKind::ClaudeStop,
            idempotency_key: "hook-l".into(),
        })
    );

    // Non-stop hooks and non-push kinds map to nothing.
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::CodexHook {
                card_id: worker.clone(),
                kind: "hook.codex.permission_request".into(),
                hook_idempotency_key: "hook-p".into(),
                payload: serde_json::Value::Null,
            },
            Some("impl-parser")
        ),
        None
    );
    assert_eq!(
        harness_observation_from_event(
            &track,
            &Event::CodexWorkerRequested {
                idempotency_key: "k".into(),
                goal: "g".into(),
                context: serde_json::Value::Null,
                acceptance_criteria: None,
                agent_message: None,
            },
            Some("impl-parser")
        ),
        None
    );
}

/// `expect_push` and `expect_observation` are separate fields because the invariant is
/// one-directional — predicate ⇒ mapping.
struct PlannerPushWiringRow {
    event: Event,
    actor: ActorId,
    expect_push: bool,
    expect_observation: bool,
}

/// Retained failed-publication facts for the repo-enriched mapping row. A failed
/// admission needs no captured-file receipt or live worker/provider process.
async fn planner_push_publication_fixture() -> (crate::db::sqlite::SqlxRepo, Event) {
    use crate::operation::{OperationKey, OperationRepo, PhaseTag, SqlxOperationRepo};
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    sqlx::raw_sql("INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('c','Area','red',0,1,1);
        INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('w','c','Track',0,1,1);")
        .execute(repo.pool()).await.unwrap();
    let task_id = "publication-source-attempt";
    let context = serde_json::json!({"neige_execution":{"version":"isolated-codex-v1",
        "workspace":"empty","file_delivery":{"role":"producer","slot":"result",
        "path":"result.json","policy":"json-document-v1"}}});
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,created_at_ms,updated_at_ms) VALUES(?1,'w','produce','codex','Write JSON',?2,'done',1,1)")
        .bind(task_id).bind(context.to_string()).execute(repo.pool()).await.unwrap();
    let operations = SqlxOperationRepo::new(repo.pool().clone());
    let payload =
        serde_json::json!({"task_id":task_id,"track_id":"w","source_operation_id":"source-op"});
    let operation_id = operations
        .insert_operation(
            crate::file_delivery::OPERATION_KIND,
            OperationKey {
                operation_key: "publication-wiring".into(),
                idempotency_key: Some(format!("file:{task_id}")),
                payload_hash: crate::routes::terminal_cards::stable_payload_hash(&payload).unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    let claimed = operations.claim_drive_batch(1).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, operation_id);
    operations
        .mark_failed(
            &claimed[0],
            "file producer has no accepted completion report".into(),
            PhaseTag::Pending,
            Some("internal".into()),
        )
        .await
        .unwrap()
        .expect("owned publication settles");
    (
        repo,
        Event::TaskFilePublicationSettled {
            task_id: task_id.into(),
            operation_id,
        },
    )
}

/// Failed verification has retained candidate identity, but no qualifying evidence.
async fn planner_push_candidate_fixture() -> (crate::db::sqlite::SqlxRepo, Event) {
    use crate::operation::{OperationKey, OperationRepo, PhaseTag, SqlxOperationRepo};
    let (
        repo,
        Event::TaskFilePublicationSettled {
            task_id,
            operation_id: publication,
        },
    ) = planner_push_publication_fixture().await
    else {
        unreachable!()
    };
    let contract = serde_json::json!({"role":"candidate_producer","slot":"project","paths":["README.md"],"policy":{"scope":"declared-checks-only","timeout_secs":1,"steps":[{"name":"check","cmd":"true"}]}});
    sqlx::query("UPDATE tasks SET context_json=json_set(context_json,'$.neige_execution.file_delivery',json(?1)) WHERE id=?2").bind(contract.to_string()).bind(&task_id).execute(repo.pool()).await.unwrap();
    let candidate = serde_json::json!({"publication_operation_id":publication,"source":{"task_id":task_id,"track_id":"w","source_operation_id":"source-op"},"contract":contract,"snapshot":"0".repeat(64),"store_root":"/unused-candidate-wiring"});
    sqlx::query("INSERT INTO task_file_candidates(operation_id,track_id,producer_attempt_id,slot,candidate_json) VALUES(?1,'w',?2,'project',?3)").bind(&publication).bind(&task_id).bind(candidate.to_string()).execute(repo.pool()).await.unwrap();
    let operations = SqlxOperationRepo::new(repo.pool().clone());
    let payload = serde_json::json!({"publication_operation_id":publication});
    let operation_id = operations
        .insert_operation(
            "candidate-verify",
            OperationKey {
                operation_key: "candidate-wiring".into(),
                idempotency_key: Some(format!("candidate:{publication}")),
                payload_hash: crate::routes::terminal_cards::stable_payload_hash(&payload).unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    let claimed = operations.claim_drive_batch(1).await.unwrap();
    operations
        .mark_failed(
            &claimed[0],
            "candidate authority withdrawn".into(),
            PhaseTag::Pending,
            Some("conflict".into()),
        )
        .await
        .unwrap();
    (
        repo,
        Event::TaskCandidateVerificationSettled {
            task_id,
            operation_id,
        },
    )
}

/// A tasks row for the settled attempt whose worker card holds a lease, so the mapping can name
/// both the plan key and the retained worktree path.
async fn planner_push_delivery_fixture() -> (crate::db::sqlite::SqlxRepo, Event) {
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    sqlx::raw_sql("INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('c','Area','red',0,1,1);
        INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('w','c','Track',0,1,1);")
        .execute(repo.pool()).await.unwrap();
    let task_id = "delivery-source-attempt";
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,priority,status,\
         worker_card_id,created_at_ms,updated_at_ms) \
         VALUES(?1,'w','deliver','codex','Deliver','{}','[]',0,'done','worker',1,1)",
    )
    .bind(task_id)
    .execute(repo.pool())
    .await
    .unwrap();
    let lease_dir = tempfile::tempdir().unwrap();
    let lease_path = lease_dir.path().join("w").join("worker");
    let mut tx = crate::db::sqlite::begin_immediate_tx(repo.pool())
        .await
        .unwrap();
    crate::operation::workspace_lease::acquire_plain_workspace_lease_tx(
        &mut tx,
        "worker",
        "w",
        "delivery-wiring",
        &lease_path,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // The mapping reads the row's path, not the directory, so the tempdir may go away here.
    drop(lease_dir);
    (
        repo,
        git_delivery_settled_event(task_id, DeliveryWakeReason::UngatedCandidate),
    )
}

/// Census of every `Event` kind tag, derived from serde's unknown-variant diagnostic so it
/// cannot drift from the enum.
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
    // The diagnostic also lists `#[serde(alias)]` spellings, which `kind_tag()` never emits;
    // strip the known ones.
    for alias in ["codex.job_requested", "terminal.job_requested"] {
        assert!(
            tags.remove(alias),
            "stale alias {alias:?} no longer in the deserializer; drop it from this list"
        );
    }
    assert!(
        tags.contains("area.updated") && tags.contains("task.completed") && tags.len() >= 46,
        "kind census parse failed; raw diagnostic: {msg}"
    );
    tags
}

/// Shared with `planner_catch_up_kinds_equal_the_push_capable_kinds` so the catch-up list is
/// pinned against the same rows.
struct PlannerPushWiringTable {
    write: WriteContext,
    track: TrackId,
    rows: Vec<PlannerPushWiringRow>,
    publication_repo: crate::db::sqlite::SqlxRepo,
    candidate_repo: crate::db::sqlite::SqlxRepo,
    delivery_repo: crate::db::sqlite::SqlxRepo,
}

async fn planner_push_wiring_table() -> PlannerPushWiringTable {
    let cache = CardRoleCache::new();
    let track = TrackId::from("w");
    let area = AreaId::from("c");
    let worker = CardId::from("worker");
    let planner = CardId::from("planner");
    let unknown = CardId::from("unknown");
    cache.insert(worker.clone(), CardRole::Worker, track.clone());
    cache.insert(planner.clone(), CardRole::Planner, track.clone());
    let write = WriteContext::new(cache, crate::track_area_cache::TrackAreaCache::new());

    let row = |event: Event, actor: ActorId, expect_push: bool, expect_observation: bool| {
        PlannerPushWiringRow {
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
    let report_edited = |author: EditAuthor| Event::TrackReportEdited {
        track_id: track.clone(),
        card_id: planner.clone(),
        author,
        author_plugin_id: None,
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
        details: None,
        agent_message: None,
    };
    let card_sample = || crate::model::Card {
        id: worker.clone(),
        track_id: track.clone(),
        title: None,
        kind: "terminal".into(),
        sort: 0.0,
        payload: serde_json::Value::Null,
        runtime: None,
        deletable: true,
        created_at: 1,
        updated_at: 1,
    };

    let mut rows: Vec<PlannerPushWiringRow> = vec![
        // Push-capable kinds, push-side rows: predicate true ⇒ mapping Some.
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
            report_edited(EditAuthor::Plugin),
            ActorId::Kernel,
            true,
            true,
        ),
        // Lifecycle notices keep their observation mapping (old snapshots may still hold queued
        // entries) but do not pass the push predicate.
        row(
            Event::WorkspaceLeased {
                track_id: track.clone(),
                card_id: worker.clone(),
                lease_id: "lease".into(),
                path: "/tmp/ws".into(),
            },
            ActorId::KernelDispatcher,
            false,
            true,
        ),
        row(
            Event::WorkspaceReleased {
                track_id: track.clone(),
                card_id: worker.clone(),
                lease_id: "lease".into(),
            },
            ActorId::KernelDispatcher,
            false,
            true,
        ),
        row(
            Event::ForgePrMerged {
                track_id: track.clone(),
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
                track_id: track.clone(),
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
            false,
            true,
        ),
        row(
            Event::RatifyRequested {
                track_id: track.clone(),
                reason: "cap_exhausted".into(),
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::RatifyResolved {
                track_id: track.clone(),
                decision: RatifyDecision::Grant,
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::ForgeScanCompleted {
                track_id: track.clone(),
                overlapping_prs: vec![1, 2],
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::ForgePrOpened {
                track_id: track.clone(),
                pr_number: 1,
                head_sha: "head-sha".into(),
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::ForgePrChecks {
                track_id: track.clone(),
                pr_number: 1,
                conclusion: "success".into(),
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::ForgeIssueClosed {
                track_id: track.clone(),
                issue_number: 1,
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::WorktreeProvisioned {
                track_id: track.clone(),
                card_id: worker.clone(),
                path: "/tmp/worktree".into(),
            },
            ActorId::KernelDispatcher,
            false,
            true,
        ),
        row(
            Event::WorktreeCommitted {
                track_id: track.clone(),
                card_id: worker.clone(),
                commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
                branch: "neige/w/card".into(),
                delivery_id: None,
                base_is_ancestor: None,
            },
            ActorId::KernelDispatcher,
            false,
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
        // Conditional kinds, false side: predicate false while the kind-scoped mapping stays Some.
        row(
            task_completed(),
            ActorId::AiPlanner(planner.clone()),
            false,
            true,
        ),
        row(
            task_failed(),
            ActorId::AiPlanner(planner.clone()),
            false,
            true,
        ),
        row(
            report_edited(EditAuthor::Planner),
            ActorId::AiPlanner(planner.clone()),
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
            codex_hook(&planner, "hook.codex.stop"),
            ActorId::User,
            false,
            true,
        ),
        row(
            claude_hook(&planner, "hook.claude.stop"),
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
        // Non-stop hooks map to nothing on either seam.
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
        // Never-push kinds: explicit-false on both seams.
        row(
            Event::AreaUpdated(crate::model::Area {
                id: area.clone(),
                name: "c".into(),
                color: "#000000".into(),
                sort: 0.0,
                kind: crate::model::AreaKind::User,
                default_template_id: None,
                default_cwd: None,
                created_at: 1,
                updated_at: 1,
            }),
            ActorId::User,
            false,
            false,
        ),
        row(
            Event::AreaDeleted { id: area.clone() },
            ActorId::User,
            false,
            false,
        ),
        row(
            Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                crate::model::Track {
                    id: track.clone(),
                    area_id: area.clone(),
                    title: "w".into(),
                    sort: 0.0,
                    archived_at: None,
                    pinned_at: None,
                    lifecycle: crate::model::TrackLifecycle::Working,
                    cwd_wire_alias: String::new(),
                    template_id: None,
                    plugin_scope: None,
                    purpose: None,
                    template_input: None,
                    terminal_at: None,
                    recipe_id: None,
                    recipe_revision: None,
                    claude_permissions_policy: None,
                    workspace: Default::default(),
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
            Event::TrackDeleted {
                id: track.clone(),
                area_id: area.clone(),
            },
            ActorId::User,
            false,
            false,
        ),
        row(
            Event::ProposalSubmitted {
                track_id: track.clone(),
                proposal_id: "pp-1".into(),
                plugin_id: "dev.neige.invest".into(),
                subject_kind: "report".into(),
                base_doc_heads: "ah1:deadbeef".into(),
                ops: vec![calm_types::proposal::ProposalOp::DeleteBlock {
                    block_id: "b_0001".into(),
                    if_rev: 1,
                }],
                note: "why".into(),
                idem_key: "idem-1".into(),
            },
            ActorId::Plugin("dev.neige.invest".into()),
            false,
            false,
        ),
        row(
            Event::ProposalResolved {
                track_id: track.clone(),
                proposal_id: "pp-1".into(),
                plugin_id: "dev.neige.invest".into(),
                decision: calm_types::proposal::ProposalDecision::Accepted,
            },
            ActorId::User,
            false,
            false,
        ),
        row(
            Event::TrackLifecycleChanged {
                id: track.clone(),
                area_id: area.clone(),
                from: crate::model::TrackLifecycle::Draft,
                to: crate::model::TrackLifecycle::Planning,
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
                track_id: track.clone(),
            },
            ActorId::User,
            false,
            false,
        ),
        row(
            Event::WorkerSessionStarted {
                worker_session_id: "rt".into(),
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
            Event::WorkerSessionStatusChanged {
                worker_session_id: "rt".into(),
                card_id: worker.to_string(),
                old_status: calm_types::worker::WorkerSessionState::Starting,
                new_status: calm_types::worker::WorkerSessionState::Running,
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::WorkerSessionSuperseded {
                old_worker_session_id: "rt-old".into(),
                new_worker_session_id: "rt-new".into(),
                card_id: worker.to_string(),
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::HarnessItemAdded {
                worker_session_id: "rt".into(),
                card_id: planner.clone(),
                track_id: track.clone(),
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
                worker_session_id: "rt".into(),
                card_id: planner.clone(),
                track_id: track.clone(),
                old_phase: calm_types::harness::HarnessPhaseTag::Idle,
                new_phase: calm_types::harness::HarnessPhaseTag::TurnRunning,
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::HarnessTranscriptCleared {
                worker_session_id: "rt".into(),
                card_id: planner.clone(),
                track_id: track.clone(),
                cleared_item_count: Some(7),
                cleared_params_bytes: Some(2_048),
                card_age_ms_at_clear: Some(3_600_000),
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::HarnessUserMessageEnqueued {
                worker_session_id: "rt".into(),
                card_id: planner.clone(),
                track_id: track.clone(),
                char_count: 5,
            },
            ActorId::User,
            false,
            false,
        ),
        // Neither a push nor an observation: the queue change is applied before the event is
        // written, so a wake would deliver a turn for no new input.
        row(
            Event::HarnessQueueChanged {
                worker_session_id: "rt".into(),
                card_id: planner.clone(),
                track_id: track.clone(),
                entry_id: "entry-1".into(),
                change: calm_types::event::HarnessQueueChange::Deleted,
                actor: ActorId::User,
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
            Event::CodexWorkerRequested {
                idempotency_key: "k".into(),
                goal: "g".into(),
                context: serde_json::Value::Null,
                acceptance_criteria: None,
                agent_message: None,
            },
            ActorId::AiPlanner(planner.clone()),
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
            ActorId::AiPlanner(planner.clone()),
            false,
            false,
        ),
        row(
            Event::PlanUpdated {
                track_id: track.clone(),
                changed_keys: vec!["k".into()],
                agent_message: None,
            },
            ActorId::AiPlanner(planner.clone()),
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
            Event::TaskContextFrozen {
                track_id: TrackId::default(),
                task_key: String::new(),
                idempotency_key: String::new(),
                task_id: "w:k".into(),
                refs: vec![],
                doc_revs: Default::default(),
                truncated: false,
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::TaskContextAdvanced {
                track_id: Default::default(),
                task_key: String::new(),
                task_id: "w:k".into(),
                changed_refs: Vec::new(),
                verdict: "material".into(),
                rationale: String::new(),
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::ForgePrDiffRead {
                track_id: track.clone(),
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
                track_id: track.clone(),
                issue_number: 1,
                artifact_path: "/tmp/issue.md".into(),
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::WorktreeRemoved {
                track_id: track.clone(),
                card_id: worker.clone(),
                path: "/tmp/worktree".into(),
            },
            ActorId::KernelDispatcher,
            false,
            false,
        ),
        row(
            Event::TaskExecutionSettled {
                task_id: "w:retry".into(),
                operation_id: "op".into(),
            },
            ActorId::KernelDispatcher,
            true,
            true,
        ),
        row(
            Event::TaskExecutionSettled {
                task_id: "w:retry".into(),
                operation_id: "op".into(),
            },
            ActorId::User,
            false,
            true,
        ),
    ];

    let (publication_repo, publication_event) = planner_push_publication_fixture().await;
    for (actor, expect_push) in [
        (ActorId::Kernel, true),
        (ActorId::KernelDispatcher, true),
        (ActorId::User, false),
    ] {
        rows.push(row(publication_event.clone(), actor, expect_push, true));
    }

    let (candidate_repo, candidate_event) = planner_push_candidate_fixture().await;
    for (actor, expect_push) in [
        (ActorId::Kernel, true),
        (ActorId::KernelDispatcher, true),
        (ActorId::User, false),
    ] {
        rows.push(row(candidate_event.clone(), actor, expect_push, true));
    }
    // #1727 S4: pure arm — actor ∈ {Kernel, KernelDispatcher} ∧ wake_reason != deferred_to_gate.
    // The mapping is row-backed (tasks row + lease), so every row expects an observation.
    let (delivery_repo, delivery_event) = planner_push_delivery_fixture().await;
    let Event::TaskGitDeliverySettled { task_id, .. } = &delivery_event else {
        unreachable!()
    };
    for (actor, expect_push) in [
        (ActorId::Kernel, true),
        (ActorId::KernelDispatcher, true),
        (ActorId::User, false),
        (ActorId::AiPlanner(planner.clone()), false),
    ] {
        rows.push(row(delivery_event.clone(), actor, expect_push, true));
    }
    rows.push(row(
        git_delivery_settled_event(task_id, DeliveryWakeReason::DeferredToGate),
        ActorId::KernelDispatcher,
        false,
        true,
    ));
    PlannerPushWiringTable {
        write,
        track,
        rows,
        publication_repo,
        candidate_repo,
        delivery_repo,
    }
}

/// Predicate⇒mapping consistency table over every event kind; the census asserts every kind
/// has a row, and a row expecting a push without an observation is rejected outright.
#[tokio::test]
async fn planner_push_predicate_and_observation_mapping_agree() {
    let PlannerPushWiringTable {
        write,
        track,
        rows,
        publication_repo,
        candidate_repo,
        delivery_repo,
    } = planner_push_wiring_table().await;
    let mut covered = std::collections::BTreeSet::new();
    for row in &rows {
        let kind = row.event.kind_tag();
        covered.insert(kind.to_string());
        assert!(
            !row.expect_push || row.expect_observation,
            "row for {kind} violates predicate⇒mapping: expect_push without expect_observation"
        );
        assert_eq!(
            event_warrants_planner_push(&row.event, &row.actor, &write),
            row.expect_push,
            "push predicate mismatch for {kind} (actor {})",
            row.actor
        );
        let observation = if let Event::TaskFilePublicationSettled { operation_id, .. } = &row.event
        {
            // This kind deliberately has no pure mapping: live dispatch and
            // catch-up require retained publication identity and outcome.
            assert!(
                harness_observation_from_event(&track, &row.event, Some("impl-parser")).is_none()
            );
            let resolved = resolve_harness_observation(&publication_repo, &track, &row.event)
                .await
                .expect("retained publication resolves");
            assert!(
                matches!(&resolved, Some(HarnessObservation::SystemContext { text })
                if text.contains(operation_id) && text.contains("failed")
                    && text.contains("file producer has no accepted completion report"))
            );
            resolved
        } else if let Event::TaskCandidateVerificationSettled { .. } = &row.event {
            assert!(
                harness_observation_from_event(&track, &row.event, Some("impl-parser")).is_none()
            );
            let resolved = resolve_harness_observation(&candidate_repo, &track, &row.event)
                .await
                .expect("retained candidate resolves");
            assert!(
                matches!(&resolved,Some(HarnessObservation::SystemContext { text }) if text.contains("failed") && text.contains("candidate authority withdrawn"))
            );
            resolved
        } else if let Event::TaskGitDeliverySettled { task_id, .. } = &row.event {
            assert!(harness_observation_from_event(&track, &row.event, Some("deliver")).is_none());
            let resolved = resolve_harness_observation(&delivery_repo, &track, &row.event)
                .await
                .expect("tasks row + lease resolve");
            assert!(
                matches!(&resolved, Some(HarnessObservation::TaskGitDeliverySettled { key, attempt_id, retained_path: Some(_), .. })
                    if key == "deliver" && attempt_id == task_id)
            );
            resolved
        } else {
            harness_observation_from_event(&track, &row.event, Some("impl-parser"))
        };
        assert_eq!(
            observation.is_some(),
            row.expect_observation,
            "observation mapping mismatch for {kind} (actor {})",
            row.actor
        );
    }

    // Completeness: every Event kind must have at least one row.
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

/// `PLANNER_CATCH_UP_KINDS` must be exactly the kinds with any `expect_push = true` row.
#[tokio::test]
async fn planner_catch_up_kinds_equal_the_push_capable_kinds() {
    let table = planner_push_wiring_table().await;
    let all = all_event_kind_tags();
    let push_capable: std::collections::BTreeSet<String> = table
        .rows
        .iter()
        .filter(|row| row.expect_push)
        .map(|row| row.event.kind_tag().to_string())
        .collect();
    let catch_up: std::collections::BTreeSet<String> = PLANNER_CATCH_UP_KINDS
        .iter()
        .map(|kind| kind.to_string())
        .collect();
    assert_eq!(
        catch_up.len(),
        PLANNER_CATCH_UP_KINDS.len(),
        "PLANNER_CATCH_UP_KINDS lists a kind twice"
    );
    for kind in &all {
        assert_eq!(
            catch_up.contains(kind),
            push_capable.contains(kind),
            "{kind}: catch-up list membership must match \"the push predicate can \
             return true for this kind\" (push-capable rows: {push_capable:?}, \
             PLANNER_CATCH_UP_KINDS: {catch_up:?})"
        );
    }
    let unknown: Vec<_> = catch_up.difference(&all).collect();
    assert!(
        unknown.is_empty(),
        "PLANNER_CATCH_UP_KINDS names kinds outside the serde census: {unknown:?}"
    );
    assert_eq!(catch_up, push_capable);
}

/// #1727 S4 D2: the settlement arm reads the actor and the event's `wake_reason` only — never a
/// row — so live push and boot replay give the same answer for the same envelope.
#[test]
fn settlement_push_arm_is_pure_on_wake_reason() {
    let planner = CardId::from("planner");
    let worker = CardId::from("worker");
    let actors = [
        (ActorId::Kernel, true),
        (ActorId::KernelDispatcher, true),
        (ActorId::AiPlanner(planner.clone()), false),
        (ActorId::AiCodex(worker.clone()), false),
        (ActorId::AiClaude(worker.clone()), false),
        (ActorId::User, false),
    ];
    let reasons = [
        (DeliveryWakeReason::Failed, true),
        (DeliveryWakeReason::UngatedCandidate, true),
        (DeliveryWakeReason::GateAlreadyTerminal, true),
        (DeliveryWakeReason::DeferredToGate, false),
    ];
    let mut role_lookups = 0usize;
    for (actor, kernel_actor) in &actors {
        for (reason, wakes) in reasons {
            let event = git_delivery_settled_event("w:k", reason);
            let pushed = event_warrants_planner_push_with_role(&event, actor, |_| {
                role_lookups += 1;
                Some(CardRole::Planner)
            });
            assert_eq!(
                pushed,
                *kernel_actor && wakes,
                "actor {actor} wake_reason {reason:?}: only a kernel actor and a non-deferred \
                 disposition push"
            );
        }
    }
    assert_eq!(
        role_lookups, 0,
        "the settlement arm consults neither the role cache nor any row"
    );
    // The failed shape carries the same arm: `result` is not read.
    let failed = Event::TaskGitDeliverySettled {
        task_id: "w:k".into(),
        idempotency_key: "w:k".into(),
        track_id: TrackId::from("w"),
        card_id: worker,
        delivery_id: "delivery-1".into(),
        ordinal: 1,
        result: DeliverySettlement::Failed {
            code: DeliveryFailureCode::CommitFailed,
            reason: "index.lock".into(),
            retry_allowed: true,
        },
        wake_reason: DeliveryWakeReason::Failed,
    };
    assert!(event_warrants_planner_push_with_role(
        &failed,
        &ActorId::KernelDispatcher,
        |_| None
    ));
    assert!(!event_warrants_planner_push_with_role(
        &failed,
        &ActorId::AiPlanner(planner),
        |_| None
    ));
}

/// The slice-2 wake sentences: the plan key comes from the tasks row, the retained path from the
/// lease, and neither sentence names a tool that does not exist yet.
#[tokio::test]
async fn settled_event_maps_to_observation_with_turn_text() {
    let (repo, candidate) = planner_push_delivery_fixture().await;
    let track = TrackId::from("w");
    let Event::TaskGitDeliverySettled { task_id, .. } = &candidate else {
        unreachable!()
    };
    let task_id = task_id.clone();

    let observation = resolve_harness_observation(&repo, &track, &candidate)
        .await
        .unwrap()
        .expect("a candidate settlement maps to an observation");
    assert!(observation.is_hard_fire());
    let text = observation.to_turn_text();
    assert!(
        text.starts_with(&format!(
            "Task deliver delivered candidate delivery-1 ({}, base {}). Accept with calm.task.verdict;",
            "c".repeat(40),
            "b".repeat(40)
        )),
        "{text}"
    );
    assert!(
        text.ends_with(&format!("read the worker output at runs/{task_id}.md.")),
        "{text}"
    );
    assert!(!text.contains("no change"), "{text}");
    assert!(
        !text.contains("calm.task.delivery") && !text.contains("base:{"),
        "{text}"
    );

    // `commit_sha == base_sha` renders the no-change marker.
    let unchanged = Event::TaskGitDeliverySettled {
        task_id: task_id.clone(),
        idempotency_key: task_id.clone(),
        track_id: track.clone(),
        card_id: CardId::from("worker"),
        delivery_id: "delivery-1".into(),
        ordinal: 1,
        result: DeliverySettlement::Candidate {
            candidate_id: "delivery-1".into(),
            commit_sha: "b".repeat(40),
            base_sha: "b".repeat(40),
            base_is_ancestor: true,
        },
        wake_reason: DeliveryWakeReason::GateAlreadyTerminal,
    };
    let text = resolve_harness_observation(&repo, &track, &unchanged)
        .await
        .unwrap()
        .unwrap()
        .to_turn_text();
    assert!(
        text.contains(&format!(
            "({}, base {}, no change).",
            "b".repeat(40),
            "b".repeat(40)
        )),
        "{text}"
    );

    let failed = Event::TaskGitDeliverySettled {
        task_id: task_id.clone(),
        idempotency_key: task_id.clone(),
        track_id: track.clone(),
        card_id: CardId::from("worker"),
        delivery_id: "delivery-2".into(),
        ordinal: 2,
        result: DeliverySettlement::Failed {
            code: DeliveryFailureCode::CommitFailed,
            // A reason ends with its own period (as every producer's does); the renderer adds none.
            reason: "git commit exited 128: index.lock exists.".into(),
            retry_allowed: true,
        },
        wake_reason: DeliveryWakeReason::Failed,
    };
    let observation = resolve_harness_observation(&repo, &track, &failed)
        .await
        .unwrap()
        .expect("a failed settlement maps to an observation");
    let HarnessObservation::TaskGitDeliverySettled {
        retained_path: Some(retained_path),
        ..
    } = &observation
    else {
        panic!("the lease path is the retained path: {observation:?}");
    };
    assert!(retained_path.ends_with("/w/worker"), "{retained_path}");
    let text = observation.to_turn_text();
    assert!(
        text.starts_with(
            "Task deliver Git delivery FAILED (commit_failed): git commit exited 128: index.lock exists. "
        ),
        "{text}"
    );
    assert!(
        text.contains(&format!("Files retained at {retained_path}; ")),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "read the worker output at runs/{task_id}.md. Decide: "
        )),
        "{text}"
    );
    // Slice 3: the decision clause names the delivery the event carried; a retryable failure
    // states G4 with it (the retry delivers the branch tip as it stands now).
    assert!(
        text.ends_with(
            "Decide: calm.task.delivery{action:\"retry\"|\"abandon\", expected_delivery_id:\"delivery-2\"}. \
             Retry delivers the branch tip as it stands now; commits and files added after the \
             base by anyone are included."
        ),
        "{text}"
    );

    // D2: `workspace_missing` is the kernel's proof the lease directory is absent. The lease row
    // still names a path here (the mapping copies it), and the sentence must not.
    let missing = Event::TaskGitDeliverySettled {
        task_id: task_id.clone(),
        idempotency_key: task_id.clone(),
        track_id: track.clone(),
        card_id: CardId::from("worker"),
        delivery_id: "delivery-3".into(),
        ordinal: 3,
        result: DeliverySettlement::Failed {
            code: DeliveryFailureCode::WorkspaceMissing,
            reason: "lease directory absent at settlement.".into(),
            retry_allowed: false,
        },
        wake_reason: DeliveryWakeReason::Failed,
    };
    let observation = resolve_harness_observation(&repo, &track, &missing)
        .await
        .unwrap()
        .expect("a workspace_missing settlement maps to an observation");
    assert!(
        matches!(
            &observation,
            HarnessObservation::TaskGitDeliverySettled {
                retained_path: None,
                ..
            }
        ),
        "the lease row still carries a path, the observation must not: {observation:?}"
    );
    let text = observation.to_turn_text();
    assert!(
        text.starts_with(
            "Task deliver Git delivery FAILED (workspace_missing): lease directory absent at settlement. "
        ),
        "{text}"
    );
    assert!(!text.contains("Files retained at"), "{text}");
    assert!(
        text.contains(&format!(
            "absent at settlement. Read the worker output at runs/{task_id}.md. Decide: "
        )),
        "{text}"
    );
    // `retry_allowed: false`: only `abandon` is offered.
    assert!(
        text.ends_with(
            "Decide: calm.task.delivery{action:\"abandon\", expected_delivery_id:\"delivery-3\"}."
        ),
        "{text}"
    );
    assert!(!text.to_ascii_lowercase().contains("retry"), "{text}");

    // Once the worktree is removed the sentence drops the retained clause.
    let removed = crate::event::EventScope::Card {
        card: CardId::from("worker"),
        track: track.clone(),
        area: AreaId::from("c"),
    };
    crate::db::write_in_tx_typed(&repo, move |tx| {
        Box::pin(async move {
            crate::db::sqlite::append_decision_event_in_tx(
                tx,
                &ActorId::KernelDispatcher,
                &removed,
                None,
                &Event::WorktreeRemoved {
                    track_id: TrackId::from("w"),
                    card_id: CardId::from("worker"),
                    path: "/gone".into(),
                },
            )
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let text = resolve_harness_observation(&repo, &track, &failed)
        .await
        .unwrap()
        .unwrap()
        .to_turn_text();
    assert!(!text.contains("Files retained at"), "{text}");
    assert!(
        text.contains("index.lock exists. Read the worker output at runs/"),
        "{text}"
    );

    // No tasks row → no observation (the same outcome as the other row-backed settlements).
    let orphan = git_delivery_settled_event("no-such-attempt", DeliveryWakeReason::Failed);
    assert!(
        resolve_harness_observation(&repo, &track, &orphan)
            .await
            .unwrap()
            .is_none()
    );
    // A foreign envelope track → no observation.
    assert!(
        resolve_harness_observation(&repo, &TrackId::from("foreign"), &candidate)
            .await
            .unwrap()
            .is_none()
    );
}

/// Slice 3: the failed wake sentence names the decision — `retry`|`abandon` when the row admits
/// a retry, `abandon` alone when it does not (`workspace_missing`) — and the `delivery_id` the
/// decision must quote. An observation persisted before `delivery_id` existed names no decision.
#[tokio::test]
async fn failed_wake_text_names_the_delivery_action() {
    use crate::git_candidate::delivery::{classify_failure, unresolved_failure};
    use crate::operation::forge_action_adapter::ForgeActionResultFile;
    let (repo, _) = planner_push_delivery_fixture().await;
    let track = TrackId::from("w");
    let failed =
        |delivery_id: &str, code: DeliveryFailureCode, reason: &str, retry_allowed: bool| {
            Event::TaskGitDeliverySettled {
                task_id: "delivery-source-attempt".into(),
                idempotency_key: "delivery-source-attempt".into(),
                track_id: track.clone(),
                card_id: CardId::from("worker"),
                delivery_id: delivery_id.into(),
                ordinal: 1,
                result: DeliverySettlement::Failed {
                    code,
                    reason: reason.into(),
                    retry_allowed,
                },
                wake_reason: DeliveryWakeReason::Failed,
            }
        };

    let observation = resolve_harness_observation(
        &repo,
        &track,
        &failed(
            "d-retryable",
            DeliveryFailureCode::CommitFailed,
            "hook.",
            true,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(&observation, HarnessObservation::TaskGitDeliverySettled { delivery_id: Some(id), .. } if id == "d-retryable"),
        "{observation:?}"
    );
    let text = observation.to_turn_text();
    assert!(text.contains("Decide: calm.task.delivery{"), "{text}");
    assert!(text.contains("action:\"retry\"|\"abandon\""), "{text}");
    assert!(
        text.contains("expected_delivery_id:\"d-retryable\""),
        "{text}"
    );
    // G4 rides with the retry offer, after the decision sentence.
    assert!(
        text.ends_with(
            "expected_delivery_id:\"d-retryable\"}. Retry delivers the branch tip as it stands \
             now; commits and files added after the base by anyone are included."
        ),
        "{text}"
    );

    let text = resolve_harness_observation(
        &repo,
        &track,
        &failed(
            "d-gone",
            DeliveryFailureCode::WorkspaceMissing,
            "hook.",
            false,
        ),
    )
    .await
    .unwrap()
    .unwrap()
    .to_turn_text();
    assert!(text.contains("Decide: calm.task.delivery{"), "{text}");
    assert!(text.contains("action:\"abandon\""), "{text}");
    // No retry offer, no G4 clause (the sentence would say "Retry").
    assert!(!text.to_ascii_lowercase().contains("retry"), "{text}");
    assert!(
        text.ends_with("expected_delivery_id:\"d-gone\"}."),
        "{text}"
    );

    // The production reasons end with their own period and the renderer adds none: no `..` in
    // the wake text for a code-11 reason (the fixed sentence alone) or an `unresolved` one (the
    // sentence plus the kernel's detail line, which `unresolved_failure` terminates). G4 appears
    // exactly once, after the decision sentence — the fixed sentences no longer carry it.
    let code_11 = ForgeActionResultFile {
        exit_code: 11,
        stdout: String::new(),
    };
    let (code, reason, retry_allowed) =
        classify_failure(Some(&code_11), Some("action-failed"), true);
    assert_eq!(code, DeliveryFailureCode::ProvenanceMismatch);
    let (unresolved, unresolved_reason, unresolved_retry) = unresolved_failure(
        "ref refs/neige/candidates/w/worker/d-11 does not resolve to the reported commit abc123",
    );
    assert_eq!(unresolved, DeliveryFailureCode::Unresolved);
    for (id, code, reason, retry_allowed) in [
        ("d-11", code, reason.as_str(), retry_allowed),
        (
            "d-unresolved",
            unresolved,
            unresolved_reason.as_str(),
            unresolved_retry,
        ),
    ] {
        let text =
            resolve_harness_observation(&repo, &track, &failed(id, code, reason, retry_allowed))
                .await
                .unwrap()
                .unwrap()
                .to_turn_text();
        assert!(!text.contains(".."), "{text}");
        assert!(
            text.contains(&format!("{reason} Files retained at ")),
            "{text}"
        );
        assert_eq!(text.matches("Retry delivers").count(), 1, "{text}");
    }
    assert!(
        unresolved_reason.ends_with("reported commit abc123."),
        "{unresolved_reason}"
    );

    // Pre-slice-3 snapshot shape: no `delivery_id`, no decision clause, the rest intact.
    let legacy = HarnessObservation::TaskGitDeliverySettled {
        key: "deliver".into(),
        attempt_id: "delivery-source-attempt".into(),
        result: DeliverySettlement::Failed {
            code: DeliveryFailureCode::CommitFailed,
            reason: "hook.".into(),
            retry_allowed: true,
        },
        retained_path: None,
        delivery_id: None,
    };
    let text = legacy.to_turn_text();
    assert!(!text.contains("calm.task.delivery"), "{text}");
    assert!(
        text.ends_with("Read the worker output at runs/delivery-source-attempt.md."),
        "{text}"
    );
    // A snapshot written by slice 2 has no `delivery_id` key at all; it decodes to `None`.
    let mut wire = serde_json::to_value(&legacy).unwrap();
    assert!(
        wire.as_object_mut()
            .unwrap()
            .remove("delivery_id")
            .is_some()
    );
    let decoded: HarnessObservation = serde_json::from_value(wire).unwrap();
    assert_eq!(decoded, legacy);
}

/// A9 / slice 3: the `task.failed` that `calm.task.delivery{abandon}` appends for a gated row
/// (`status_detail = delivery-abandoned`, with or without a reason tail) is not a Planner wake —
/// the tool receipt is the answer. `delivery-abandoned` is not in the pre-gate class table, so
/// the gated `task.failed` rule suppresses it; adding it there would push it.
#[tokio::test]
async fn abandon_task_failed_is_not_a_wake() {
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut abandoned = crate::model::Task {
        id: "w:abandoned".into(),
        track_id: "w".into(),
        key: "abandoned".into(),
        kind: crate::model::TaskKind::Codex,
        goal: "g".into(),
        context_json: "null".into(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: "[]".into(),
        priority: 0,
        gate_json: Some("{\"steps\":[{\"name\":\"t\",\"cmd\":\"true\"}]}".to_string()),
        status: crate::model::TaskStatus::Failed,
        status_detail: Some(crate::db::sqlite::TASK_STATUS_DETAIL_DELIVERY_ABANDONED.into()),
        worker_card_id: None,
        gate_result_json: None,
        gate_attempt: 1,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR.into(),
        spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
        created_at_ms: 1,
        updated_at_ms: 1,
        finished_at_ms: Some(2),
    };
    let mut with_reason = abandoned.clone();
    with_reason.id = "w:abandoned-reason".into();
    with_reason.key = "abandoned-reason".into();
    with_reason.status_detail = Some(crate::db::sqlite::status_detail_with_reason(
        crate::db::sqlite::TASK_STATUS_DETAIL_DELIVERY_ABANDONED,
        "hook keeps rejecting the commit",
    ));
    abandoned.status_detail = Some("delivery-abandoned".into());
    crate::db::write_in_tx_typed(&repo, move |tx| {
        Box::pin(async move {
            crate::test_support::insert_task_tx(tx, &abandoned).await?;
            crate::test_support::insert_task_tx(tx, &with_reason).await?;
            Ok(())
        })
    })
    .await
    .expect("seed tasks");
    let failed = |key: &str, reason: &str| Event::TaskFailed {
        idempotency_key: format!("w:{key}"),
        reason: reason.into(),
        details: None,
        agent_message: None,
    };
    let event = failed("abandoned", "delivery-abandoned");
    assert!(
        event_warrants_planner_push_with_role(&event, &ActorId::KernelDispatcher, |_| None),
        "absent the deferral rule the kernel-authored task.failed would be pushed"
    );
    assert!(
        is_deferred_self_report(&repo, &event).await,
        "the abandon's task.failed is suppressed: the tool receipt already answered the Planner"
    );
    assert!(
        is_deferred_self_report(
            &repo,
            &failed(
                "abandoned-reason",
                "delivery-abandoned: hook keeps rejecting the commit"
            )
        )
        .await,
        "a reason tail on the row does not reclassify it as a pre-gate failure"
    );
}

/// The row-level guard in `git_delivery_settled::observation`: the envelope track and the event's
/// `track_id` are both `w` (so the envelope guard passes) and the tasks row exists (so the missing
/// row arm does not fire), but that row lives on track `x` → no observation.
#[tokio::test]
async fn settled_event_for_a_row_on_another_track_maps_to_nothing() {
    let (repo, _) = planner_push_delivery_fixture().await;
    sqlx::raw_sql(
        "INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('x','c','Other',0,1,1);
         INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,priority,status,\
          worker_card_id,created_at_ms,updated_at_ms) \
          VALUES('row-on-x','x','other','codex','Deliver','{}','[]',0,'done','worker',1,1);",
    )
    .execute(repo.pool())
    .await
    .unwrap();
    let track = TrackId::from("w");
    let event = git_delivery_settled_event("row-on-x", DeliveryWakeReason::Failed);
    let Event::TaskGitDeliverySettled {
        track_id: event_track,
        ..
    } = &event
    else {
        unreachable!()
    };
    assert_eq!(event_track, &track, "the envelope guard passes");
    let row = crate::db::ServerRepoReadExt::task_get(&repo, "row-on-x")
        .await
        .unwrap()
        .expect("the tasks row exists");
    assert_eq!(row.track_id, "x");

    assert_eq!(
        git_delivery_settled::observation(&repo, &track, &event)
            .await
            .unwrap(),
        None,
        "a tasks row on another track maps to nothing"
    );
}

/// #1727 S4 D2 on both delivery paths: a Kernel-actor `deferred_to_gate` settlement for a
/// `verifying` row is silent through the live `handle_envelope` arm, and stays silent on boot
/// catch-up after the gate flipped the row to `done`; an `ungated_candidate` settlement wakes the
/// Planner exactly once live and exactly once on replay.
#[tokio::test]
async fn deferred_settlement_is_silent_live_and_on_replay() {
    use crate::db::prelude::*;
    use crate::db::sqlite::{
        SqlxRepo, append_decision_event_in_tx, card_create_with_id_tx, session_start_runtime_tx,
    };
    use crate::harness::queue::{MutationRefused, QueueEntryId, QueueMutation};
    use crate::harness::{
        HarnessConfig, HarnessPhaseTag, HarnessSnapshot, PlannerHarness, PlannerHarnessParams,
    };
    use crate::model::{NewArea, NewCard, NewTrack, new_id, now_ms};
    use crate::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    use crate::track_area_cache::TrackAreaCache;

    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    let area = repo
        .area_create(NewArea {
            name: "delivery wake".into(),
            color: "#222222".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "delivery wake".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    track_area_cache.insert(track.id.clone(), area.id.clone());

    let mut tx = repo.pool().begin().await.unwrap();
    let planner_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!({"schemaVersion": 1, "planner_harness": true}),
        },
        CardRole::Planner,
        false,
        &role_cache,
    )
    .await
    .unwrap();
    let worker_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!({"schemaVersion": 1}),
        },
        CardRole::Worker,
        true,
        &role_cache,
    )
    .await
    .unwrap();
    // The gated row is still `verifying` when its candidate settles (the gate verdict is the
    // wake); the ungated row is already `done` (nothing else will wake the Planner for it).
    let mk_task = |key: &str, gate_json: Option<String>, status: crate::model::TaskStatus| {
        crate::model::Task {
            id: format!("{}:{key}", track.id),
            track_id: track.id.to_string(),
            key: key.into(),
            kind: crate::model::TaskKind::Codex,
            goal: "g".into(),
            context_json: "null".into(),
            acceptance_criteria: None,
            cwd: None,
            depends_on_json: "[]".into(),
            priority: 0,
            gate_json,
            status,
            status_detail: None,
            worker_card_id: Some(worker_card.id.to_string()),
            gate_result_json: None,
            gate_attempt: 1,
            gate_pid: None,
            gate_pid_starttime: None,
            gate_pid_boot_id: None,
            running_deadline_ms: None,
            context_stale_at_ms: None,
            declared_by: calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR.into(),
            spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
            created_at_ms: 1,
            updated_at_ms: 1,
            finished_at_ms: None,
        }
    };
    let deferred = mk_task(
        "gated",
        Some("{\"steps\":[{\"name\":\"t\",\"cmd\":\"true\"}]}".into()),
        crate::model::TaskStatus::Verifying,
    );
    let ungated = mk_task("ungated", None, crate::model::TaskStatus::Done);
    crate::test_support::insert_task_tx(&mut tx, &deferred)
        .await
        .unwrap();
    crate::test_support::insert_task_tx(&mut tx, &ungated)
        .await
        .unwrap();
    let worker_session_id = new_id();
    let thread_id = "thread-delivery-wake".to_string();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: worker_session_id.clone(),
            card_id: planner_card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // A live Planner harness in the registry the dispatcher consults; the forced `TurnRunning`
    // phase keeps every delivered observation queued instead of issuing a turn on the stub.
    let events = EventBus::new();
    let registry = HarnessRegistry::new();
    let handle = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: worker_session_id.clone(),
        track_id: track.id.clone(),
        card_id: planner_card.id.clone(),
        thread_id: Some(thread_id),
        repo: repo.clone(),
        events: events.clone(),
        card_role_cache: role_cache.clone(),
        track_area_cache: track_area_cache.clone(),
        daemon: SharedCodexAppServer::new_stub(repo.clone()),
        config: HarnessConfig::default(),
        snapshot,
    });
    handle
        .force_phase_for_dev(HarnessPhaseTag::TurnRunning)
        .await
        .unwrap();
    let reservation = registry
        .try_reserve(worker_session_id.clone())
        .expect("vacant slot");
    assert!(reservation.install(handle.clone()));

    // The production `Inner` behind `handle_envelope`, minus the bus listener and the periodic
    // loops. The scheduler's operation runtime is dropped, so its poke drives no gate.
    let write = WriteContext::new(role_cache, track_area_cache);
    let semaphore = Arc::new(Semaphore::new(1));
    let scheduler = Scheduler::new_with_task_budget_default(
        repo.clone(),
        events.clone(),
        write.clone(),
        std::sync::Weak::new(),
        Arc::clone(&semaphore),
        std::env::temp_dir().join("neige-dispatcher-test-gate-logs"),
        crate::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    );
    let context_monitor = Arc::new(TaskContextMonitor::new_with_metrics(
        repo.clone(),
        events,
        write.clone(),
        scheduler.context_metrics(),
    ));
    let inner = Arc::new(Inner {
        repo: repo.clone(),
        write,
        harness: registry,
        scheduler,
        context_monitor,
        push_cursor: EventCursorCache::new(),
        push_locks: DashMap::new(),
        failure_push_hook: std::sync::Mutex::new(None),
        semaphore,
    });

    let scope = track_scope(&track.id, &area.id);
    let persist = |event: Event| {
        let repo = repo.clone();
        let scope = scope.clone();
        async move {
            let mut tx = repo.pool().begin().await.unwrap();
            let id = append_decision_event_in_tx(&mut tx, &ActorId::Kernel, &scope, None, &event)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            BroadcastEnvelope {
                id,
                event_version: 1,
                actor: ActorId::Kernel,
                scope,
                event,
            }
        }
    };
    let settled_observations = |handle: PlannerHarness| async move {
        // A refused no-op queue command acknowledges every prior delivery; `snapshot()` alone
        // does not drain the asynchronous observation ingress.
        assert_eq!(
            handle
                .mutate_pending_entry(
                    QueueMutation::Delete {
                        entry_id: QueueEntryId::from_wire("absent-observation-barrier".into()),
                        if_entry_rev: 1,
                    },
                    ActorId::User,
                )
                .await
                .unwrap(),
            Err(MutationRefused::NotFound)
        );
        handle
            .snapshot()
            .await
            .pending_observations()
            .into_iter()
            .filter(|observation| {
                matches!(
                    observation,
                    HarnessObservation::TaskGitDeliverySettled { .. }
                )
            })
            .collect::<Vec<_>>()
    };

    // Live: the deferred settlement is silent, and the cursor does not move.
    let deferred_envelope = persist(git_delivery_settled_event_on(
        &track.id,
        &deferred.id,
        DeliveryWakeReason::DeferredToGate,
    ))
    .await;
    let deferred_event_id = deferred_envelope.id;
    Arc::clone(&inner).handle_envelope(deferred_envelope).await;
    assert_eq!(
        settled_observations(handle.clone()).await,
        Vec::<HarnessObservation>::new(),
        "a deferred_to_gate settlement must not wake the Planner live"
    );
    assert_eq!(inner.push_cursor.get(&planner_card.id), 0);

    // Live sibling: the ungated candidate wakes exactly once (its settlement prefix replays the
    // deferred row's event again and still yields nothing).
    let ungated_envelope = persist(git_delivery_settled_event_on(
        &track.id,
        &ungated.id,
        DeliveryWakeReason::UngatedCandidate,
    ))
    .await;
    let ungated_event_id = ungated_envelope.id;
    assert!(ungated_event_id > deferred_event_id);
    Arc::clone(&inner).handle_envelope(ungated_envelope).await;
    let live = settled_observations(handle.clone()).await;
    assert!(
        matches!(
            live.as_slice(),
            [HarnessObservation::TaskGitDeliverySettled { attempt_id, .. }] if attempt_id == &ungated.id
        ),
        "exactly one live wake, for the ungated row: {live:?}"
    );
    assert_eq!(inner.push_cursor.get(&planner_card.id), ungated_event_id);

    // Boot replay from before both events, after the gate flipped the deferred row to `done`:
    // the event's wake_reason, not the row, decides — still exactly one, for the ungated row.
    sqlx::query("UPDATE tasks SET status = 'done' WHERE id = ?1")
        .bind(&deferred.id)
        .execute(repo.pool())
        .await
        .unwrap();
    let replayed = crate::harness::catch_up::observations_since(
        repo.as_ref(),
        &track.id,
        deferred_event_id - 1,
        None,
    )
    .await
    .unwrap();
    let replayed_settlements = replayed
        .iter()
        .filter(|(_, observation)| {
            matches!(
                observation,
                HarnessObservation::TaskGitDeliverySettled { .. }
            )
        })
        .collect::<Vec<_>>();
    assert!(
        matches!(
            replayed_settlements.as_slice(),
            [(id, HarnessObservation::TaskGitDeliverySettled { attempt_id, .. })]
                if *id == ungated_event_id && attempt_id == &ungated.id
        ),
        "exactly one replayed wake, for the ungated row: {replayed:?}"
    );

    handle.shutdown().await.unwrap();
}

/// Same-track acquisitions must serialize (a concurrent dedup-check-and-deliver would lose
/// events in the seed→insert window); different tracks stay independent.
#[tokio::test]
async fn per_track_push_lock_serializes_same_track_runs_in_parallel_across_tracks() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Same map shape as `Inner::push_locks`.
    let push_locks: DashMap<TrackId, Arc<tokio::sync::Mutex<()>>> = DashMap::new();
    let take_lock = |track_id: &TrackId| -> Arc<tokio::sync::Mutex<()>> {
        push_locks
            .entry(track_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };

    // Track concurrent occupancy. Same-track: must never exceed 1.
    let in_flight_a = Arc::new(AtomicUsize::new(0));
    let max_in_flight_a = Arc::new(AtomicUsize::new(0));
    let track_a = TrackId::from("track-a");

    let mut handles = vec![];
    for i in 0..8 {
        let lock = take_lock(&track_a);
        let in_flight = in_flight_a.clone();
        let max_in_flight = max_in_flight_a.clone();
        handles.push(tokio::spawn(async move {
            let _g = lock.lock_owned().await;
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max_in_flight.fetch_max(now, Ordering::SeqCst);
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
        "same-track per-track lock must serialize: observed concurrent holders"
    );

    // Different tracks: independent locks → can run in parallel.
    let in_flight_total = Arc::new(AtomicUsize::new(0));
    let max_in_flight_total = Arc::new(AtomicUsize::new(0));
    let mut handles = vec![];
    for i in 0..6 {
        let track: TrackId = format!("track-parallel-{i}").into();
        let lock = take_lock(&track);
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
    assert!(
        max_in_flight_total.load(Ordering::SeqCst) > 1,
        "different-track locks must allow parallel runs; observed serialization"
    );
}

mod report_edit_block_refs {
    use super::*;
    use crate::db::{ServerRepoSyncDomainRawExt, sqlite::SqlxRepo};
    use crate::model::{NewArea, NewTrack};
    use crate::routes::theme::RequestTheme;
    use crate::state::WriteContext;
    use crate::track_area_cache::TrackAreaCache;
    use crate::track_report::{TrackReportPayload, persist_report, resolve_report_for_track};
    use calm_types::report_edit_diff::ReportBlockRef;

    struct Fixture {
        repo: SqlxRepo,
        events: EventBus,
        write: WriteContext,
        track_id: TrackId,
    }

    async fn fixture() -> Fixture {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let area = repo
            .area_create(NewArea {
                name: "block-refs".into(),
                color: "#123456".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                area_id: area.id,
                title: "report".into(),
                sort: None,
                cwd: "/tmp".into(),
                template_id: None,
                plugin_scope: None,
                template_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO cards \
             (id, track_id, kind, sort, payload, role, deletable, body_crdt, created_at, updated_at) \
             VALUES ('report', ?1, 'track-report', -1, ?2, 'reportcard', 0, NULL, 1, 1)",
        )
        .bind(track.id.as_str())
        .bind(
            serde_json::to_string(&serde_json::json!({"schemaVersion": 1, "summary": "", "body": ""}))
                .unwrap(),
        )
        .execute(repo.pool())
        .await
        .unwrap();
        Fixture {
            repo,
            events: EventBus::new(),
            write: WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
            track_id: track.id,
        }
    }

    /// Persist `body` through the production writer and return the `track.report_edited` it broadcast.
    async fn persist_and_capture(fx: &Fixture, body: &str) -> Event {
        let mut rx = fx.events.subscribe();
        let (track, card, current) = resolve_report_for_track(&fx.repo, fx.track_id.as_str())
            .await
            .unwrap();
        let if_doc_rev = current.doc_rev;
        persist_report(
            &fx.repo,
            &fx.events,
            &fx.write,
            ActorId::User,
            EditAuthor::User,
            track,
            card,
            current,
            TrackReportPayload::new("s".to_string(), body.to_string()),
            if_doc_rev,
            None,
            None,
            false,
        )
        .await
        .unwrap();
        loop {
            let envelope = rx.try_recv().expect("the persist broadcast its events");
            if matches!(envelope.event, Event::TrackReportEdited { .. }) {
                return envelope.event;
            }
        }
    }

    async fn resolve(fx: &Fixture, event: &Event) -> (Option<u64>, Option<Vec<ReportBlockRef>>) {
        match resolve_harness_observation(&fx.repo, &fx.track_id, event)
            .await
            .unwrap()
            .expect("a report edit maps to an observation")
        {
            HarnessObservation::ReportEdited {
                doc_rev_after,
                blocks_after,
                ..
            } => (doc_rev_after, blocks_after),
            other => panic!("expected ReportEdited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolved_edit_carries_the_reads_doc_rev_and_block_refs() {
        let fx = fixture().await;
        let event = persist_and_capture(&fx, "# Goal\n\nalpha\n\n## Next\n\nbeta\n").await;

        let (doc_rev_after, blocks_after) = resolve(&fx, &event).await;

        let read = crate::track_report_read::load_report_read_snapshot(
            &fx.repo,
            "report",
            crate::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        )
        .await
        .unwrap();
        assert_eq!(doc_rev_after, Some(read.doc_rev));
        assert!(read.doc_rev >= 1, "the persist bumped docRev");
        let expected: Vec<ReportBlockRef> = read
            .blocks
            .iter()
            .map(|block| ReportBlockRef {
                id: block.id.clone(),
                rev: block.rev,
            })
            .collect();
        assert_eq!(expected.len(), 2, "two H1/H2 slices: {expected:?}");
        assert_eq!(blocks_after, Some(expected));
    }

    /// The read no longer projects to the event's body, so NO ids are attached — not the newer ones.
    #[tokio::test]
    async fn resolved_edit_omits_refs_when_a_later_write_landed() {
        let fx = fixture().await;
        let first = persist_and_capture(&fx, "# Goal\n\nalpha\n\n## Next\n\nbeta\n").await;
        let second = persist_and_capture(&fx, "# Goal\n\nalpha\n\n## Next\n\ngamma\n").await;

        assert_eq!(
            resolve(&fx, &first).await,
            (None, None),
            "the first edit's body is not what the report reads as now"
        );
        let (doc_rev_after, blocks_after) = resolve(&fx, &second).await;
        assert!(doc_rev_after.is_some(), "the newest edit still aligns");
        assert_eq!(blocks_after.map(|refs| refs.len()), Some(2));
    }
}
