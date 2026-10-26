//! Real ingress → Harness → provider → occupancy commit, against clean/delete.
use super::claude_planner_stack_fixture::{Root, Stack};
use calm_server::test_seams::{PausePoint, install_pause_for_test};
use serde_json::json;
use std::{collections::HashSet, sync::Arc, time::Duration};

struct Pause(PausePoint, String);
impl Pause {
    fn arm(point: &str, key: &str) -> Self {
        let pause = PausePoint::default();
        install_pause_for_test(point, key, pause.clone());
        Self(pause, format!("{point}/{key}"))
    }
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(20), self.0.entered.notified())
            .await
            .unwrap_or_else(|_| panic!("race point not entered: {}", self.1));
    }
}
impl Drop for Pause {
    fn drop(&mut self) {
        self.0.release.notify_one();
    }
}
#[derive(Clone, Copy)]
enum Entry {
    Resume,
    Retry,
    Historical,
    Canonical,
    HistoricalCompact,
    HistoricalPlannerCompact,
    CanonicalCompact,
    CanonicalPlannerCompact,
}
impl Entry {
    fn canonical(self) -> bool {
        matches!(
            self,
            Self::Canonical
                | Self::Resume
                | Self::Retry
                | Self::CanonicalCompact
                | Self::CanonicalPlannerCompact
        )
    }
    fn compact(self) -> bool {
        !matches!(
            self,
            Self::Historical | Self::Canonical | Self::Resume | Self::Retry
        )
    }
}
async fn fixture(
    provider: &str,
    entry: Entry,
    prime: bool,
) -> (Root, Arc<Stack>, String, String, std::path::PathBuf) {
    let root = Root::new("exit");
    let mut config = root.config(true);
    if provider == "opencode" {
        let path = root.path().join("acp.json");
        std::fs::write(&path, json!({"agents":[{
            "provider":"opencode", "command":"/usr/bin/python3",
            "args":["-u", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/acp_planner/agent.py")],
            "env":{"ACP_FIXTURE_ROOT":root.path()}, "min_agent_version":"1.0.0"
        }]}).to_string()).unwrap();
        std::fs::write(root.path().join("scenario"), "reply").unwrap();
        config.acp_planner_config = Some(path);
        config.mcp_stdio_shim_bin = Some(crate::support::mcp::locate_shim_bin());
    }
    config.auth_username = Some("fixture-owner".into());
    config.auth_password = Some("fixture-password".into());
    config.auth_dev_autologin = false;
    let (state, auth) = calm_server::state::AppState::boot(&config)
        .await
        .unwrap()
        .into_parts();
    let app =
        crate::support::authenticated_router::application_with_auth(state.clone(), auth).await;
    let stack = Arc::new(Stack { state, app });
    let origin = root.path().join("origin.git");
    let seed = root.path().join("seed");
    let checkout = root.path().join("checkout");
    crate::support::git_helpers::init_bare_origin(&origin, &seed);
    crate::support::git_helpers::clone_for_track(&origin, &checkout);
    let (track, card) = stack.create_claude_track_with(json!({
        "attach_folder": !entry.canonical(), "planner_provider":provider,
        "model": if provider == "claude" { json!("claude-fable-5-1[1m]") } else { serde_json::Value::Null },
        "reasoning_effort": if provider == "claude" { json!("high") } else { serde_json::Value::Null },
        "template_id": if entry.canonical() { serde_json::Value::Null } else { json!("small-change") }
    })).await;
    // Seed the location through its owner before activation: canonical creation
    // context intentionally prevents using the relocation API as fixture setup.
    let mut workspace = stack
        .repo()
        .track_get(&track)
        .await
        .unwrap()
        .unwrap()
        .workspace;
    workspace.kind = calm_server::model::TrackWorkspaceKind::Attached;
    workspace.path = checkout.to_str().unwrap().to_owned();
    workspace.worktree = Some(
        workspace::worktree::track_worktree_path_for(&checkout, &track)
            .to_str()
            .unwrap()
            .to_owned(),
    );
    let pool = stack.repo().sqlite_db().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    calm_truth::db::sqlite::track_workspace_write_tx(&mut tx, &track, &workspace)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    calm_server::test_seams::ensure_track_worktree_for_test(&pool, &track)
        .await
        .unwrap();
    if !entry.canonical() {
        let (status, body) = stack
            .send("POST", &format!("/api/cards/{card}/planner/restart"), None)
            .await;
        assert!(
            status.is_success(),
            "restart after fixture location setup: {body}"
        );
    }
    // Clean cases start from a cleanable idle carrier. First-commit deletion
    // also exercises dormant activation with no previous Engine history.
    if prime {
        let (status, body) = stack
            .post_input(&card, "establish native conversation")
            .await;
        assert!(status.is_success(), "{body}");
        stack.wait_outcomes(&card, 1).await;
        wait_phase(&stack, &card, "turn_completed").await;
    }
    if matches!(entry, Entry::Resume) {
        let pool = stack.repo().sqlite_db().unwrap();
        sqlx::query(concat!(
            "CREATE TRIGGER c5d_fail_marker BEFORE UPDATE OF handle_state_json ON worker_sessions ",
            "WHEN NEW.handle_state_json LIKE '%crossing_unknown%' ",
            "BEGIN SELECT RAISE(ABORT,'marker failed'); END"
        ))
        .execute(pool.writer_pool())
        .await
        .unwrap();
        let (status, body) = stack.post_input(&card, "resume frozen claim").await;
        assert!(status.is_success(), "{body}");
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if stack
                    .runtime(&card)
                    .await
                    .handle_state_json
                    .as_ref()
                    .is_some_and(|f| f["canonical_inputs"]["active"].is_object())
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // A recovered unattempted frame can have an idle session projection.
        // Keep the Planner proxy so PR1 exercises the actual admission fence.
        sqlx::query("UPDATE worker_sessions SET state='idle' WHERE id=?")
            .bind(stack.runtime(&card).await.id)
            .execute(pool.writer_pool())
            .await
            .unwrap();
    }
    let frame = stack.runtime(&card).await.handle_state_json.unwrap();
    assert_eq!(
        frame.get("canonical_inputs").is_some(),
        entry.canonical(),
        "creation storage mode"
    );
    let track_row = stack.repo().track_get(&track).await.unwrap().unwrap();
    assert_eq!(
        track_row.workspace.isolation(),
        calm_types::model::WorkspaceIsolation::Worktree
    );
    let cwd = std::path::PathBuf::from(track_row.workspace.agent_cwd());
    std::fs::write(checkout.join(".git/info/exclude"), "target/\n").unwrap();
    std::fs::create_dir_all(cwd.join("target")).unwrap();
    std::fs::write(cwd.join("target/ignored"), "build output").unwrap();
    (root, stack, track, card, cwd)
}

async fn wait_phase(stack: &Stack, card: &str, phase: &str) {
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            if stack
                .runtime(card)
                .await
                .handle_state_json
                .as_ref()
                .is_some_and(|frame| frame["phase"] == phase)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("persisted phase");
}

async fn planner_request(stack: &Stack, card: &str) {
    let track_id = stack.repo().card_get(card).await.unwrap().unwrap().track_id;
    let (track, report_card, report) =
        calm_server::track_report::resolve_report_for_track(stack.repo(), track_id.as_str())
            .await
            .unwrap();
    let body = format!(
        "{}\n{}",
        report.body,
        calm_types::report_blocks::render_fence(
            "task",
            &json!({
                "key":"compact", "kind":"compact", "context":"Continue from verified task evidence.", "ready":true, "declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR
            })
        )
    );
    calm_server::track_report::persist_report(
        stack.repo(),
        &stack.state.events,
        stack.state.write(),
        calm_server::ids::ActorId::KernelDispatcher,
        calm_server::event::EditAuthor::Planner,
        track,
        report_card,
        report.clone(),
        calm_server::track_report::TrackReportPayload::new(report.summary, body),
        report.doc_rev,
        None,
    )
    .await
    .unwrap();
    stack
        .state
        .scheduler_for_test()
        .schedule_track(track_id)
        .await;
}

async fn race(provider: &str, entry: Entry, delete: bool) {
    race_with_history(provider, entry, delete, true).await;
}
async fn race_with_history(provider: &str, entry: Entry, delete: bool, prime: bool) {
    let (root, stack, track, card, cwd) = fixture(provider, entry, prime).await;
    let worker = stack.runtime(&card).await.id;
    let pool = stack.repo().sqlite_db().unwrap();
    let before = Pause::arm("c5d-before-occupancy-commit", &worker);
    let release = Pause::arm("c4-release-authorized", &worker);
    let held = Pause::arm(
        if matches!(entry, Entry::Resume) {
            "c5d-resume-admission-ready"
        } else {
            "c5d-admission-track-held"
        },
        &worker,
    );
    if entry.compact() {
        root.set_scenario("compact");
        std::fs::write(root.path().join("scenario"), "compact-success").unwrap();
    } else {
        root.set_scenario("hold");
        std::fs::write(root.path().join("scenario"), "hold").unwrap();
    }
    let launch = tokio::spawn({
        let stack = stack.clone();
        let card = card.clone();
        async move {
            if matches!(entry, Entry::Resume) {
                // The existing source resumes once the held fence is released.
            } else if matches!(entry, Entry::Retry) {
                let row = stack.runtime(&card).await;
                let frame = row.handle_state_json.unwrap();
                let attempt = frame["canonical_inputs"]["attempts"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap();
                let (status, body) = stack.send_with(axum::http::Request::builder().method("POST")
                    .uri(format!("/api/cards/{card}/conversation/input/retry"))
                    .header("Idempotency-Key", "c5d-retry"), Some(json!({
                        "carrier_id":row.id, "submission_id":attempt["attempt"]["submission_id"],
                        "attempt_id":attempt["attempt"]["attempt_id"], "revision":attempt["revision"].as_str().map(str::to_owned).unwrap_or_else(|| attempt["revision"].to_string())
                    }))).await;
                assert!(status.is_success(), "{body}");
            } else if matches!(
                entry,
                Entry::HistoricalPlannerCompact | Entry::CanonicalPlannerCompact
            ) {
                planner_request(&stack, &card).await;
            } else {
                let (status, body) = if entry.compact() {
                    stack
                        .send(
                            "POST",
                            &format!("/api/cards/{card}/planner/compact"),
                            Some(json!({})),
                        )
                        .await
                } else {
                    stack.post_input(&card, "launch behind the fence").await
                };
                if !delete {
                    assert!(status.is_success(), "{body}");
                }
            }
        }
    });
    held.entered().await;
    sqlx::query("UPDATE tracks SET closed_at=123 WHERE id=?")
        .bind(&track)
        .execute(pool.writer_pool())
        .await
        .unwrap();
    let clean_ready = (!delete).then(|| Pause::arm("c5d-clean-before-track", &track));
    let cleaning = if !delete {
        let stack = stack.clone();
        Some(tokio::spawn(async move {
            let mut history = HashSet::new();
            calm_server::test_seams::clean_closed_worktrees_with_history_for_test(
                &stack.repo().sqlite_db().unwrap(),
                &stack.state.operation_runtime,
                &mut history,
            )
            .await
            .unwrap()
        }))
    } else {
        None
    };
    if let Some(pause) = &clean_ready {
        pause.entered().await;
    }
    if matches!(entry, Entry::Resume) {
        sqlx::query("DROP TRIGGER c5d_fail_marker")
            .execute(pool.writer_pool())
            .await
            .unwrap();
    }
    drop(held);
    if tokio::time::timeout(Duration::from_secs(20), before.0.entered.notified())
        .await
        .is_err()
    {
        let tasks: Vec<(String, String)> =
            sqlx::query_as("SELECT id,status FROM tasks WHERE track_id=?")
                .bind(&track)
                .fetch_all(pool.read_pool())
                .await
                .unwrap();
        panic!(
            "admission not reached; frame={:?}; tasks={tasks:?}",
            stack.runtime(&card).await.handle_state_json
        );
    }
    assert!(
        calm_server::per_card_lock::try_lock_key(
            stack.state.operation_runtime.track_locks_for_test(),
            &track
        )
        .is_none(),
        "issuer released Track before occupancy commit"
    );
    let arrival = Pause::arm("key-lock-waiting", &track);
    let acquired = if delete {
        let point = PausePoint::default();
        calm_server::routes::tracks::install_track_delete_teardown_hook_for_test(
            &track,
            calm_server::routes::tracks::TrackDeleteTeardownHook {
                entered: point.entered.clone(),
                release: point.release.clone(),
            },
        );
        Pause(point, "delete acquired Track".into())
    } else {
        Pause::arm("c1-clean-fenced", &track)
    };
    drop(clean_ready);
    let deleting = delete.then(|| {
        tokio::spawn({
            let stack = stack.clone();
            let track = track.clone();
            async move {
                stack
                    .send("DELETE", &format!("/api/tracks/{track}"), None)
                    .await
            }
        })
    });
    tokio::select! {
        entered = tokio::time::timeout(Duration::from_secs(20), arrival.0.entered.notified()) => entered.expect("lifecycle must wait on Track"),
        _ = acquired.0.entered.notified() => panic!("lifecycle acquired Track before occupancy commit"),
    }
    // A try-lock observes the same registry used by clean/delete, not a second lock.
    assert!(
        calm_server::per_card_lock::try_lock_key(
            stack.state.operation_runtime.track_locks_for_test(),
            &track
        )
        .is_none(),
        "Track released before occupancy commit"
    );
    drop(arrival);
    drop(acquired);
    drop(before);
    if let Some(cleaning) = cleaning {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), cleaning)
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert!(cwd.join("target/ignored").exists());
        let unsettled: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM workspace_executions WHERE track_id=? AND settled_at_ms IS NULL",
        )
        .bind(&track)
        .fetch_one(pool.read_pool())
        .await
        .unwrap();
        assert_eq!(unsettled, 1, "clean deferred across committed occupancy");
    }
    drop(release);
    if let Some(deleting) = deleting {
        let (status, body) = tokio::time::timeout(Duration::from_secs(30), deleting)
            .await
            .expect("delete must not hang")
            .unwrap();
        assert!(status.is_success(), "{body}");
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM workspace_executions WHERE track_id=? AND settled_at_ms IS NULL",
        )
        .bind(&track)
        .fetch_one(pool.read_pool())
        .await
        .unwrap();
        assert_eq!(live, 0, "delete stops the admitted execution");
    }
    tokio::time::timeout(Duration::from_secs(30), launch)
        .await
        .unwrap()
        .unwrap();
    // Shutdown through the stack's existing owner after all request futures finish.
    let stack = Arc::try_unwrap(stack)
        .ok()
        .expect("all request owners finished");
    stack.shutdown().await;
}
macro_rules! cases {
    ($($clean:ident, $delete:ident: $provider:literal, $entry:ident;)*) => {$ (
        #[tokio::test(flavor="multi_thread", worker_threads=4)]
        async fn $clean() { race($provider, Entry::$entry, false).await; }
        #[tokio::test(flavor="multi_thread", worker_threads=4)]
        async fn $delete() { race($provider, Entry::$entry, true).await; }
    )*};
}
cases! {
    c5d_claude_resume_clean, c5d_claude_resume_delete: "claude", Resume;
    c5d_acp_resume_clean, c5d_acp_resume_delete: "opencode", Resume;
    c5d_claude_retry_clean, c5d_claude_retry_delete: "claude", Retry;
    c5d_claude_historical_clean, c5d_claude_historical_delete: "claude", Historical;
    c5d_claude_canonical_clean, c5d_claude_canonical_delete: "claude", Canonical;
    c5d_acp_historical_clean, c5d_acp_historical_delete: "opencode", Historical;
    c5d_acp_canonical_clean, c5d_acp_canonical_delete: "opencode", Canonical;
    c5d_claude_historical_compact_clean, c5d_claude_historical_compact_delete: "claude", HistoricalCompact;
    c5d_claude_historical_planner_compact_clean, c5d_claude_historical_planner_compact_delete: "claude", HistoricalPlannerCompact;
    c5d_claude_canonical_compact_clean, c5d_claude_canonical_compact_delete: "claude", CanonicalCompact;
    c5d_claude_canonical_planner_compact_clean, c5d_claude_canonical_planner_compact_delete: "claude", CanonicalPlannerCompact;
    c5d_acp_canonical_compact_clean, c5d_acp_canonical_compact_delete: "opencode", CanonicalCompact;
    c5d_acp_canonical_planner_compact_clean, c5d_acp_canonical_planner_compact_delete: "opencode", CanonicalPlannerCompact;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c5d_claude_canonical_first_commit_delete() {
    race_with_history("claude", Entry::Canonical, true, false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c5d_acp_canonical_first_commit_delete() {
    race_with_history("opencode", Entry::Canonical, true, false).await;
}
