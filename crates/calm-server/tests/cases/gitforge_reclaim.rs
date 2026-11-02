//! Actual plugin lowering/execution/receipts, close tick and HTTP deletion; only fresh resources.
use super::*;
use calm_server::test_seams::{
    clean_closed_worktrees_with_history_for_test, count_workspace_observation_for_test,
    gitforge_worktree_add_for_test,
};
use std::{
    collections::HashSet,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Fixture {
    b: Boot,
    up: Upstream,
    track: String,
    kernel: PathBuf,
    clean: PathBuf,
    dirty: PathBuf,
    unrecorded: PathBuf,
    dirty_attempts: Arc<AtomicUsize>,
    clean_attempts: Arc<AtomicUsize>,
    _execution: crate::support::execution::Fixture,
}
impl Fixture {
    async fn new() -> Self {
        let mut b = boot().await;
        let execution = crate::support::execution::Fixture::new(b.repo.db().clone()).await;
        b.state = state_for_reclaim(&b, &execution.socket);
        b.app = crate::support::authenticated_router::application(b.state.clone()).await;
        b.state
            .operation_runtime
            .execution()
            .bind(Arc::new(execution.lifecycle.engine().unwrap().clone()));
        let up = upstream(b.tmp.path());
        let (status, body) = b.create_at(&up.clone, None, None).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let track = body["id"].as_str().unwrap().to_string();
        b.shutdown_harnesses().await;
        let card: String =
            sqlx::query_scalar("SELECT id FROM cards WHERE track_id=? AND role='planner'")
                .bind(&track)
                .fetch_one(b.repo.pool())
                .await
                .unwrap();
        let kernel = expected_worktree(&up.clone, &track);
        let clean = b.tmp.path().join("clean-extra");
        let dirty = b.tmp.path().join("dirty-extra");
        let unrecorded = b.tmp.path().join("unrecorded-extra");
        for (path, branch) in [(&clean, "extra-clean"), (&dirty, "extra-dirty")] {
            gitforge_worktree_add_for_test(
                &b.state.operation_runtime,
                &b.tmp.path().join("forge-logs"),
                &track,
                &card,
                &kernel,
                path,
                branch,
            )
            .await
            .unwrap();
        }
        run_git(
            &up.clone,
            [
                "worktree",
                "add",
                "-qb",
                "unrecorded",
                unrecorded.to_str().unwrap(),
            ],
        );
        std::fs::write(dirty.join("untracked"), "precious user work").unwrap();
        let dirty_attempts =
            count_workspace_observation_for_test("extra-worktree-remove-attempt", &dirty);
        let clean_attempts =
            count_workspace_observation_for_test("extra-worktree-remove-attempt", &clean);
        Self {
            b,
            up,
            track,
            kernel,
            clean,
            dirty,
            unrecorded,
            dirty_attempts,
            clean_attempts,
            _execution: execution,
        }
    }
    async fn tick(&self, history: &mut HashSet<(String, i64)>) {
        clean_closed_worktrees_with_history_for_test(
            self.b.repo.db(),
            &self.b.state.operation_runtime,
            history,
        )
        .await
        .unwrap();
    }
    async fn close(&self) {
        let (status, body) = self
            .b
            .as_user(
                "PATCH",
                &format!("/api/tracks/{}", self.track),
                json!({"closed":true}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    async fn assert_retired(&self) {
        assert_eq!(self.clean_attempts.load(Ordering::SeqCst), 0);
        assert_eq!(self.dirty_attempts.load(Ordering::SeqCst), 0);
        assert!(self.clean.exists());
        assert_eq!(
            std::fs::read_to_string(self.dirty.join("untracked")).unwrap(),
            "precious user work"
        );
        assert!(self.unrecorded.exists());
        assert!(git_ref_exists(&self.up.clone, "refs/heads/extra-clean"));
        let removed: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM events WHERE kind='worktree.removed' AND scope_track=?",
        )
        .bind(&self.track)
        .fetch_one(self.b.repo.pool())
        .await
        .unwrap();
        assert_eq!(removed, 0);
        let retired: Vec<String> = sqlx::query_scalar("SELECT root_path FROM workspace_resources WHERE source_track_id=? AND retired_at_ms IS NOT NULL")
            .bind(&self.track).fetch_all(self.b.repo.pool()).await.unwrap();
        assert!(retired.iter().any(|path| Path::new(path) == self.clean));
        assert!(retired.iter().any(|path| Path::new(path) == self.dirty));
    }
}

#[tokio::test]
async fn retirement_close_retains_clean_and_dirty_worktrees_without_preflight() {
    let f = Fixture::new().await;
    f.close().await;
    f.assert_retired().await;
    let mut history = HashSet::new();
    f.tick(&mut history).await;
    std::fs::remove_file(f.dirty.join("untracked")).unwrap();
    f.tick(&mut history).await;
    assert!(
        f.dirty.exists(),
        "becoming clean does not establish isolation"
    );
    assert_eq!(f.clean_attempts.load(Ordering::SeqCst), 0);
    assert_eq!(f.dirty_attempts.load(Ordering::SeqCst), 0);
    assert!(f.kernel.exists());
}

#[tokio::test]
async fn retirement_track_delete_preserves_work_and_refs() {
    let f = Fixture::new().await;
    let (status, body) = f.b.delete_track(&f.track).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    f.assert_retired().await;
    assert!(f.kernel.exists());
    assert!(git_ref_exists(
        &f.up.clone,
        &format!("refs/heads/neige/track-{}", f.track)
    ));
}

#[tokio::test]
async fn retirement_area_delete_preserves_work_and_refs() {
    let f = Fixture::new().await;
    let (status, body) =
        f.b.as_user(
            "DELETE",
            &format!("/api/areas/{}", f.b.area_id),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    f.assert_retired().await;
    assert!(f.kernel.exists());
}

async fn recover_reclaims(f: &Fixture) -> Arc<calm_server::operation::OperationRuntime> {
    // Fresh production adapter assembly and execution recovery share only the durable DB and
    // the supervisor journal with the original invocation, not its runtime/reservation state.
    let engine = f._execution.new_kernel(f.b.repo.db().clone(), false).await;
    assert!(engine.recover().await.error.is_none());
    let state = state_for_reclaim(&f.b, &f._execution.socket);
    state.operation_runtime.execution().bind(Arc::new(engine));
    let runtime = state.operation_runtime.clone();
    let plan = runtime.recover_on_boot().await.unwrap();
    runtime.apply_recovery(plan).await.unwrap();
    runtime
}

fn state_for_reclaim(b: &Boot, socket: &Path) -> AppState {
    AppState::from_parts(
        b.repo.clone(),
        b.state.events.clone(),
        Arc::new(calm_server::state::DaemonClient {
            data_dir: b.state.daemon.data_dir.clone(),
            proc_supervisor_sock: Some(socket.to_owned()),
        }),
        b.state.plugin.clone(),
        b.state.codex.clone(),
        Some(b.state.card_role_cache.clone()),
        Some(b.state.track_area_cache.clone()),
    )
    .with_workspace_root(b.tmp.path().join("workspaces"))
    .with_shared_codex_appserver(b.state.shared_codex_appserver.clone())
}

#[tokio::test]
async fn c5_gitforge_reclaim_foreign_execution_retains_extra_on_close() {
    foreign_execution_retains_extra(false).await;
}

#[tokio::test]
async fn c5_gitforge_reclaim_foreign_execution_retains_extra_on_delete() {
    foreign_execution_retains_extra(true).await;
}

async fn foreign_execution_retains_extra(delete: bool) {
    use workspace_runtime::execution::TrackId;
    {
        let f = Fixture::new().await;
        let mut intent = crate::support::execution::occupancy_intent(f.clean.clone());
        intent.metadata.consumer = "gate".into();
        intent.metadata.track = TrackId("foreign-owner".into());
        let id = intent.metadata.id.clone();
        let engine = f._execution.lifecycle.engine().unwrap();
        let ticket = {
            let write_db = (f.b.repo.db()).clone();
            let engine = &engine;
            write_db
                .writer()
                .with_writer(|write_session| {
                    Box::pin(async move {
                        let mut tx = write_session.begin_immediate().await.unwrap();
                        let ticket = engine.admit(&mut tx, intent).await.unwrap();
                        tx.commit().await.unwrap();
                        ticket
                    })
                })
                .await
                .expect("independent write operation")
        };
        engine.drive(ticket.committed());
        if delete {
            assert_foreign_occupancy(&f, &id).await;
            let (status, body) = f.b.delete_track(&f.track).await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        } else {
            f.close().await;
            assert_foreign_occupancy(&f, &id).await;
            f.tick(&mut HashSet::new()).await;
        }
        assert!(f.clean.exists(), "occupied extra worktree must be retained");
        let desired: String =
            sqlx::query_scalar("SELECT desired FROM workspace_executions WHERE execution_id=?")
                .bind(id.as_str())
                .fetch_one(f.b.repo.pool())
                .await
                .unwrap();
        assert_eq!(desired, "run", "foreign occupancy must not be stopped");
    }
}

async fn assert_foreign_occupancy(f: &Fixture, id: &workspace_runtime::execution::ExecutionId) {
    use calm_truth::db::sqlite::workspace_execution::{Scope, location_occupancy};
    let occupants = location_occupancy(
        &mut f.b.repo.pool().acquire().await.unwrap(),
        Scope::Track(&f.track),
        std::slice::from_ref(&f.clean),
    )
    .await
    .unwrap();
    assert!(
        occupants.contains(id),
        "foreign execution must still occupy the extra worktree before reclaim"
    );
}

#[path = "gitforge_reclaim_admission.rs"]
mod admission;

#[path = "workspace_retirement.rs"]
mod retirement;
