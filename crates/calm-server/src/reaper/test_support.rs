use super::*;
use calm_truth::db::RepoRead;
use std::{path::Path, process::Command};

use crate::card_role_cache::CardRoleCache;
use calm_exec::WorkerProvider;
use calm_truth::db::RepoEventWrite;
use calm_truth::db::RepoSyncDomainRaw;
use calm_truth::db::sqlite::{SqlxRepo, begin_immediate_tx, session_insert_tx};
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

pub(super) static REAPER_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(super) async fn seeded_repo() -> (Arc<SqlxRepo>, TrackId) {
    seeded_repo_with_plan(|_| crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd).await
}

pub(super) async fn seeded_repo_with_plan(
    plan: impl FnOnce(&Path) -> crate::db::sqlite::TrackWorkspacePlan,
) -> (Arc<SqlxRepo>, TrackId) {
    let track_cwd = tempfile::tempdir().expect("track cwd tempdir").keep();
    init_git_repo(&track_cwd);
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = RepoSyncDomainRaw::area_create(
        repo.as_ref(),
        calm_types::organization::LEGACY_OWNER_USER_ID,
        NewArea {
            name: "reaper-test".into(),
            color: "#000".into(),
            sort: None,
        },
    )
    .await
    .expect("seed area");
    let track = crate::test_seams::create_track_with_workspace_for_test(
        repo.db(),
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
        &plan(&track_cwd),
        &crate::track_area_cache::TrackAreaCache::new(),
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

pub(super) fn session(id: &str, track_id: TrackId, created_at_ms: i64) -> WorkerSession {
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

pub(super) async fn insert_session(repo: &SqlxRepo, mut session: WorkerSession) -> Card {
    let card = RepoSyncDomainRaw::card_create(
        repo,
        NewCard {
            track_id: session.track_id.clone(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({"terminal_presentation": {"source": "unrecorded", "in_workbench": false}}),
        },
    )
    .await
    .expect("seed runtime card");
    let mut tx = begin_immediate_tx(repo.db()).await.expect("begin tx");
    let session_id = session.id.clone();
    session.card_id = Some(CardId(card.id.to_string()));
    session_insert_tx(&mut tx, session)
        .await
        .expect("insert session");
    sqlx::query("UPDATE cards SET session_id = ?1 WHERE id = ?2")
        .bind(session_id.as_str())
        .bind(card.id.as_str())
        .execute(&mut **tx)
        .await
        .expect("link card session");
    tx.commit().await.expect("commit tx");
    card
}

pub(super) fn exited_liveness() -> Liveness {
    Liveness::Exited {
        evidence: ExitEvidence {
            exit_code: Some(7),
            signal_killed: false,
            observed_at_ms: 123,
            source: ExitSource::Probe,
        },
    }
}

pub(super) fn registry(fake: Arc<FakeProvider>) -> WorkerProviderRegistry {
    registry_for(WorkerProviderKind::Terminal, fake)
}

pub(super) fn registry_for(
    kind: WorkerProviderKind,
    fake: Arc<FakeProvider>,
) -> WorkerProviderRegistry {
    WorkerProviderRegistry::from_entries([(kind, fake as Arc<dyn WorkerProvider>)])
}

pub(super) async fn write_context(repo: &SqlxRepo) -> WriteContext {
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

pub(super) async fn insert_task(
    repo: &SqlxRepo,
    track_id: &TrackId,
    key: &str,
    status: TaskStatus,
) -> Task {
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
        result_contract: None,
        child_track_json: None,
        created_at_ms: now,
        updated_at_ms: now,
        finished_at_ms: None,
    };
    let mut tx = begin_immediate_tx(repo.db()).await.expect("begin tx");
    crate::test_support::insert_task_tx(&mut tx, &task)
        .await
        .expect("insert task");
    tx.commit().await.expect("commit tx");
    task
}

pub(super) async fn insert_spawn_operation(
    repo: &SqlxRepo,
    task_id: Option<&str>,
    target_card_id: Option<&str>,
) -> String {
    let op_repo = SqlxOperationRepo::new(repo.db().clone());
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

pub(super) async fn acquire_test_workspace_lease(
    repo: &SqlxRepo,
    card_id: &str,
    track_id: &TrackId,
    lease_owner: &str,
) -> (String, String) {
    acquire_test_workspace_lease_as(
        repo,
        card_id,
        track_id,
        lease_owner,
        crate::model::TaskAccess::ReadWrite,
    )
    .await
}

pub(super) async fn acquire_test_workspace_lease_as(
    repo: &SqlxRepo,
    card_id: &str,
    track_id: &TrackId,
    lease_owner: &str,
    access: crate::model::TaskAccess,
) -> (String, String) {
    crate::test_support::ensure_track_worktree(repo.db(), track_id.as_str()).await;
    let mut tx = begin_immediate_tx(repo.db()).await.expect("begin tx");
    let plan = crate::operation::workspace_lease::prepare_worker_lease_as_tx(
        &mut tx,
        track_id.as_str(),
        access,
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

pub(super) fn init_git_repo(path: &Path) {
    std::fs::create_dir_all(path).expect("create git repo dir");
    run_git(path, ["init"]);
    run_git(path, ["config", "user.email", "reaper@example.test"]);
    run_git(path, ["config", "user.name", "Reaper Test"]);
    std::fs::write(path.join("README.md"), "initial\n").expect("write readme");
    run_git(path, ["add", "README.md"]);
    run_git(path, ["commit", "-m", "initial"]);
}

pub(super) fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
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

pub(super) fn git_stdout<const N: usize>(repo: &Path, args: [&str; N]) -> String {
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

pub(super) async fn task_failed_events(repo: &SqlxRepo, task_id: &str) -> Vec<Event> {
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
