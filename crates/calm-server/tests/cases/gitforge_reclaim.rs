//! Actual plugin lowering/execution/receipts, close tick and HTTP deletion; only fresh resources.
use super::*;
use calm_server::db::RepoOutOfDomain;
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
    async fn assert_reclaimed(&self) {
        // This assertion uniquely kills removal without dirty preflight, even when Git refuses.
        assert_eq!(
            self.dirty_attempts.load(Ordering::SeqCst),
            0,
            "dirty removal must have zero Git attempts"
        );
        assert_eq!(self.clean_attempts.load(Ordering::SeqCst), 1);
        assert!(!self.clean.exists());
        assert_eq!(
            std::fs::read_to_string(self.dirty.join("untracked")).unwrap(),
            "precious user work"
        );
        assert!(self.unrecorded.exists());
        assert!(git_ref_exists(&self.up.clone, "refs/heads/extra-clean"));
        let paths:Vec<String>=sqlx::query_scalar("SELECT json_extract(payload,'$.path') FROM events WHERE kind='worktree.removed' AND scope_track=?")
            .bind(&self.track).fetch_all(self.b.repo.pool()).await.unwrap();
        assert_eq!(paths, vec![self.clean.to_str().unwrap().to_string()]);
    }
}

#[tokio::test]
async fn gitforge_reclaim_closed_dirty_preflight() {
    let f = Fixture::new().await;
    f.close().await;
    let mut history = HashSet::new();
    f.tick(&mut history).await;
    f.assert_reclaimed().await;
    assert!(f.kernel.exists());
    assert!(git_ref_exists(
        &f.up.clone,
        &format!("refs/heads/neige/track-{}", f.track)
    ));
    std::fs::remove_file(f.dirty.join("untracked")).unwrap();
    f.tick(&mut history).await;
    assert!(
        !f.dirty.exists(),
        "dirty retention retries even with same ignored-clean history"
    );
    assert!(f.kernel.exists());
}

#[tokio::test]
async fn gitforge_reclaim_track_delete_dirty_preflight() {
    let f = Fixture::new().await;
    let (status, body) = f.b.delete_track(&f.track).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    f.assert_reclaimed().await;
    assert!(!f.kernel.exists());
}

#[tokio::test]
async fn gitforge_reclaim_area_delete_dirty_preflight() {
    let f = Fixture::new().await;
    let (status, body) =
        f.b.as_user(
            "DELETE",
            &format!("/api/areas/{}", f.b.area_id),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    f.assert_reclaimed().await;
}

#[tokio::test]
async fn gitforge_reclaim_nested_claim_blocks_ancestor_close_and_delete() {
    let f = Fixture::new().await;
    let nested = f.kernel.join("nested-extra");
    let card: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=? AND role='planner'")
            .bind(&f.track)
            .fetch_one(f.b.repo.pool())
            .await
            .unwrap();
    gitforge_worktree_add_for_test(
        &f.b.state.operation_runtime,
        &f.b.tmp.path().join("logs"),
        &f.track,
        &card,
        &f.kernel,
        &nested,
        "nested",
    )
    .await
    .unwrap();
    std::fs::write(nested.join("precious"), "nested work").unwrap();
    f.close().await;
    f.tick(&mut HashSet::new()).await;
    assert!(nested.join("precious").exists());
    assert_eq!(f.b.delete_track(&f.track).await.0, StatusCode::CONFLICT);
    assert_eq!(
        f.b.as_user(
            "DELETE",
            &format!("/api/areas/{}", f.b.area_id),
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(nested.join("precious").exists());
    assert!(f.kernel.exists());
}

#[tokio::test]
async fn gitforge_reclaim_unknown_global_terminal_retains_then_retries() {
    use calm_server::model::{NewCard, NewTerminal, NewTrack};
    let f = Fixture::new().await;
    // A different track's terminal can cd anywhere; launch cwd is not current-cwd authority.
    let other =
        f.b.repo
            .track_create(NewTrack {
                template_input: None,
                area_id: f.b.area_id.clone().into(),
                title: "terminal owner".into(),
                sort: None,
                cwd: f.b.tmp.path().to_str().unwrap().into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
    let card = f
        .b
        .repo
        .card_create(NewCard {
            track_id: other.id,
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({"terminal_presentation":{"source":"unrecorded","in_workbench":false}}),
        })
        .await
        .unwrap();
    let terminal =
        f.b.repo
            .terminal_create(NewTerminal {
                card_id: card.id,
                program: "/bin/sh".into(),
                cwd: f.b.tmp.path().to_str().unwrap().into(),
                env: json!({}),
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
    f.close().await;
    let mut history = HashSet::new();
    f.tick(&mut history).await;
    assert!(f.clean.exists());
    assert!(f.dirty.exists());
    assert_eq!(f.clean_attempts.load(Ordering::SeqCst), 0);
    assert_eq!(f.dirty_attempts.load(Ordering::SeqCst), 0);
    f.b.repo
        .terminal_set_exit(terminal.id.as_str(), Some(0), false)
        .await
        .unwrap();
    f.tick(&mut history).await;
    f.assert_reclaimed().await;
}

#[tokio::test]
async fn gitforge_reclaim_delete_excludes_kernel_ref_from_head_protection() {
    let f = Fixture::new().await;
    run_git(&f.clean, ["checkout", "--detach", "-q"]);
    std::fs::write(f.clean.join("only-extra"), "new work").unwrap();
    run_git(&f.clean, ["add", "only-extra"]);
    run_git(&f.clean, ["commit", "-qm", "only extra"]);
    let head = git_stdout(&f.clean, ["rev-parse", "HEAD"]);
    run_git(
        &f.up.clone,
        [
            "update-ref",
            &format!("refs/heads/neige/track-{}", f.track),
            &head,
        ],
    );
    let (status, body) = f.b.delete_track(&f.track).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(
        f.clean.exists(),
        "a ref deleted by this sweep cannot protect extra HEAD"
    );
    assert_eq!(f.clean_attempts.load(Ordering::SeqCst), 0);
    assert!(f.clean.join("only-extra").exists());
}

#[tokio::test]
async fn gitforge_reclaim_close_only_reclaims_creating_track() {
    let f = Fixture::new().await;
    std::fs::remove_file(f.dirty.join("untracked")).unwrap();
    let (status, body) = f.b.create_at(&f.up.clone, None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let other = body["id"].as_str().unwrap();
    f.b.shutdown_harnesses().await;
    let card: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=? AND role='planner'")
            .bind(other)
            .fetch_one(f.b.repo.pool())
            .await
            .unwrap();
    let target = f.b.tmp.path().join("other-track-extra");
    gitforge_worktree_add_for_test(
        &f.b.state.operation_runtime,
        &f.b.tmp.path().join("logs"),
        other,
        &card,
        &expected_worktree(&f.up.clone, other),
        &target,
        "other-extra",
    )
    .await
    .unwrap();
    f.close().await;
    f.tick(&mut HashSet::new()).await;
    assert!(!f.clean.exists());
    assert!(!f.dirty.exists());
    assert!(target.exists());
    assert!(f.unrecorded.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gitforge_reclaim_fences_cross_track_terminal_admission() {
    cross_track_terminal_admission(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gitforge_reclaim_fences_cross_track_typed_terminal_admission() {
    cross_track_terminal_admission(true).await;
}

async fn cross_track_terminal_admission(typed: bool) {
    use calm_server::test_seams::{PausePoint, install_pause_for_test};
    let mut f = Fixture::new().await;
    f.b.state = f.b.state.clone().with_terminal_execution(typed);
    f.b.app = crate::support::authenticated_router::application(f.b.state.clone()).await;
    let f = Arc::new(f);
    let (status, other) = f.b.create_at(&f.up.clone, None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
    f.b.shutdown_harnesses().await;
    f.close().await;
    let pause = PausePoint::default();
    install_pause_for_test(
        "extra-worktree-remove-attempt",
        f.clean.to_str().unwrap(),
        pause.clone(),
    );
    let tick = tokio::spawn({
        let f = f.clone();
        async move { f.tick(&mut HashSet::new()).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(15), pause.entered.notified())
        .await
        .unwrap();
    // Exercise the actual HTTP admission, with the other owner's explicit external cwd.
    let (status, body) = f.b.as_user("POST", &format!("/api/tracks/{}/terminal-cards", other["id"].as_str().unwrap()),
        json!({"cwd":f.clean, "program":"exec sleep 120", "theme":calm_server::routes::theme::RequestTheme::default_dark()})).await;
    // Always release the blocking production seam before making the red assertion.
    pause.release.notify_one();
    tick.await.unwrap();
    f.b.shutdown_harnesses().await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "cross-owner admission during removal: {body}"
    );
    assert_eq!(body["code"], "workspace_busy");
    assert!(!f.clean.exists());
    let (status, body) = f.b.as_user("POST", &format!("/api/tracks/{}/terminal-cards", other["id"].as_str().unwrap()),
        json!({"cwd":f.unrecorded, "program":"true", "theme":calm_server::routes::theme::RequestTheme::default_dark()})).await;
    assert_eq!(status, StatusCode::CREATED, "reservation released: {body}");
}

#[tokio::test]
async fn gitforge_reclaim_recovers_removed_effect_after_receipt_failure() {
    let f = Fixture::new().await;
    let nested = f.kernel.join("nested-receipt-retry");
    let card: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=? AND role='planner'")
            .bind(&f.track)
            .fetch_one(f.b.repo.pool())
            .await
            .unwrap();
    gitforge_worktree_add_for_test(
        &f.b.state.operation_runtime,
        &f.b.tmp.path().join("logs"),
        &f.track,
        &card,
        &f.kernel,
        &nested,
        "receipt-retry",
    )
    .await
    .unwrap();
    f.close().await;
    sqlx::query("CREATE TRIGGER fail_reclaim_receipt BEFORE INSERT ON events WHEN NEW.kind='worktree.removed' BEGIN SELECT RAISE(FAIL,'receipt unavailable'); END")
        .execute(f.b.repo.pool()).await.unwrap();
    f.tick(&mut HashSet::new()).await;
    assert!(
        !nested.exists(),
        "physical effect completed before injected receipt failure"
    );
    sqlx::query("DROP TRIGGER fail_reclaim_receipt")
        .execute(f.b.repo.pool())
        .await
        .unwrap();
    let recovered = recover_reclaims(&f).await;
    f.tick(&mut HashSet::new()).await;
    recovered.sweep_parked().await.unwrap();
    let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='worktree.removed' AND json_extract(payload,'$.path')=?")
        .bind(nested.to_str().unwrap()).fetch_one(f.b.repo.pool()).await.unwrap();
    assert_eq!(
        receipts, 1,
        "settled success must replay one durable receipt after recovery"
    );
    assert_eq!(f.b.delete_track(&f.track).await.0, StatusCode::NO_CONTENT);
    assert_eq!(
        f.b.as_user(
            "DELETE",
            &format!("/api/areas/{}", f.b.area_id),
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn gitforge_reclaim_receipt_recovery_outlives_deleted_owner_rows() {
    let f = Fixture::new().await;
    sqlx::query("CREATE TRIGGER fail_reclaim_receipt BEFORE INSERT ON events WHEN NEW.kind='worktree.removed' BEGIN SELECT RAISE(FAIL,'receipt unavailable'); END")
        .execute(f.b.repo.pool()).await.unwrap();
    assert_eq!(f.b.delete_track(&f.track).await.0, StatusCode::NO_CONTENT);
    assert!(!f.clean.exists());
    assert!(f.b.repo.track_get(&f.track).await.unwrap().is_none());
    sqlx::query("DROP TRIGGER fail_reclaim_receipt")
        .execute(f.b.repo.pool())
        .await
        .unwrap();
    let recovered = recover_reclaims(&f).await;
    recovered.sweep_parked().await.unwrap();
    let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='worktree.removed' AND scope_track=? AND json_extract(payload,'$.path')=?")
        .bind(&f.track).bind(f.clean.to_str().unwrap()).fetch_one(f.b.repo.pool()).await.unwrap();
    assert_eq!(receipts, 1);
    assert!(f.dirty.join("untracked").exists());
    assert_eq!(
        f.b.as_user(
            "DELETE",
            &format!("/api/areas/{}", f.b.area_id),
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn gitforge_reclaim_old_success_does_not_cancel_new_provision() {
    use calm_server::{
        event::{Event, EventScope},
        ids::ActorId,
    };
    use calm_truth::db::sqlite::{
        append_decision_event_in_tx, begin_immediate_tx,
        worktree_events::unmatched_provisioned_worktrees,
    };
    let f = Fixture::new().await;
    f.close().await;
    sqlx::query("CREATE TRIGGER fail_reclaim_receipt BEFORE INSERT ON events WHEN NEW.kind='worktree.removed' BEGIN SELECT RAISE(FAIL,'receipt unavailable'); END")
        .execute(f.b.repo.pool()).await.unwrap();
    f.tick(&mut HashSet::new()).await;
    assert!(!f.clean.exists());
    sqlx::query("DROP TRIGGER fail_reclaim_receipt")
        .execute(f.b.repo.pool())
        .await
        .unwrap();
    run_git(
        &f.up.clone,
        [
            "worktree",
            "add",
            "-qb",
            "new-lifecycle",
            f.clean.to_str().unwrap(),
        ],
    );
    std::fs::write(f.clean.join("new-work"), "new lifecycle").unwrap();
    let card: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=? AND role='planner'")
            .bind(&f.track)
            .fetch_one(f.b.repo.pool())
            .await
            .unwrap();
    let mut tx = begin_immediate_tx(f.b.repo.db()).await.unwrap();
    let provision = append_decision_event_in_tx(
        &mut tx,
        &ActorId::KernelDispatcher,
        &EventScope::Card {
            card: card.clone().into(),
            track: f.track.clone().into(),
            area: f.b.area_id.clone().into(),
        },
        None,
        &Event::WorktreeProvisioned {
            track_id: f.track.clone().into(),
            card_id: card.into(),
            path: f.clean.to_str().unwrap().into(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // Only recover the already admitted old operations. No new close snapshot is taken.
    let recovered = recover_reclaims(&f).await;
    recovered.sweep_parked().await.unwrap();
    let claims = unmatched_provisioned_worktrees(&mut f.b.repo.pool().acquire().await.unwrap())
        .await
        .unwrap();
    assert!(claims.iter().any(|claim| claim.event_id == provision));
    assert_eq!(
        std::fs::read_to_string(f.clean.join("new-work")).unwrap(),
        "new lifecycle"
    );
    let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='worktree.removed' AND json_extract(payload,'$.path')=?")
        .bind(f.clean.to_str().unwrap()).fetch_one(f.b.repo.pool()).await.unwrap();
    assert_eq!(
        receipts, 0,
        "old success must not receipt the newer provision"
    );
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
async fn gitforge_reclaim_hidden_index_flags_close_and_delete() {
    for delete in [false, true] {
        let f = Fixture::new().await;
        std::fs::remove_file(f.dirty.join("untracked")).unwrap();
        let mut saved = Vec::new();
        for (path, flag) in [
            (&f.clean, "--assume-unchanged"),
            (&f.dirty, "--skip-worktree"),
        ] {
            let file = git_stdout(path, ["ls-files"])
                .lines()
                .next()
                .unwrap()
                .to_string();
            run_git(path, ["update-index", flag, &file]);
            std::fs::write(path.join(&file), "precious hidden work").unwrap();
            assert!(git_stdout(path, ["status", "--porcelain"]).is_empty());
            let index = git_stdout(path, ["rev-parse", "--git-path", "index"]);
            saved.push((
                path,
                file,
                index.clone(),
                std::fs::read(&index).unwrap(),
                git_stdout(path, ["ls-files", "-v"]),
            ));
        }
        if delete {
            assert_eq!(f.b.delete_track(&f.track).await.0, StatusCode::NO_CONTENT);
        } else {
            f.close().await;
            f.tick(&mut HashSet::new()).await;
        }
        assert_eq!(
            (
                f.clean_attempts.load(Ordering::SeqCst),
                f.dirty_attempts.load(Ordering::SeqCst)
            ),
            (0, 0)
        );
        for (path, file, index, bytes, flags) in saved {
            assert_eq!(
                std::fs::read_to_string(path.join(file)).unwrap(),
                "precious hidden work"
            );
            assert_eq!(git_stdout(path, ["ls-files", "-v"]), flags);
            assert_eq!(std::fs::read(index).unwrap(), bytes);
        }
        let receipts: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM events WHERE kind='worktree.removed' AND scope_track=?",
        )
        .bind(&f.track)
        .fetch_one(f.b.repo.pool())
        .await
        .unwrap();
        assert_eq!(receipts, 0);
    }
}
