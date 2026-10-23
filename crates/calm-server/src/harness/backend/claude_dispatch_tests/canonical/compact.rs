//! Compaction uses the live loop and production Claude stdin writer.
use super::*;
use crate::claude_planner::session::SettlePause;
use crate::harness::PlannerHarness;

async fn ready() -> (Fixture, PlannerHarness, serde_json::Value) {
    let fx = Fixture::new("exit").await;
    let harness = fx.start_loop().await;
    // #2982: turn_completed can precede the exit fixture's sole usage event.
    // Wait for that persisted reading so whole-frame comparisons below cannot
    // race unrelated telemetry, including after a failed compact guard.
    let before = tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let frame = fx.frame().await;
            if frame["phase"] == "turn_completed" && !frame["token_usage"].is_null() {
                return frame;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("completed exit fixture with persisted usage");
    std::fs::write(fx.dir.path().join("scenario"), "compact").unwrap();
    (fx, harness, before)
}
fn pause() -> SettlePause {
    SettlePause {
        entered: Arc::default(),
        release: Arc::default(),
    }
}
fn lines(fx: &Fixture) -> usize {
    std::fs::read_to_string(fx.dir.path().join("stdin"))
        .unwrap_or_default()
        .lines()
        .count()
}
async fn entered(pause: &SettlePause) {
    tokio::time::timeout(Duration::from_secs(15), pause.entered.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn canonical_claude_compact_uses_current_human_and_commits_before_line() {
    let (fx, harness, before) = ready().await;
    let blocked = pause();
    fx.session.hold_user_writer_for_test(blocked.clone());
    let compact = tokio::spawn({
        let harness = harness.clone();
        let source = fx.source.clone();
        async move { harness.compact_canonical(source).await }
    });
    entered(&blocked).await;
    let guard = fx.frame().await;
    assert!(
        matches!(
            harness.state_for_test().await,
            crate::harness::HarnessState::Compacting { .. }
        ),
        "live compact must stay busy, not project restart recovery"
    );
    assert_eq!(guard["phase"], "compacting");
    assert_eq!(guard["compact_outcome"]["status"], "started");
    assert_eq!(guard["last_turn_id"], before["last_turn_id"]);
    assert_eq!(
        lines(&fx),
        1,
        "the real writer has emitted no maintenance bytes"
    );
    blocked.release.notify_one();
    compact.await.unwrap().unwrap();
    let after = fx.phase("turn_completed").await;
    assert_eq!(after["compact_outcome"]["status"], "completed");
    assert_eq!(after["last_turn_id"], before["last_turn_id"]);
    assert_eq!(after["canonical_inputs"], before["canonical_inputs"]);
    assert_eq!(after["token_usage"], before["token_usage"]);
    assert_eq!(lines(&fx), 2);
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_presentations WHERE card_id=?1")
            .bind(&fx.owner.card)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert_eq!(rows, 2, "one human presentation and its native reply");
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_compact_guard_failure_writes_nothing() {
    let (fx, harness, before) = ready().await;
    sqlx::query(
        "CREATE TRIGGER fail_compact_guard
         BEFORE UPDATE OF handle_state_json ON worker_sessions
         WHEN json_extract(NEW.handle_state_json,'$.compact_outcome.status')='started'
         BEGIN SELECT RAISE(ABORT,'guard failed'); END",
    )
    .execute(fx.owner.repo.pool())
    .await
    .unwrap();
    let error = harness
        .compact_canonical(fx.source.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("guard failed"), "{error}");
    assert_eq!(fx.frame().await, before);
    assert_eq!(
        fx.session.user_line_writes_for_test(),
        1,
        "failed guard never entered the stdin writer"
    );
    assert_eq!(lines(&fx), 1);
    assert_eq!(
        std::fs::read_to_string(fx.dir.path().join("spawns"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_compact_rechecks_after_writer_wait() {
    for change in [
        "logout",
        "resource",
        "archive",
        "frame",
        "pending",
        "thread",
        "binding",
        "membership",
    ] {
        let (fx, harness, before) = ready().await;
        let blocked = pause();
        fx.session.hold_user_writer_for_test(blocked.clone());
        let compact = tokio::spawn({
            let harness = harness.clone();
            let source = fx.source.clone();
            async move { harness.compact_canonical(source).await }
        });
        entered(&blocked).await;
        match change {
            "thread" => {
                sqlx::query("UPDATE worker_sessions SET thread_id=?1 WHERE id=?2")
                    .bind(uuid::Uuid::new_v4().to_string())
                    .bind(&fx.owner.worker)
                    .execute(fx.owner.repo.pool())
                    .await
                    .unwrap();
            }
            "binding" => {
                sqlx::query("UPDATE cards SET payload=json_set(payload,'$.planner_provider','codex') WHERE id=?1")
                    .bind(&fx.owner.card).execute(fx.owner.repo.pool()).await.unwrap();
            }
            "membership" => {
                let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
                let area = conversation_area_tx(&mut tx, &fx.owner.card).await.unwrap();
                sqlx::query(
                    "UPDATE area_members SET role='member' WHERE area_id=?1 AND user_id=?2",
                )
                .bind(area.id.as_str())
                .bind(calm_types::organization::LEGACY_OWNER_USER_ID)
                .execute(&mut **tx)
                .await
                .unwrap();
                tx.commit().await.unwrap();
            }
            "logout" => {
                fx.auth.sessions.remove(&fx.login).unwrap();
            }
            "resource" => {
                sqlx::query("UPDATE areas SET conversation_history_contract='shared_history'")
                    .execute(fx.owner.repo.pool())
                    .await
                    .unwrap();
            }
            "archive" => {
                sqlx::query("INSERT INTO assistant_conversation_archives(card_id,archived_at_ms) VALUES(?1,1)")
                    .bind(&fx.owner.card)
                    .execute(fx.owner.repo.pool())
                    .await
                    .unwrap();
            }
            "frame" => {
                let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
                let carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
                    .await
                    .unwrap();
                let mut header = RuntimeExecutionFence::from_carrier(&carrier)
                    .header()
                    .clone();
                header["compact_outcome"]["operation_id"] = serde_json::json!("new-operation");
                persist_runtime_frame_tx(
                    &mut tx,
                    &fx.owner.card,
                    &fx.owner.worker,
                    &RuntimeExecutionFence::from_carrier(&carrier),
                    header,
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
            }
            "pending" => {
                let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
                let mut carrier =
                    current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
                        .await
                        .unwrap();
                crate::conversation_inputs::accept_human_tx(
                    &mut tx,
                    &fx.source.current_principal().unwrap(),
                    &mut carrier,
                    HumanPayload {
                        text: "later human".into(),
                        bound_attachments: vec![],
                    },
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
            }
            other => panic!("unexpected change {other}"),
        }
        blocked.release.notify_one();
        assert!(compact.await.unwrap().is_err(), "{change}");
        assert_eq!(lines(&fx), 1, "{change}");
        let after = fx.frame().await;
        assert_eq!(after["last_turn_id"], before["last_turn_id"]);
        harness.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_claude_compact_before_write_restores_and_crossed_holds() {
    for crossed in [false, true] {
        let (fx, harness, before) = ready().await;
        if crossed {
            let blocked = pause();
            fx.session.fail_user_flush_for_test(blocked.clone());
            let compact = tokio::spawn({
                let harness = harness.clone();
                let source = fx.source.clone();
                async move { harness.compact_canonical(source).await }
            });
            entered(&blocked).await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while lines(&fx) != 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            blocked.release.notify_one();
            assert!(compact.await.unwrap().is_err());
            let after = fx.phase("wedged").await;
            assert_eq!(after["compact_outcome"]["status"], "unconfirmed");
            assert!(harness.compact_canonical(fx.source.clone()).await.is_err());
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert_eq!(lines(&fx), 2);
        } else {
            // Refuse during readiness, before the Engine wrapper can accept
            // stdin. Vanishing after readiness races the wrapper's first write
            // and can legitimately produce a submitted, failed compaction.
            std::fs::remove_file(fx.dir.path().join("claude")).unwrap();
            assert!(harness.compact_canonical(fx.source.clone()).await.is_err());
            let after = fx.frame().await;
            assert_eq!(after["phase"], before["phase"]);
            assert_eq!(after["compact_outcome"]["status"], "refused");
            assert_eq!(after["canonical_inputs"], before["canonical_inputs"]);
            assert_eq!(lines(&fx), 1);
        }
        harness.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_claude_compact_requires_boundary_and_exact_cleanup() {
    for scenario in ["compact", "compact-no-boundary", "compact-boundary-failed"] {
        let (fx, harness, before) = ready().await;
        std::fs::write(fx.dir.path().join("scenario"), scenario).unwrap();
        let blocked = pause();
        fx.session
            .set_before_turn_completed_pause_for_test(blocked.clone());
        harness.compact_canonical(fx.source.clone()).await.unwrap();
        entered(&blocked).await;
        assert_eq!(
            fx.frame().await["phase"],
            "compacting",
            "LocalWrite is submission only"
        );
        blocked.release.notify_one();
        let expected = if scenario == "compact" {
            "turn_completed"
        } else {
            "wedged"
        };
        let after = fx.phase(expected).await;
        assert_eq!(
            after["context_usage_invalidated"],
            scenario != "compact-no-boundary"
        );
        assert_eq!(after["canonical_inputs"], before["canonical_inputs"]);
        assert_eq!(after["last_turn_id"], before["last_turn_id"]);
        harness.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_claude_compact_blocks_pending_and_held_input() {
    let fx = Fixture::new("stream").await;
    std::fs::write(fx.dir.path().join("stream"), "").unwrap();
    std::fs::write(fx.dir.path().join("stream-rest"), "").unwrap();
    let harness = fx.start_loop().await;
    fx.phase("turn_running").await;
    let before = fx.checkpoint().await;
    assert!(harness.compact_canonical(fx.source.clone()).await.is_err());
    assert_eq!(fx.checkpoint().await, before);
    tokio::time::timeout(Duration::from_secs(5), async {
        while lines(&fx) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_compact_restart_never_reissues() {
    use crate::harness::{HarnessRuntimeSnapshot, PlannerHarnessParams};
    for window in ["guard", "written", "bound"] {
        let fx = Fixture::persistent("exit").await;
        let harness = fx.start_loop().await;
        let before = fx.phase("turn_completed").await;
        if window != "guard" {
            std::fs::write(fx.dir.path().join("scenario"), "stream").unwrap();
            std::fs::write(fx.dir.path().join("stream"), "").unwrap();
            std::fs::write(fx.dir.path().join("stream-rest"), "").unwrap();
            harness.compact_canonical(fx.source.clone()).await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while lines(&fx) != 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        harness.shutdown().await.unwrap();
        // Crash-window seed uses the real protected writer, not a maintenance reducer copy.
        let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
        let carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
            .await
            .unwrap();
        let fence = RuntimeExecutionFence::from_carrier(&carrier);
        let mut header = fence.header().clone();
        header["phase"] = serde_json::json!("compacting");
        if window == "guard" {
            header["compact_outcome"] = serde_json::json!({"operation_id":crate::model::new_id(),"thread_id":fx.owner.thread,"turn_id":null,"status":"started"});
        } else if window == "written" {
            header["compact_outcome"]["turn_id"] = serde_json::Value::Null;
        }
        persist_runtime_frame_tx(&mut tx, &fx.owner.card, &fx.owner.worker, &fence, header)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open(&fx.owner.db_url)
                .await
                .unwrap(),
        );
        let roles = crate::card_role_cache::CardRoleCache::new();
        roles.seed_from_db(repo.pool()).await.unwrap();
        let areas = crate::track_area_cache::TrackAreaCache::new();
        areas.seed_from_db(repo.pool()).await.unwrap();
        let rebooted = PlannerHarness::run(PlannerHarnessParams {
            track_delete_locks: Default::default(),
            worker_session_id: fx.owner.worker.clone(),
            track_id: crate::ids::TrackId(fx.owner.track.clone()),
            card_id: crate::ids::CardId(fx.owner.card.clone()),
            thread_id: Some(fx.owner.thread.clone()),
            repo,
            events: crate::event::EventBus::new(),
            card_role_cache: roles,
            track_area_cache: areas,
            backend: fx.backend.clone(),
            registry: Default::default(),
            config: Default::default(),
            snapshot: HarnessRuntimeSnapshot::decode(fx.frame().await).unwrap(),
        })
        .await
        .unwrap();
        let after = fx.phase("wedged").await;
        assert_eq!(after["compact_outcome"]["status"], "unconfirmed");
        assert_eq!(after["canonical_inputs"], before["canonical_inputs"]);
        assert_eq!(after["last_turn_id"], before["last_turn_id"]);
        assert!(rebooted.compact_canonical(fx.source.clone()).await.is_err());
        assert_eq!(lines(&fx), if window == "guard" { 1 } else { 2 });
        rebooted.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_claude_compact_cancelled_caller_keeps_one_owned_submission() {
    let (fx, harness, before) = ready().await;
    let blocked = pause();
    fx.session.hold_dispatch_result_for_test(blocked.clone());
    let caller = tokio::spawn({
        let harness = harness.clone();
        let source = fx.source.clone();
        async move { harness.compact_canonical(source).await }
    });
    entered(&blocked).await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert_eq!(fx.frame().await["compact_outcome"]["status"], "started");
    blocked.release.notify_one();
    let after = fx.phase("turn_completed").await;
    assert_eq!(after["compact_outcome"]["status"], "completed");
    assert_eq!(after["canonical_inputs"], before["canonical_inputs"]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lines(&fx), 2);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_compact_bad_session_and_failed_cleanup_cannot_complete() {
    for failure in ["session", "instructions", "stop", "turn"] {
        let (fx, harness, before) = ready().await;
        let blocked = pause();
        let broken_instruction = Arc::new(std::sync::Mutex::new(None::<std::path::PathBuf>));
        match failure {
            "turn" => fx
                .session
                .set_before_turn_completed_pause_for_test(blocked.clone()),
            "session" => {
                std::fs::write(fx.dir.path().join("scenario"), "wrong-init-session").unwrap();
            }
            "instructions" => {
                let instructions = fx.session.host().instructions_dir.clone();
                let broken = broken_instruction.clone();
                fx.session.set_after_spawn_hook_for_test(Arc::new(move || {
                    for file in std::fs::read_dir(&instructions).unwrap() {
                        let path = file.unwrap().path();
                        if !path.is_file() {
                            continue;
                        }
                        std::fs::remove_file(&path).unwrap();
                        std::fs::create_dir(&path).unwrap();
                        *broken.lock().unwrap() = Some(path);
                    }
                }));
            }
            "stop" => {
                std::fs::write(fx.dir.path().join("scenario"), "compact-linger").unwrap();
                crate::test_support::execution::block_stops(fx.owner.repo.db()).await;
            }
            other => panic!("unexpected failure {other}"),
        }
        harness.compact_canonical(fx.source.clone()).await.unwrap();
        if failure == "turn" {
            entered(&blocked).await;
            let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
            let carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
                .await
                .unwrap();
            let fence = RuntimeExecutionFence::from_carrier(&carrier);
            let mut header = fence.header().clone();
            header["compact_outcome"]["turn_id"] = serde_json::json!("foreign-turn");
            persist_runtime_frame_tx(&mut tx, &fx.owner.card, &fx.owner.worker, &fence, header)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            blocked.release.notify_one();
        }
        let after = fx.phase("wedged").await;
        assert_eq!(after["compact_outcome"]["status"], "unconfirmed");
        assert_eq!(after["canonical_inputs"], before["canonical_inputs"]);
        assert_eq!(after["last_turn_id"], before["last_turn_id"]);
        if failure == "stop" {
            crate::test_support::execution::unblock_stops(fx.owner.repo.db()).await;
        }
        if let Some(path) = broken_instruction.lock().unwrap().take() {
            std::fs::remove_dir(path).unwrap();
        }
        harness.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_claude_compact_rejects_other_session_owner() {
    use provider::control::CompactSubmission;
    let (fx, harness, before) = ready().await;
    let result = fx
        .session
        .compact_canonical(
            MaintenanceTarget {
                card_id: "another-card".into(),
                track_id: fx.owner.track.clone(),
                worker_session_id: fx.owner.worker.clone(),
                thread_id: fx.owner.thread.clone(),
                operation_id: crate::model::new_id(),
            },
            &TurnModelSelection::inherit(),
            &crate::harness::native_admission::NativePromptAdmission::new(
                || Ok(()),
                || Box::pin(async { Ok(()) }),
            ),
        )
        .await;
    assert!(matches!(result, CompactSubmission::NotSubmitted { .. }));
    assert_eq!(lines(&fx), 1);
    assert_eq!(fx.frame().await, before);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_compact_start_without_end_expires() {
    let (fx, harness, before) = ready().await;
    std::fs::write(fx.dir.path().join("scenario"), "compact-hang").unwrap();
    harness.compact_canonical(fx.source.clone()).await.unwrap();
    let turn = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let crate::harness::HarnessState::CompactionRunning { turn_id, .. } =
                harness.state_for_test().await
            {
                break turn_id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &fx.source.current_principal().unwrap(),
        &mut carrier,
        HumanPayload {
            text: "later input".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let pending = fx.frame().await;
    harness
        .set_state_for_test(crate::harness::HarnessState::CompactionRunning {
            turn_id: turn,
            started_at: std::time::Instant::now()
                - crate::harness::HarnessConfig::default().max_turn_duration,
        })
        .await;
    let after = tokio::time::timeout(Duration::from_secs(5), fx.phase("wedged"))
        .await
        .unwrap();
    assert_eq!(after["compact_outcome"]["status"], "unconfirmed");
    assert_eq!(after["last_turn_id"], before["last_turn_id"]);
    assert_eq!(after["canonical_inputs"], pending["canonical_inputs"]);
    assert!(harness.compact_canonical(fx.source.clone()).await.is_err());
    assert_eq!(fx.session.user_line_writes_for_test(), 2);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn c4_canonical_lost_compaction_reports_failure_before_releasing_driver_slot() {
    let (fx, harness, before) = ready().await;
    let pause = pause();
    fx.session
        .set_before_turn_completed_pause_for_test(pause.clone());
    harness.compact_canonical(fx.source.clone()).await.unwrap();
    entered(&pause).await;
    fx.session.abort_driver_for_test().await;
    let owner = &fx.session.host().provider_execution;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            owner
                .sweep(fx.owner.repo.db(), fx.session.host().execution.engine())
                .await
                .unwrap();
            if calm_truth::db::sqlite::provider_execution::load(
                fx.owner.repo.pool(),
                &fx.owner.worker,
            )
            .await
            .unwrap()
            .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let after = fx.phase("wedged").await;
    assert_eq!(after["compact_outcome"]["status"], "unconfirmed");
    assert_eq!(after["last_turn_id"], before["last_turn_id"]);
    assert_eq!(after["canonical_inputs"], before["canonical_inputs"]);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_planner_compact_claude_positive() {
    use calm_truth::db::RepoEventWrite;
    use calm_truth::db::RepoRead;
    let (fx, harness, before) = ready().await;
    fx.auth.sessions.remove(&fx.login).unwrap();
    let daemon =
        crate::shared_codex_appserver::SharedCodexAppServer::new_stub(fx.owner.repo.clone());
    let (scheduler, _) = crate::harness::run_loop::compact_task_tests::declared_for(
        fx.owner.repo.clone(),
        &harness,
        daemon,
    )
    .await;
    let id = format!("{}:compact", fx.owner.track);
    assert_ne!(
        fx.owner.repo.task_get(&id).await.unwrap().unwrap().status,
        crate::model::TaskStatus::Failed
    );
    let after = tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let frame = fx.frame().await;
            if frame["compact_outcome"]["status"] == "completed" {
                break frame;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(after["compact_outcome"]["operation_id"], id);
    assert_eq!(after["canonical_inputs"], before["canonical_inputs"]);
    assert_eq!(after["last_turn_id"], before["last_turn_id"]);
    assert_eq!(lines(&fx), 2, "one original input and one maintenance line");
    assert_eq!(
        fx.owner.repo.task_get(&id).await.unwrap().unwrap().status,
        crate::model::TaskStatus::Done
    );
    scheduler.sweep_all().await;
    let events = fx.owner.repo.events_since(0, 1000).await.unwrap();
    let wakes: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let crate::event::Event::TrackWakeRequested { text, .. } = &event.3 {
                Some(text)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(wakes.len(), 1);
    assert!(wakes[0].ends_with(crate::harness::run_loop::compact_task_tests::HANDOFF));
    harness.shutdown().await.unwrap();
}
