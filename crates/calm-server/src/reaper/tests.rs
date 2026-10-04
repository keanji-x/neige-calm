use super::*;

use std::{path::Path, process::Command};

use crate::card_role_cache::CardRoleCache;
use calm_exec::WorkerProvider;
use calm_truth::db::RepoEventWrite;
use calm_truth::db::RepoSyncDomainRaw;
use calm_truth::db::sqlite::{
    SqlxRepo, begin_immediate_tx, session_insert_tx, status_detail_class,
};
use calm_truth::session_repo::SessionRepo;
use calm_truth_test_harness::FakeProvider;
use calm_types::ids::{CardId, TrackId};
use calm_types::worker::{
    ExitEvidence, ExitSource, LivenessTag, SessionMode, WorkerContract, WorkerProviderKind,
    WorkerSession, WorkerSessionId, WorkerSessionState,
};
use serde_json::json;

use crate::model::{
    Card, NewArea, NewCard, NewTrack, RequestTheme, Task, TaskKind, TaskStatus, new_id,
};
use crate::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
use crate::state::WriteContext;
use crate::track_area_cache::TrackAreaCache;

static REAPER_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn seeded_repo() -> (Arc<SqlxRepo>, TrackId) {
    let track_cwd = tempfile::tempdir().expect("track cwd tempdir").keep();
    init_git_repo(&track_cwd);
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = RepoSyncDomainRaw::area_create(
        repo.as_ref(),
        NewArea {
            name: "reaper-test".into(),
            color: "#000".into(),
            sort: None,
        },
    )
    .await
    .expect("seed area");
    let track = RepoSyncDomainRaw::track_create(
        repo.as_ref(),
        NewTrack {
            template_input: None,
            area_id: area.id,
            title: "reaper-test".into(),
            sort: None,
            cwd: track_cwd.display().to_string(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
    )
    .await
    .expect("seed track");
    let planner_card = RepoSyncDomainRaw::card_create(
        repo.as_ref(),
        NewCard {
            track_id: track.id.clone(),
            title: Some("planner".into()),
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        },
    )
    .await
    .expect("seed planner card");
    sqlx::query("UPDATE cards SET role = 'planner', deletable = 0 WHERE id = ?1")
        .bind(planner_card.id.as_str())
        .execute(repo.pool())
        .await
        .expect("mark seeded card as planner");
    (repo, track.id)
}

fn session(id: &str, track_id: TrackId, created_at_ms: i64) -> WorkerSession {
    WorkerSession {
        id: WorkerSessionId::from(id),
        track_id,
        provider: WorkerProviderKind::Terminal,
        mode: SessionMode::Ephemeral,
        contract: WorkerContract::Executor,
        parent_session_id: None,
        requester_session_id: None,
        state: WorkerSessionState::Running,
        mcp_token_hash: None,
        thread_id: None,
        agent_session_id: None,
        active_turn_id: None,
        terminal_run_id: None,
        card_id: Some(CardId(format!("card-{id}"))),
        handle_state_json: None,
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        last_activity_ms: None,
        last_thread_status: None,
        created_at_ms,
        updated_at_ms: created_at_ms,
        completed_at_ms: None,
    }
}

async fn insert_session(repo: &SqlxRepo, mut session: WorkerSession) -> Card {
    let card = RepoSyncDomainRaw::card_create(
        repo,
        NewCard {
            track_id: session.track_id.clone(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        },
    )
    .await
    .expect("seed runtime card");
    let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
    let session_id = session.id.clone();
    session.card_id = Some(CardId(card.id.to_string()));
    session_insert_tx(&mut tx, session)
        .await
        .expect("insert session");
    sqlx::query("UPDATE cards SET session_id = ?1 WHERE id = ?2")
        .bind(session_id.as_str())
        .bind(card.id.as_str())
        .execute(&mut *tx)
        .await
        .expect("link card session");
    tx.commit().await.expect("commit tx");
    card
}

fn exited_liveness() -> Liveness {
    Liveness::Exited {
        evidence: ExitEvidence {
            exit_code: Some(7),
            signal_killed: false,
            observed_at_ms: 123,
            source: ExitSource::Probe,
        },
    }
}

fn registry(fake: Arc<FakeProvider>) -> WorkerProviderRegistry {
    registry_for(WorkerProviderKind::Terminal, fake)
}

fn registry_for(kind: WorkerProviderKind, fake: Arc<FakeProvider>) -> WorkerProviderRegistry {
    WorkerProviderRegistry::from_entries([(kind, fake as Arc<dyn WorkerProvider>)])
}

async fn write_context(repo: &SqlxRepo) -> WriteContext {
    let role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    repo.seed_card_role_cache(&role_cache)
        .await
        .expect("seed card role cache");
    repo.seed_track_area_cache(&track_area_cache)
        .await
        .expect("seed track area cache");
    WriteContext::new(role_cache, track_area_cache)
}

async fn insert_task(repo: &SqlxRepo, track_id: &TrackId, key: &str, status: TaskStatus) -> Task {
    let now = now_ms();
    let task = Task {
        id: format!("{}:{key}", track_id.as_str()),
        track_id: track_id.as_str().to_string(),
        key: key.into(),
        kind: TaskKind::Terminal,
        goal: "test worker".into(),
        context_json: "null".into(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: "[]".into(),
        priority: 0,
        gate_json: None,
        status,
        status_detail: None,
        worker_card_id: None,
        gate_result_json: None,
        gate_attempt: 0,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: "spec".into(),
        spawn: "in-wave".into(),
        access: crate::model::TaskAccess::ReadWrite,
        start: crate::model::TaskStart::Checkout,
        created_at_ms: now,
        updated_at_ms: now,
        finished_at_ms: None,
    };
    let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
    crate::test_support::insert_task_tx(&mut tx, &task)
        .await
        .expect("insert task");
    tx.commit().await.expect("commit tx");
    task
}

async fn insert_spawn_operation(
    repo: &SqlxRepo,
    task_id: Option<&str>,
    target_card_id: Option<&str>,
) -> String {
    let op_repo = SqlxOperationRepo::new(repo.pool().clone());
    let op_id = op_repo
        .insert_operation(
            "terminal-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: task_id.map(str::to_string),
                payload_hash: format!("hash-{}", new_id()),
            },
            json!({
                "actor": ActorId::KernelDispatcher,
                "kind": "terminal-worker-test"
            }),
        )
        .await
        .expect("insert operation");
    if let Some(card_id) = target_card_id {
        sqlx::query(
            "UPDATE operations SET target_type = 'card', target_id = ?1, target_json = ?2 \
                 WHERE id = ?3",
        )
        .bind(card_id)
        .bind(json!({ "type": "card", "id": card_id }).to_string())
        .bind(&op_id)
        .execute(repo.pool())
        .await
        .expect("stamp operation target");
    }
    op_id
}

async fn acquire_test_workspace_lease(
    repo: &SqlxRepo,
    card_id: &str,
    track_id: &TrackId,
    lease_owner: &str,
) -> (String, String) {
    // The worker lease is the track worktree (#1830 S2): give the attached track one first.
    let checkout: String = sqlx::query_scalar("SELECT workspace_path FROM tracks WHERE id = ?1")
        .bind(track_id.as_str())
        .fetch_one(repo.pool())
        .await
        .expect("track checkout");
    crate::test_support::attach_track_worktree(
        repo.pool(),
        track_id.as_str(),
        Path::new(&checkout),
    )
    .await;
    let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
    let plan = crate::operation::workspace_lease::prepare_worker_lease_as_tx(
        &mut tx,
        track_id.as_str(),
        crate::model::TaskAccess::ReadWrite,
        &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
    )
    .await
    .expect("prepare the worker lease");
    let (lease, _event) = crate::operation::workspace_lease::acquire_workspace_lease_tx(
        &mut tx,
        card_id,
        track_id.as_str(),
        lease_owner,
        &plan,
    )
    .await
    .expect("acquire workspace lease");
    tx.commit().await.expect("commit lease");
    (lease.lease_id, lease.path)
}

fn init_git_repo(path: &Path) {
    std::fs::create_dir_all(path).expect("create git repo dir");
    run_git(path, ["init"]);
    run_git(path, ["config", "user.email", "reaper@example.test"]);
    run_git(path, ["config", "user.name", "Reaper Test"]);
    std::fs::write(path.join("README.md"), "initial\n").expect("write readme");
    run_git(path, ["add", "README.md"]);
    run_git(path, ["commit", "-m", "initial"]);
}

fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        args,
        repo.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout<const N: usize>(repo: &Path, args: [&str; N]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        args,
        repo.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

async fn task_failed_events(repo: &SqlxRepo, task_id: &str) -> Vec<Event> {
    RepoEventWrite::events_since(repo, 0, i64::MAX)
        .await
        .expect("events")
        .into_iter()
        .filter_map(|(_id, _version, _scope, event)| match &event {
            Event::TaskFailed {
                idempotency_key, ..
            } if idempotency_key == task_id => Some(event),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn sweep_records_non_exit_liveness_and_terminals_exited_without_spawn_op() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    for (idx, id) in ["ws-alive", "ws-idle", "ws-unknown", "ws-exited"]
        .into_iter()
        .enumerate()
    {
        insert_session(&repo, session(id, track_id.clone(), idx as i64 + 1)).await;
    }

    let fake = Arc::new(FakeProvider::new().with_probe_script([
        Liveness::Alive {
            active_turn_id: Some("turn-1".into()),
        },
        Liveness::Idle,
        Liveness::Unknown { since_ms: 99 },
        exited_liveness(),
    ]));
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry(fake.clone()),
        EventBus::new(),
        write_context(&repo).await,
    );
    let before_events = RepoEventWrite::events_since(repo.as_ref(), 0, i64::MAX)
        .await
        .expect("events before");

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(fake.probe_call_count(), 4);
    assert_eq!(
        RepoEventWrite::events_since(repo.as_ref(), 0, i64::MAX)
            .await
            .expect("events after")
            .len(),
        before_events.len(),
        "exited reaper session without spawn_op_id must not emit task events"
    );

    for (id, tag) in [
        ("ws-alive", LivenessTag::Alive),
        ("ws-idle", LivenessTag::Idle),
        ("ws-unknown", LivenessTag::Unknown),
    ] {
        let row = repo
            .session_get(&WorkerSessionId::from(id))
            .await
            .expect("session get")
            .expect("session exists");
        assert_eq!(row.liveness, tag, "{id} liveness tag");
        assert!(
            row.liveness_probed_at_ms.is_some(),
            "{id} liveness_probed_at_ms"
        );
        assert_eq!(
            row.state,
            WorkerSessionState::Running,
            "{id} state must not transition"
        );
        assert_eq!(row.exit_code, None, "{id} exit_code untouched");
        assert_eq!(
            row.exit_interpretation, None,
            "{id} exit_interpretation untouched"
        );
    }

    let exited = repo
        .session_get(&WorkerSessionId::from("ws-exited"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(exited.liveness, LivenessTag::Exited);
    assert!(exited.liveness_probed_at_ms.is_some());
    assert_eq!(exited.state, WorkerSessionState::Failed);
    assert_eq!(exited.exit_code, Some(7));
    assert_eq!(exited.exit_interpretation.as_deref(), Some("failed"));
    assert!(exited.completed_at_ms.is_some());

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn sweep_exited_failed_converges_dead_worker_task() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "dead-worker", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    let mut worker = session("ws-dead-worker", track_id.clone(), 1);
    worker.spawn_op_id = Some(op_id);
    insert_session(&repo, worker).await;

    let fake = Arc::new(FakeProvider::new().with_probe_script([exited_liveness()]));
    let events = EventBus::new();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry(fake.clone()),
        events,
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(fake.probe_call_count(), 1);
    let worker = repo
        .session_get(&WorkerSessionId::from("ws-dead-worker"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.state, WorkerSessionState::Failed);
    assert_eq!(worker.liveness, LivenessTag::Exited);
    assert_eq!(worker.exit_code, Some(7));
    assert_eq!(worker.exit_interpretation.as_deref(), Some("failed"));

    let task_row = repo
        .task_get(&task.id)
        .await
        .expect("task get")
        .expect("task exists");
    assert_eq!(task_row.status, TaskStatus::Failed);
    // The `spawn-failed` classifier is knowingly wrong for a runtime death; only the reason tail is asserted.
    let detail = task_row.status_detail.clone().unwrap_or_default();
    assert_eq!(status_detail_class(&detail), "spawn-failed");
    assert!(
        detail.contains("outcome unknown") && detail.contains("supervisor probe"),
        "status_detail must carry the reaper reason, got {detail:?}"
    );

    let failed = task_failed_events(&repo, &task.id).await;
    assert_eq!(failed.len(), 1);
    match &failed[0] {
        Event::TaskFailed {
            idempotency_key,
            reason,
            agent_message,
            ..
        } => {
            assert_eq!(idempotency_key, &task.id);
            // The probe-sourced evidence hides the exit sentinel behind 'outcome unknown'.
            assert!(
                reason.contains("outcome unknown") && reason.contains("supervisor probe"),
                "expected provider reason, got {reason:?}"
            );
            assert!(!reason.contains("exit Some("));
            assert_eq!(agent_message, &None);
        }
        other => panic!("expected task.failed, got {other:?}"),
    }

    reset_reaper_boot_gate_for_test();
}

/// A stale `last_activity_ms` lets the pre-gate through; a `Dead` verdict must converge like the ephemeral path.
#[tokio::test]
async fn sweep_resumable_codex_exited_arbiter_dead_converges() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "codex-dead", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    // `created_at_ms = 1` (NULL last_activity ⇒ created_at) is far past the deadline, so the pre-gate does not short-circuit.
    let mut worker = session("ws-codex-dead", track_id.clone(), 1);
    worker.provider = WorkerProviderKind::Codex;
    worker.mode = SessionMode::Resumable;
    worker.thread_id = Some("t-codex-dead".into());
    worker.spawn_op_id = Some(op_id);
    insert_session(&repo, worker).await;

    let fake = Arc::new(
        FakeProvider::new()
            .with_session_mode(SessionMode::Resumable)
            .with_death_verdict(DeathVerdict::Dead)
            .with_probe_script([exited_liveness()]),
    );
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry_for(WorkerProviderKind::Codex, fake.clone()),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(fake.probe_call_count(), 1);
    assert_eq!(
        fake.death_verdict_call_count(),
        1,
        "arbiter must be consulted for a stale resumable Exited"
    );

    let worker = repo
        .session_get(&WorkerSessionId::from("ws-codex-dead"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.state, WorkerSessionState::Failed);
    assert_eq!(worker.liveness, LivenessTag::Exited);
    assert_eq!(worker.exit_code, Some(7));
    assert_eq!(worker.exit_interpretation.as_deref(), Some("failed"));
    assert!(worker.completed_at_ms.is_some());

    let task_row = repo
        .task_get(&task.id)
        .await
        .expect("task get")
        .expect("task exists");
    assert_eq!(task_row.status, TaskStatus::Failed);
    let detail = task_row.status_detail.clone().unwrap_or_default();
    assert_eq!(status_detail_class(&detail), "spawn-failed");
    let event_reason = match &task_failed_events(&repo, &task.id).await[0] {
        Event::TaskFailed { reason, .. } => reason.clone(),
        other => panic!("expected task.failed, got {other:?}"),
    };
    assert!(
        detail.ends_with(&event_reason),
        "row detail {detail:?} must carry the event reason {event_reason:?}"
    );

    let failed = task_failed_events(&repo, &task.id).await;
    assert_eq!(failed.len(), 1);

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn sweep_resumable_codex_dead_worker_releases_same_boot_workspace_lease() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "codex-lease-dead", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    let mut worker = session("ws-codex-lease-dead", track_id.clone(), 1);
    worker.provider = WorkerProviderKind::Codex;
    worker.mode = SessionMode::Resumable;
    worker.thread_id = Some("t-codex-lease-dead".into());
    worker.spawn_op_id = Some(op_id.clone());
    let card = insert_session(&repo, worker).await;
    let (lease_id, lease_path) =
        acquire_test_workspace_lease(&repo, card.id.as_str(), &track_id, &op_id).await;
    assert!(
        std::path::Path::new(&lease_path).is_dir(),
        "leased cwd exists before reaping"
    );

    let fake = Arc::new(
        FakeProvider::new()
            .with_session_mode(SessionMode::Resumable)
            .with_death_verdict(DeathVerdict::Dead)
            .with_probe_script([exited_liveness()]),
    );
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry_for(WorkerProviderKind::Codex, fake),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(repo.pool())
            .await
            .expect("lease state");
    assert_eq!(state, "released");
    assert!(
        std::path::Path::new(&lease_path).is_dir(),
        "reaper release preserves leased cwd"
    );
    assert_eq!(
        git_stdout(
            std::path::Path::new(&lease_path),
            ["rev-parse", "--abbrev-ref", "HEAD"]
        )
        .trim(),
        format!("neige/track-{}", track_id.as_str()),
        "reaper release preserves the track branch"
    );
    // #1830 S2 D7: the reaper's fail transaction commits the attempt as `failed`.
    let outcome: String = sqlx::query_scalar(
        "SELECT outcome FROM task_git_deliveries WHERE producer_attempt_id = ?1",
    )
    .bind(&task.id)
    .fetch_one(repo.pool())
    .await
    .expect("the dead attempt's delivery row");
    assert_eq!(outcome, "failed");
    let released_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
            .fetch_one(repo.pool())
            .await
            .expect("released event count");
    assert_eq!(released_events, 1);
    let removed_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'worktree.removed'")
            .fetch_one(repo.pool())
            .await
            .expect("removed event count");
    assert_eq!(removed_events, 0);

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn converge_dead_worker_without_spawn_op_releases_workspace_lease() {
    let _guard = REAPER_TEST_LOCK.lock().await;

    let (repo, track_id) = seeded_repo().await;
    let mut worker = session("ws-codex-no-spawn-op", track_id.clone(), 1);
    worker.provider = WorkerProviderKind::Codex;
    worker.mode = SessionMode::Resumable;
    worker.thread_id = Some("t-codex-no-spawn-op".into());
    let card = insert_session(&repo, worker.clone()).await;
    worker.card_id = Some(CardId(card.id.to_string()));
    let (lease_id, lease_path) =
        acquire_test_workspace_lease(&repo, card.id.as_str(), &track_id, "missing-spawn-op").await;
    assert!(
        std::path::Path::new(&lease_path).is_dir(),
        "leased cwd exists before converge guard"
    );

    let events = EventBus::new();
    let write = write_context(&repo).await;
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    converge_dead_worker(repo_dyn.as_ref(), &events, &write, &worker, "dead")
        .await
        .expect("converge dead worker");

    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(repo.pool())
            .await
            .expect("lease state");
    assert_eq!(state, "released");
    assert!(
        std::path::Path::new(&lease_path).is_dir(),
        "spawn_op_id guard release preserves leased cwd"
    );
    assert_eq!(
        git_stdout(
            std::path::Path::new(&lease_path),
            ["rev-parse", "--abbrev-ref", "HEAD"]
        )
        .trim(),
        format!("neige/track-{}", track_id.as_str()),
        "spawn_op_id guard release preserves the track branch"
    );
    let released_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
            .fetch_one(repo.pool())
            .await
            .expect("released event count");
    assert_eq!(released_events, 1);
    let removed_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'worktree.removed'")
            .fetch_one(repo.pool())
            .await
            .expect("removed event count");
    assert_eq!(removed_events, 0);
}

#[tokio::test]
async fn sweep_resumable_codex_exited_arbiter_alive_records_t2_only() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "codex-alive", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    let mut worker = session("ws-codex-alive", track_id.clone(), 1);
    worker.provider = WorkerProviderKind::Codex;
    worker.mode = SessionMode::Resumable;
    worker.thread_id = Some("t-codex-alive".into());
    worker.spawn_op_id = Some(op_id);
    insert_session(&repo, worker).await;

    let fake = Arc::new(
        FakeProvider::new()
            .with_session_mode(SessionMode::Resumable)
            .with_death_verdict(DeathVerdict::Alive)
            .with_probe_script([exited_liveness()]),
    );
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry_for(WorkerProviderKind::Codex, fake.clone()),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(fake.probe_call_count(), 1);
    assert_eq!(fake.death_verdict_call_count(), 1);

    let worker = repo
        .session_get(&WorkerSessionId::from("ws-codex-alive"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.liveness, LivenessTag::Exited);
    assert!(worker.liveness_probed_at_ms.is_some());
    assert_eq!(
        worker.state,
        WorkerSessionState::Running,
        "arbiter Alive must NOT terminalize the session"
    );
    assert_eq!(worker.exit_code, None);
    assert_eq!(worker.exit_interpretation, None);
    assert!(worker.completed_at_ms.is_none());

    assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);
    let task_row = repo
        .task_get(&task.id)
        .await
        .expect("task get")
        .expect("task exists");
    assert_eq!(task_row.status, TaskStatus::Running);

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn sweep_resumable_codex_exited_arbiter_unknown_records_t2_only() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "codex-unknown", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    let mut worker = session("ws-codex-unknown", track_id.clone(), 1);
    worker.provider = WorkerProviderKind::Codex;
    worker.mode = SessionMode::Resumable;
    worker.thread_id = Some("t-codex-unknown".into());
    worker.spawn_op_id = Some(op_id);
    insert_session(&repo, worker).await;

    let fake = Arc::new(
        FakeProvider::new()
            .with_session_mode(SessionMode::Resumable)
            .with_death_verdict(DeathVerdict::Unknown)
            .with_probe_script([exited_liveness()]),
    );
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry_for(WorkerProviderKind::Codex, fake.clone()),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(fake.probe_call_count(), 1);
    assert_eq!(fake.death_verdict_call_count(), 1);

    let worker = repo
        .session_get(&WorkerSessionId::from("ws-codex-unknown"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.liveness, LivenessTag::Exited);
    assert!(worker.liveness_probed_at_ms.is_some());
    assert_eq!(
        worker.state,
        WorkerSessionState::Running,
        "arbiter Unknown must NOT terminalize the session"
    );
    assert_eq!(worker.exit_code, None);
    assert_eq!(worker.exit_interpretation, None);
    assert!(worker.completed_at_ms.is_none());

    assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);

    reset_reaper_boot_gate_for_test();
}

/// Arbiter would say `Dead`, but the pre-gate never asks it.
#[tokio::test]
async fn sweep_resumable_codex_exited_recent_activity_pregate_skips_arbiter() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "codex-recent", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    let mut worker = session("ws-codex-recent", track_id.clone(), 1);
    worker.provider = WorkerProviderKind::Codex;
    worker.mode = SessionMode::Resumable;
    worker.thread_id = Some("t-codex-recent".into());
    worker.last_activity_ms = Some(now_ms());
    worker.spawn_op_id = Some(op_id);
    insert_session(&repo, worker).await;

    let fake = Arc::new(
        FakeProvider::new()
            .with_session_mode(SessionMode::Resumable)
            // Arbiter WOULD reap, proving the pre-gate is what holds it.
            .with_death_verdict(DeathVerdict::Dead)
            .with_probe_script([exited_liveness()]),
    );
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry_for(WorkerProviderKind::Codex, fake.clone()),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(fake.probe_call_count(), 1);
    assert_eq!(
        fake.death_verdict_call_count(),
        0,
        "recent-activity pre-gate must short-circuit WITHOUT consulting the arbiter"
    );

    let worker = repo
        .session_get(&WorkerSessionId::from("ws-codex-recent"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.liveness, LivenessTag::Exited);
    assert!(worker.liveness_probed_at_ms.is_some());
    assert_eq!(
        worker.state,
        WorkerSessionState::Running,
        "recent-activity pre-gate must NOT terminalize the session"
    );
    assert_eq!(worker.exit_code, None);
    assert_eq!(worker.exit_interpretation, None);

    assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);

    reset_reaper_boot_gate_for_test();
}

/// A supervisor `proc_running:false` in the spawn window means 'not registered YET', not 'exited': liveness is recorded and the spawn operation owns convergence.
#[tokio::test]
async fn sweep_exited_starting_session_records_liveness_without_convergence() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "spawn-window", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    let mut worker = session("ws-starting", track_id.clone(), 1);
    worker.state = WorkerSessionState::Starting;
    worker.spawn_op_id = Some(op_id);
    insert_session(&repo, worker).await;

    let fake = Arc::new(FakeProvider::new().with_probe_script([exited_liveness()]));
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry(fake.clone()),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(fake.probe_call_count(), 1);
    let worker = repo
        .session_get(&WorkerSessionId::from("ws-starting"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.liveness, LivenessTag::Exited);
    assert!(worker.liveness_probed_at_ms.is_some());
    assert_eq!(
        worker.state,
        WorkerSessionState::Starting,
        "spawn-window Exited must NOT terminalize a `starting` session"
    );
    assert_eq!(worker.exit_code, None, "no exit committed in spawn window");
    assert_eq!(worker.exit_interpretation, None);
    assert!(worker.completed_at_ms.is_none());

    assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);
    let task_row = repo
        .task_get(&task.id)
        .await
        .expect("task get")
        .expect("task exists");
    assert_eq!(task_row.status, TaskStatus::Running);

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn sweep_exited_with_null_spawn_op_task_key_terminalizes_without_task_failed() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "null-op-key", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, None, None).await;
    let mut worker = session("ws-null-op-key", track_id.clone(), 1);
    worker.spawn_op_id = Some(op_id);
    insert_session(&repo, worker).await;

    let fake = Arc::new(FakeProvider::new().with_probe_script([exited_liveness()]));
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry(fake),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    let worker = repo
        .session_get(&WorkerSessionId::from("ws-null-op-key"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.state, WorkerSessionState::Failed);
    assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn sweep_exited_race_lost_after_live_terminal_completion_emits_no_second_event() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "race", TaskStatus::Running).await;
    let mut worker = session("ws-race", track_id.clone(), 1);
    let worker_card = insert_session(&repo, worker.clone()).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), Some(worker_card.id.as_str())).await;
    worker.spawn_op_id = Some(op_id);
    sqlx::query("UPDATE worker_sessions SET spawn_op_id = ?1 WHERE id = ?2")
        .bind(worker.spawn_op_id.as_deref())
        .bind(worker.id.as_str())
        .execute(repo.pool())
        .await
        .expect("stamp session spawn op");

    let events = EventBus::new();
    let write = write_context(&repo).await;
    crate::scheduler::complete_terminal_task(
        repo.as_ref(),
        &events,
        &write,
        &task.id,
        track_id.as_str(),
        worker_card.id.as_str(),
        Some(0),
        false,
        "",
        false,
    )
    .await
    .expect("live terminal completion");

    let fake = Arc::new(FakeProvider::new().with_probe_script([exited_liveness()]));
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(repo_dyn, registry(fake), events, write);

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);
    let task_row = repo
        .task_get(&task.id)
        .await
        .expect("task get")
        .expect("task exists");
    assert_eq!(task_row.status, TaskStatus::Done);
    let completed = RepoEventWrite::events_since(repo.as_ref(), 0, i64::MAX)
        .await
        .expect("events")
        .into_iter()
        .filter(|(_id, _version, _scope, event)| {
            matches!(event, Event::TaskCompleted { idempotency_key, .. } if idempotency_key == &task.id)
        })
        .count();
    assert_eq!(completed, 1);
    let worker = repo
        .session_get(&WorkerSessionId::from("ws-race"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.state, WorkerSessionState::Failed);

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn sweep_unknown_liveness_records_t2_without_death_convergence() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "unknown", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    let mut worker = session("ws-unknown-death", track_id.clone(), 1);
    worker.spawn_op_id = Some(op_id);
    insert_session(&repo, worker).await;

    let fake =
        Arc::new(FakeProvider::new().with_probe_script([Liveness::Unknown { since_ms: 55 }]));
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry(fake),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    let worker = repo
        .session_get(&WorkerSessionId::from("ws-unknown-death"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.state, WorkerSessionState::Running);
    assert_eq!(worker.liveness, LivenessTag::Unknown);
    assert!(worker.liveness_probed_at_ms.is_some());
    let task_row = repo
        .task_get(&task.id)
        .await
        .expect("task get")
        .expect("task exists");
    assert_eq!(task_row.status, TaskStatus::Running);
    assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn sweep_noops_until_reaper_on_boot_opens_gate() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    insert_session(&repo, session("ws-gated", track_id, 1)).await;
    let fake = Arc::new(FakeProvider::new().with_probe_script([Liveness::Alive {
        active_turn_id: None,
    }]));
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry(fake.clone()),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper.sweep_all().await;
    assert_eq!(fake.probe_call_count(), 0);
    let before = repo
        .session_get(&WorkerSessionId::from("ws-gated"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(before.liveness, LivenessTag::Unknown);
    assert_eq!(before.liveness_probed_at_ms, None);

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(fake.probe_call_count(), 1);
    let after = repo
        .session_get(&WorkerSessionId::from("ws-gated"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(after.liveness, LivenessTag::Alive);
    assert_eq!(after.state, WorkerSessionState::Running);
    assert!(after.liveness_probed_at_ms.is_some());

    reset_reaper_boot_gate_for_test();
}
